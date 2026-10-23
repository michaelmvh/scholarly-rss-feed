import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, mkdir, writeFile, readFile, readdir, rm, lstat } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { githubClient, waitForDeployment } from "../publisher/github.mts";
import { loadConfig, publish, redact, removeRun, type Config, type Run } from "../publisher/main.mts";

const identity = {
  ...process.env, GIT_AUTHOR_NAME: "Fixture", GIT_AUTHOR_EMAIL: "fixture@example.org",
  GIT_COMMITTER_NAME: "Fixture", GIT_COMMITTER_EMAIL: "fixture@example.org",
};

async function setup() {
  const root = await mkdtemp(path.join(tmpdir(), "container-publisher-"));
  const remote = path.join(root, "remote.git");
  const seed = path.join(root, "seed");
  await mkdir(seed);
  const git = (cwd: string, args: string[]) => execFileSync("git", args, { cwd, env: identity, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
  git(root, ["init", "--bare", "--initial-branch=main", remote]);
  git(seed, ["init", "--initial-branch=main"]);
  await mkdir(path.join(seed, "scripts"));
  await writeFile(path.join(seed, "scripts/validate-data.ts"), "// fixture");
  await writeFile(path.join(seed, "keep.txt"), "must survive");
  git(seed, ["add", "."]); git(seed, ["commit", "-m", "Bootstrap"]);
  git(seed, ["remote", "add", "origin", remote]); git(seed, ["push", "-u", "origin", "main"]);
  const config: Config = {
    state: path.join(root, "state"), repository: "fixture/reading-site",
    mode: "prepare", bootstrap: true,
    allowShrink: false, gitName: "Fixture", gitEmail: "fixture@example.org",
    mailto: "fixture@example.org", githubToken: "fixture-github-token", openalexKey: "fixture-openalex-key",
  };
  const calls: { command: string; args: string[]; env: NodeJS.ProcessEnv }[] = [];
  const log: string[] = [];
  let fail: "export" | "validate" | "push" | "deploy" | undefined;
  let wrongOrigin = false;
  let deploymentChecks = 0;
  let updateUiDuringCollection = false;
  const run: Run = async (command, args, options) => {
    calls.push({ command, args, env: options.env });
    if (command === "git") {
      if (args[0] === "clone") {
        return git(options.cwd, args.map((arg) => arg === `https://github.com/${config.repository}.git` ? remote : arg));
      }
      if (args[0] === "remote" && args[1] === "get-url") {
        return wrongOrigin ? "https://example.org/unexpected.git" : `https://github.com/${config.repository}.git`;
      }
      if (args[0] === "push" && fail === "push") throw new Error("push failed");
      return execFileSync("git", args, { ...options, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
    }
    if (command === "scholarly-rss-feed") {
      if (fail === "export") throw new Error("export failed");
      const candidate = args[args.indexOf("--export") + 1];
      await mkdir(candidate, { recursive: true });
      await writeFile(path.join(candidate, "snapshot.json"), '{"schema_version":1,"article_files":[]}');
      if (updateUiDuringCollection) {
        await writeFile(path.join(seed, "reader-ui.txt"), "independent UI update");
        git(seed, ["add", "reader-ui.txt"]); git(seed, ["commit", "-m", "Update reader UI"]); git(seed, ["push"]);
        updateUiDuringCollection = false;
      }
      return `Generated fixture (redaction probe: ${config.openalexKey})`;
    }
    if (command === "npm") return "";
    if (command === "node") {
      if (fail === "validate") throw new Error("validation failed");
      assert.ok((await lstat(path.join(args[1], "snapshot.json"))).isFile());
      return "";
    }
    throw new Error(`Unexpected command: ${command}`);
  };
  return {
    config, root, log, calls,
    git: (args: string[]) => git(path.join(config.state, "checkout"), args),
    fail: (value: typeof fail) => { fail = value; },
    wrongOrigin: () => { wrongOrigin = true; },
    updateUi: () => { updateUiDuringCollection = true; },
    deploymentChecks: () => deploymentChecks,
    run: () => publish(config, {
      run, request: async () => ({}), log: (message) => log.push(message),
      wait: async () => { deploymentChecks++; if (fail === "deploy") throw new Error("deploy failed"); },
    }),
  };
}

test("prepare mode validates without pushing and keeps only its latest prepared snapshot", async () => {
  const f = await setup();
  try {
    await f.run();
    assert.equal(f.git(["rev-list", "--count", "HEAD"]), "1");
    assert.equal(f.git(["status", "--porcelain"]), "");
    assert.equal(f.deploymentChecks(), 0);
    const first = JSON.parse(await readFile(path.join(f.config.state, "last-run.json"), "utf8"));
    assert.equal(first.status, "prepared");
    await f.run();
    assert.equal((await readdir(path.join(f.config.state, "work"))).length, 1);
    assert.ok(f.log.some((message) => message.includes("Nothing was committed")));
    assert.ok(!f.log.join("\n").includes(f.config.openalexKey));
  } finally { await rm(f.root, { recursive: true }); }
});

test("publish commits only JSON data, waits for deployment, and skips unchanged reruns", async () => {
  const f = await setup();
  try {
    f.config.mode = "publish";
    await f.run();
    assert.equal(f.git(["rev-list", "--count", "HEAD"]), "2");
    assert.equal(f.git(["status", "--porcelain"]), "");
    assert.equal(f.deploymentChecks(), 1);
    assert.equal(await readFile(path.join(f.config.state, "checkout/keep.txt"), "utf8"), "must survive");
    f.config.bootstrap = false;
    await f.run();
    assert.equal(f.git(["rev-list", "--count", "HEAD"]), "2");
    assert.equal(f.deploymentChecks(), 2);
    assert.equal((await readdir(path.join(f.config.state, "work"))).length, 0);
    assert.ok(f.calls.some((call) => call.command === "scholarly-rss-feed" && call.args.includes("--previous")));
  } finally { await rm(f.root, { recursive: true }); }
});

test("credentials are isolated from validation/npm and never put in command arguments", async () => {
  const f = await setup();
  try {
    await f.run();
    for (const call of f.calls) {
      assert.ok(!call.args.join(" ").includes(f.config.githubToken));
      assert.ok(!call.args.join(" ").includes(f.config.openalexKey));
      if (call.command === "git") {
        assert.equal(call.env.PUBLISH_GITHUB_TOKEN, f.config.githubToken);
        assert.equal(call.env.GSRF_OPENALEX_API_KEY, undefined);
      } else if (call.command === "scholarly-rss-feed") {
        assert.equal(call.env.GSRF_OPENALEX_API_KEY, f.config.openalexKey);
        assert.equal(call.env.PUBLISH_GITHUB_TOKEN, undefined);
      } else {
        assert.equal(call.env.PUBLISH_GITHUB_TOKEN, undefined);
        assert.equal(call.env.GSRF_OPENALEX_API_KEY, undefined);
      }
    }
    assert.equal(redact(`${f.config.githubToken} ${f.config.openalexKey}`, f.config), "[REDACTED] [REDACTED]");
  } finally { await rm(f.root, { recursive: true }); }
});

test("a concurrent reader UI commit is preserved while the publisher commits only data", async () => {
  const f = await setup();
  try {
    f.config.mode = "publish";
    f.updateUi();
    await f.run();
    assert.equal(await readFile(path.join(f.config.state, "checkout/reader-ui.txt"), "utf8"), "independent UI update");
    assert.equal(f.git(["show", "--pretty=", "--name-only", "HEAD"]), "data/snapshot.json");
    assert.equal(f.git(["status", "--porcelain"]), "");
  } finally { await rm(f.root, { recursive: true }); }
});

test("failed export/validation preserves the checkout; push/deploy failures remain explicit", async () => {
  for (const phase of ["export", "validate", "push", "deploy"] as const) {
    const f = await setup();
    try {
      f.config.mode = "publish";
      f.fail(phase);
      await assert.rejects(f.run(), new RegExp(`${phase === "validate" ? "validation" : phase} failed`));
      const summary = JSON.parse(await readFile(path.join(f.config.state, "last-run.json"), "utf8"));
      assert.equal(summary.status, "failed");
      assert.equal(f.git(["status", "--porcelain"]), "");
      assert.equal(f.git(["rev-list", "--count", "HEAD"]), ["export", "validate"].includes(phase) ? "1" : "2");
      assert.ok(!(await readdir(f.config.state)).includes("publisher.lock"));
      if (phase === "push") {
        f.fail(undefined);
        await assert.rejects(f.run(), /Unpushed commits/);
      }
    } finally { await rm(f.root, { recursive: true }); }
  }
});

test("guards bootstrap, dirty checkouts, unexpected remotes, concurrency, and cleanup boundaries", async () => {
  const f = await setup();
  try {
    f.config.bootstrap = false;
    await assert.rejects(f.run(), /BOOTSTRAP_EXPORT/);
    f.config.bootstrap = true;
    await writeFile(path.join(f.config.state, "checkout/keep.txt"), "user edit");
    await assert.rejects(f.run(), /dirty/);
    await writeFile(path.join(f.config.state, "checkout/keep.txt"), "must survive");
    f.wrongOrigin();
    await assert.rejects(f.run(), /unexpected origin/);
    await mkdir(path.join(f.config.state, "publisher.lock"));
    await assert.rejects(f.run(), /publisher is running/);
    await assert.rejects(removeRun(f.config.state, path.join(f.config.state, "work")), /Refusing/);
  } finally { await rm(f.root, { recursive: true }); }
});

test("configuration reads one dotenv credentials file and prepare is the default", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "publisher-config-"));
  try {
    const secrets = path.join(root, "secrets.env");
    await writeFile(secrets, '# Private values\nPUBLISH_GITHUB_TOKEN="fixture-gh"\nGSRF_OPENALEX_API_KEY=fixture-oa\n');
    const env = {
      PUBLISH_GIT_NAME: "Fixture", PUBLISH_GIT_EMAIL: "fixture@example.org",
      GSRF_MAILTO: "fixture@example.org", PUBLISH_SECRETS_FILE: secrets,
    };
    const config = await loadConfig(env);
    assert.equal(config.mode, "prepare");
    assert.equal(config.githubToken, "fixture-gh");
    assert.equal(config.openalexKey, "fixture-oa");
    await assert.rejects(loadConfig({ ...env, SITE_URL: "https://example.org/" }), /reader build/);
    await assert.rejects(loadConfig({ ...env, PUBLISH_MODE: "unsafe" }), /PUBLISH_MODE/);
    await assert.rejects(loadConfig({ ...env, PUBLISH_REPOSITORY: "https://evil.example/repo" }), /owner\/repository/);
    await writeFile(secrets, 'PUBLISH_GITHUB_TOKEN="bad\ntoken"\nGSRF_OPENALEX_API_KEY=fixture-oa\n');
    await assert.rejects(loadConfig(env), /control characters/);
    await writeFile(secrets, "PUBLISH_GITHUB_TOKEN=fixture-gh\n");
    await assert.rejects(loadConfig(env), /GSRF_OPENALEX_API_KEY is missing/);
    await writeFile(secrets, "PUBLISH_GITHUB_TOKEN=fixture-gh\nGSRF_OPENALEX_API_KEY=fixture-oa\nPUBLISH_MODE=publish\n");
    await assert.rejects(loadConfig(env), /keep other settings in publisher.env/);
    await writeFile(secrets, "x".repeat(65_537));
    await assert.rejects(loadConfig(env), /64 KiB/);
  } finally { await rm(root, { recursive: true }); }
});

test("Git askpass refuses any non-GitHub credential request", () => {
  const script = fileURLToPath(new URL("../publisher/git-askpass.sh", import.meta.url));
  const run = (prompt: string) => execFileSync("sh", [script, prompt], { env: { ...process.env, PUBLISH_GITHUB_TOKEN: "fixture-token" }, encoding: "utf8" }).trim();
  assert.equal(run("Username for 'https://github.com': "), "x-access-token");
  assert.equal(run("Password for 'https://x-access-token@github.com': "), "fixture-token");
  assert.throws(() => run("Password for 'https://x-access-token@github.com.evil.example': "));
});

test("GitHub client restricts redirects and reports authorization failures without echoing a key", async () => {
  const fetcher: typeof fetch = async (input, init) => {
    assert.equal(String(input), "https://api.github.com/repos/fixture/site");
    assert.equal(init?.redirect, "error");
    assert.equal(new Headers(init?.headers).get("Authorization"), "Bearer fixture-token");
    return new Response("{}", { status: 403 });
  };
  await assert.rejects(githubClient("fixture-token", fetcher)("/repos/fixture/site"), /HTTP 403/);
});

test("deployment polling distinguishes success, skipped deployment, failure, and timeout", async () => {
  for (const conclusion of ["success", "skipped", "failure"]) {
    const request = async (endpoint: string) => endpoint.includes("/jobs?")
      ? { jobs: [{ name: "deploy", conclusion }] }
      : { workflow_runs: [{ id: 1, head_sha: "revision", status: "completed", conclusion: "success" }] };
    if (conclusion === "success") await waitForDeployment(request, "fixture/site", "revision");
    else await assert.rejects(waitForDeployment(request, "fixture/site", "revision"), /NOT deployed/);
  }
  let time = 0;
  await assert.rejects(waitForDeployment(async () => ({ workflow_runs: [] }), "fixture/site", "revision", {
    timeoutMs: 2, intervalMs: 1, now: () => time, sleep: async (ms) => { time += ms; },
  }), /Timed out/);
  await assert.rejects(waitForDeployment(async () => ({ workflow_runs: [{ id: 1, head_sha: "revision", status: "completed", conclusion: "failure" }] }), "fixture/site", "revision"), /did not succeed/);
});
