import { execFile } from "node:child_process";
import { lstat, mkdir, mkdtemp, readFile, realpath, rename, rm, rmdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseEnv, promisify } from "node:util";
import { githubClient, waitForDeployment, type GitHubRequest } from "./github.mts";

const execute = promisify(execFile);
const askpass = fileURLToPath(new URL("./git-askpass.sh", import.meta.url));

export interface Config {
  state: string;
  repository: string;
  mode: "prepare" | "publish";
  bootstrap: boolean;
  allowShrink: boolean;
  gitName: string;
  gitEmail: string;
  mailto: string;
  githubToken: string;
  openalexKey: string;
}

interface CommandOptions {
  cwd: string;
  env: NodeJS.ProcessEnv;
}
export type Run = (command: string, args: string[], options: CommandOptions) => Promise<string>;
export interface Dependencies {
  run?: Run;
  request?: GitHubRequest;
  wait?: typeof waitForDeployment;
  log?: (message: string) => void;
}

function required(env: NodeJS.ProcessEnv, name: string): string {
  const value = env[name]?.trim();
  if (!value) throw new Error(`Set ${name} in publisher.env`);
  return value;
}

function boolean(env: NodeJS.ProcessEnv, name: string): boolean {
  const value = env[name] ?? "false";
  if (!["true", "false"].includes(value)) throw new Error(`${name} must be true or false`);
  return value === "true";
}

async function credentials(file: string): Promise<{ githubToken: string; openalexKey: string }> {
  const metadata = await lstat(file);
  if (!metadata.isFile() || metadata.size > 65_536) throw new Error("secrets.env must be a plain text file no larger than 64 KiB");
  const contents = await readFile(file, "utf8");
  let values: ReturnType<typeof parseEnv>;
  try {
    values = parseEnv(contents);
  } catch {
    throw new Error("Cannot parse secrets.env; use plain KEY=VALUE entries");
  }
  const allowed = ["PUBLISH_GITHUB_TOKEN", "GSRF_OPENALEX_API_KEY"];
  if (Object.keys(values).some((key) => !allowed.includes(key))) {
    throw new Error("secrets.env must contain only PUBLISH_GITHUB_TOKEN and GSRF_OPENALEX_API_KEY; keep other settings in publisher.env");
  }
  const key = (name: string) => {
    const value = values[name]?.trim();
    if (!value || !/^[\x21-\x7e]+$/.test(value)) throw new Error(`${name} is missing or contains whitespace/control characters in secrets.env`);
    return value;
  };
  return { githubToken: key("PUBLISH_GITHUB_TOKEN"), openalexKey: key("GSRF_OPENALEX_API_KEY") };
}

export async function loadConfig(env: NodeJS.ProcessEnv): Promise<Config> {
  const repository = env.PUBLISH_REPOSITORY?.trim() || "michaelmvh/reading-site";
  if (!/^[a-zA-Z0-9_-]+\/[a-zA-Z0-9_-][a-zA-Z0-9_.-]*$/.test(repository)) throw new Error("PUBLISH_REPOSITORY must be owner/repository");
  if (env.SITE_URL) throw new Error("Remove SITE_URL from publisher.env; the reader build now owns its public URL.");
  const mode = env.PUBLISH_MODE ?? "prepare";
  if (mode !== "prepare" && mode !== "publish") throw new Error("PUBLISH_MODE must be prepare or publish");
  const state = path.resolve(env.PUBLISH_STATE_DIR || "/state");
  if (state === path.parse(state).root) throw new Error("PUBLISH_STATE_DIR cannot be the filesystem root");
  const gitEmail = required(env, "PUBLISH_GIT_EMAIL");
  const mailto = required(env, "GSRF_MAILTO");
  if (!/^[^\s@]+@[^\s@]+$/.test(gitEmail) || !/^[^\s@]+@[^\s@]+$/.test(mailto)) throw new Error("Set valid PUBLISH_GIT_EMAIL and GSRF_MAILTO addresses in publisher.env");
  return {
    state, repository, mode,
    bootstrap: boolean(env, "BOOTSTRAP_EXPORT"),
    allowShrink: boolean(env, "ALLOW_FEED_SHRINK"),
    gitName: required(env, "PUBLISH_GIT_NAME"),
    gitEmail,
    mailto,
    ...await credentials(env.PUBLISH_SECRETS_FILE || "/run/secrets/credentials"),
  };
}

export function redact(message: string, config: Config): string {
  for (const value of [config.githubToken, config.openalexKey]) {
    if (value) message = message.split(value).join("[REDACTED]");
  }
  return message;
}

async function exists(file: string): Promise<boolean> {
  try { await lstat(file); return true; } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return false;
    throw error;
  }
}

async function directory(file: string): Promise<void> {
  await mkdir(file, { recursive: true });
  const metadata = await lstat(file);
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) throw new Error(`Expected a real directory: ${file}`);
}

export async function removeRun(run: string, work: string): Promise<void> {
  if (path.dirname(run) !== work || !/^run-[a-zA-Z0-9]+$/.test(path.basename(run))) {
    throw new Error("Refusing to remove a path outside the publisher's disposable run directories");
  }
  await rm(run, { recursive: true, force: true });
}

async function summary(state: string, value: Record<string, unknown>): Promise<void> {
  const temporary = path.join(state, "last-run.json.tmp");
  await writeFile(temporary, JSON.stringify({ ...value, at: new Date().toISOString() }, null, 2), { mode: 0o600 });
  await rename(temporary, path.join(state, "last-run.json"));
}

export async function publish(config: Config, dependencies: Dependencies = {}): Promise<void> {
  const log = (message: string) => (dependencies.log ?? console.log)(redact(message, config));
  const run: Run = dependencies.run ?? (async (command, args, options) => {
    try {
      const result = await execute(command, args, { ...options, timeout: 30 * 60_000, maxBuffer: 8 * 1024 * 1024 });
      if (result.stderr.trim()) log(result.stderr.trim());
      return result.stdout.trim();
    } catch (error) {
      throw new Error(redact(`${command} failed: ${(error as Error).message}`, config));
    }
  });
  const request = dependencies.request ?? githubClient(config.githubToken);
  const wait = dependencies.wait ?? waitForDeployment;
  await directory(config.state);
  const work = path.join(config.state, "work");
  const home = path.join(config.state, "home");
  const cache = path.join(config.state, "cache");
  for (const dir of [work, home, cache]) await directory(dir);
  const lock = path.join(config.state, "publisher.lock");
  try { await mkdir(lock); } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "EEXIST") throw new Error("A publisher is running, or a stale publisher.lock remains. Inspect Container Manager before removing the lock.");
    throw error;
  }
  let attempt: string | undefined;
  let failure: unknown;
  const checkout = path.join(config.state, "checkout");
  const destination = path.join(checkout, "data");
  const remote = `https://github.com/${config.repository}.git`;
  const cleanEnv: NodeJS.ProcessEnv = {
    PATH: process.env.PATH, HOME: home, LANG: "C.UTF-8",
    NPM_CONFIG_CACHE: path.join(config.state, "npm-cache"),
    NPM_CONFIG_UPDATE_NOTIFIER: "false",
    GIT_TERMINAL_PROMPT: "0", GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_CONFIG_COUNT: "1", GIT_CONFIG_KEY_0: "credential.helper", GIT_CONFIG_VALUE_0: "",
  };
  const gitEnv: NodeJS.ProcessEnv = {
    ...cleanEnv, GIT_ASKPASS: askpass, PUBLISH_GITHUB_TOKEN: config.githubToken,
    GIT_AUTHOR_NAME: config.gitName, GIT_COMMITTER_NAME: config.gitName,
    GIT_AUTHOR_EMAIL: config.gitEmail, GIT_COMMITTER_EMAIL: config.gitEmail,
  };
  const git = (args: string[]) => run("git", args, { cwd: checkout, env: gitEnv });
  try {
    // Fail early for an expired/revoked token, even though cloning the public repository is anonymous.
    await request(`/repos/${config.repository}`);
    if (!(await exists(checkout))) {
      await run("git", ["clone", "--branch", "main", "--single-branch", "--", remote, checkout], { cwd: config.state, env: gitEnv });
    }
    if ((await lstat(checkout)).isSymbolicLink()) throw new Error("Publishing checkout cannot be a symlink");
    if (await realpath(await git(["rev-parse", "--show-toplevel"])) !== await realpath(checkout)) throw new Error("Invalid publishing checkout root");
    if (await git(["remote", "get-url", "origin"]) !== remote) throw new Error("Publishing checkout has an unexpected origin");
    if (await git(["remote", "get-url", "--push", "origin"]) !== remote) throw new Error("Publishing checkout has an unexpected push origin");
    if (await git(["branch", "--show-current"]) !== "main") throw new Error("Publishing checkout must be on main");
    if (await git(["status", "--porcelain"])) throw new Error("Publishing checkout is dirty; inspect it rather than discarding changes");
    await git(["fetch", "origin", "main"]);
    await git(["merge", "--ff-only", "origin/main"]);
    if (await git(["rev-list", "--count", "origin/main..HEAD"]) !== "0") throw new Error("Unpushed commits remain; recover the previous push before starting another export");
    if (!(await exists(path.join(checkout, "scripts/validate-data.ts")))) throw new Error("Merge the JSON-first reading-site PR before running the publisher");
    let previous = await exists(destination);
    if (previous) {
      if (!(await lstat(destination)).isDirectory() || (await lstat(destination)).isSymbolicLink()) throw new Error("Previous data must be a real directory");
      if (!(await exists(path.join(destination, "snapshot.json")))) throw new Error("Previous article snapshot is missing; restore it before publishing");
    } else if (!config.bootstrap) {
      throw new Error("No previous data exists. Set BOOTSTRAP_EXPORT=true only for the initial publication");
    }
    attempt = await mkdtemp(path.join(work, "run-"));
    const candidate = path.join(attempt, "data");
    log("Collecting a JSON snapshot; HTML and RSS are built by GitHub Actions.");
    const args = ["--export", candidate];
    if (previous) args.push("--previous", destination);
    const output = await run("scholarly-rss-feed", args, {
      cwd: config.state,
      env: { ...cleanEnv, GSRF_CONFIG: "/config/feeds.toml", GSRF_CACHE_DIR: cache, GSRF_PROVIDER: "openalex", GSRF_MAILTO: config.mailto, GSRF_OPENALEX_API_KEY: config.openalexKey },
    });
    if (output) log(output);
    await run("npm", ["ci", "--omit=dev", "--ignore-scripts", "--no-audit", "--no-fund"], { cwd: checkout, env: cleanEnv });
    await run("node", ["scripts/validate-data.ts", candidate], {
      cwd: checkout, env: { ...cleanEnv, PREVIOUS_DATA: previous ? destination : "", ALLOW_FEED_SHRINK: String(config.allowShrink) },
    });
    if (config.mode === "prepare") {
      const previousSummary = path.join(config.state, "last-run.json");
      if (await exists(previousSummary)) {
        const old: unknown = JSON.parse(await readFile(previousSummary, "utf8"));
        if (old && typeof old === "object" && "status" in old && old.status === "prepared" && "attempt" in old && typeof old.attempt === "string" && old.attempt !== attempt) {
          await removeRun(old.attempt, work);
        }
      }
      await summary(config.state, { status: "prepared", attempt, output: candidate });
      log(`Prepared and validated ${candidate}. Nothing was committed, pushed, or deployed. Set PUBLISH_MODE=publish when ready.`);
      return;
    }
    // UI code can change during collection. Refresh the data handoff against main
    // and validate against that version's schema without overwriting source changes.
    await git(["fetch", "origin", "main"]);
    await git(["merge", "--ff-only", "origin/main"]);
    previous = await exists(destination);
    await run("npm", ["ci", "--omit=dev", "--ignore-scripts", "--no-audit", "--no-fund"], { cwd: checkout, env: cleanEnv });
    await run("node", ["scripts/validate-data.ts", candidate], {
      cwd: checkout, env: { ...cleanEnv, PREVIOUS_DATA: previous ? destination : "", ALLOW_FEED_SHRINK: String(config.allowShrink) },
    });
    const backup = path.join(attempt, "previous-data");
    if (previous) await rename(destination, backup);
    try { await rename(candidate, destination); } catch (error) {
      if (previous) await rename(backup, destination);
      throw error;
    }
    await git(["add", "--", "data"]);
    // An empty staged diff is a normal no-op, unlike a failed command.
    const diff = await git(["diff", "--cached", "--name-only"]);
    if (!diff) {
      const revision = await git(["rev-parse", "HEAD"]);
      await wait(request, config.repository, revision);
      await summary(config.state, { status: "unchanged", revision });
      log("No public snapshot changes; the existing revision's deployment was confirmed.");
    } else {
      if (diff.split("\n").some((file) => !file.startsWith("data/"))) throw new Error("Refusing to commit changes outside data/");
      await git(["commit", "-m", "Publish scholarly article data"]);
      const revision = await git(["rev-parse", "HEAD"]);
      await git(["push", "origin", "HEAD:main"]);
      log(`Pushed ${revision}; waiting for the matching Pages deploy job.`);
      await wait(request, config.repository, revision);
      await summary(config.state, { status: "deployed", revision });
      log(`Snapshot ${revision} deployed successfully.`);
    }
    await removeRun(attempt, work);
  } catch (error) {
    failure = error;
    try {
      await summary(config.state, { status: "failed", attempt, message: redact((error as Error).message, config) });
    } catch (summaryError) {
      log(`Could not save failure summary: ${(summaryError as Error).message}`);
    }
    log(`Publication failed. ${attempt ? `Inspect ${attempt}. ` : ""}The last successful Pages deployment is not replaced by a failed export or validation.`);
    throw error;
  } finally {
    try { await rmdir(lock); } catch (error) {
      if (!failure) throw error;
      log("Could not remove publisher.lock; inspect it before the next run.");
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  if (process.argv[2] === "--help") {
    console.log("One-shot reading publisher. Configure publisher.env and the read-only secrets.env credentials file. PUBLISH_MODE=prepare is the default; publish commits, pushes, and verifies Pages deployment.");
  } else {
    let config: Config | undefined;
    try {
      if (process.argv.length > 2) throw new Error("Unexpected command-line arguments; configuration is supplied through publisher.env");
      process.umask(0o077);
      config = await loadConfig(process.env);
      await publish(config);
    } catch (error) {
      const message = (error as Error).message;
      console.error(config ? redact(message, config) : message);
      process.exitCode = 1;
    }
  }
}
