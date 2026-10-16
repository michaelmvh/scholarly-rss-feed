import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

const script = path.resolve("NAS/publish-static.sh");
const gitEnv = {
  ...process.env, GIT_AUTHOR_NAME: "Fixture", GIT_AUTHOR_EMAIL: "fixture@example.org",
  GIT_COMMITTER_NAME: "Fixture", GIT_COMMITTER_EMAIL: "fixture@example.org",
};

function setup() {
  const root = mkdtempSync(path.join(tmpdir(), "nas-publish-test-"));
  const checkout = path.join(root, "checkout");
  const remote = path.join(root, "remote.git");
  const work = path.join(root, "work");
  mkdirSync(checkout); mkdirSync(work);
  const git = (...args) => execFileSync("git", ["-C", checkout, ...args], { env: gitEnv, stdio: "pipe", encoding: "utf8" }).trim();
  execFileSync("git", ["init", "--bare", "--initial-branch=main", remote], { stdio: "pipe" });
  git("init", "--initial-branch=main");
  mkdirSync(path.join(checkout, "scripts"));
  writeFileSync(path.join(checkout, "scripts/validate.ts"), "// fixture tooling");
  writeFileSync(path.join(checkout, "keep.txt"), "must survive");
  git("add", "."); git("commit", "-m", "Bootstrap"); git("remote", "add", "origin", remote); git("push", "-u", "origin", "main");
  const docker = path.join(root, "docker");
  writeFileSync(docker, `#!/bin/sh
set -eu
case " $* " in
  *" exporter "*)
    [ "\${FAIL_EXPORT:-0}" != 1 ] || exit 17
    mkdir -p "$STATIC_RUN_DIR/site/data"
    printf '%s' '{"schema_version":1,"articles":{}}' > "$STATIC_RUN_DIR/site/data/archive.json"
    printf '%s' 'fixture snapshot' > "$STATIC_RUN_DIR/site/index.html"
    ;;
  *" validator "*) [ "\${FAIL_VALIDATE:-0}" != 1 ] || exit 18 ;;
  *) exit 19 ;;
esac
`, { mode: 0o755 });
  const gh = path.join(root, "gh");
  writeFileSync(gh, `#!/bin/sh
case "$2" in
  list) echo 123 ;;
  watch) [ "\${FAIL_DEPLOY:-0}" != 1 ] ;;
  view) echo "\${DEPLOY_CONCLUSION:-success}" ;;
  *) exit 20 ;;
esac
`, { mode: 0o755 });
  const env = {
    ...gitEnv, PUBLISH_CHECKOUT: checkout, WORK_ROOT: work,
    SITE_URL: "https://example.org/", STATIC_IMAGE: "fixture", CACHE_VOLUME: "fixture",
    GSRF_MAILTO: "fixture@example.org", GSRF_OPENALEX_API_KEY: "fixture-key",
    DOCKER_BIN: docker, GH_BIN: gh, BOOTSTRAP_EXPORT: "1",
  };
  return {
    root, checkout, work, git,
    run(extra = {}) { return spawnSync("sh", [script], { env: { ...env, ...extra }, encoding: "utf8" }); },
  };
}

test("publication pushes only output, confirms deployment, and skips unchanged reruns", () => {
  const fixture = setup();
  try {
    const first = fixture.run();
    assert.equal(first.status, 0, first.stderr);
    assert.match(first.stdout, /deployed successfully/);
    assert.equal(readFileSync(path.join(fixture.checkout, "keep.txt"), "utf8"), "must survive");
    assert.equal(fixture.git("status", "--porcelain"), "");
    const revision = fixture.git("rev-parse", "HEAD");
    const next = fixture.run({ BOOTSTRAP_EXPORT: "0" });
    assert.equal(next.status, 0, next.stderr);
    assert.match(next.stdout, /nothing to publish/);
    assert.equal(fixture.git("rev-parse", "HEAD"), revision);
    assert.equal(existsSync(path.join(fixture.work, "publish.lock")), false);
  } finally { rmSync(fixture.root, { recursive: true }); }
});

test("failed export or validation does not modify the checkout", () => {
  for (const flag of ["FAIL_EXPORT", "FAIL_VALIDATE"]) {
    const fixture = setup();
    try {
      const revision = fixture.git("rev-parse", "HEAD");
      const result = fixture.run({ [flag]: "1" });
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /Publication failed/);
      assert.equal(fixture.git("rev-parse", "HEAD"), revision);
      assert.equal(fixture.git("status", "--porcelain"), "");
      assert.equal(existsSync(path.join(fixture.work, "publish.lock")), false);
    } finally { rmSync(fixture.root, { recursive: true }); }
  }
});

test("bootstrap, dirty checkout, and concurrent execution are guarded", () => {
  const fixture = setup();
  try {
    assert.notEqual(fixture.run({ BOOTSTRAP_EXPORT: "0" }).status, 0);
    writeFileSync(path.join(fixture.checkout, "keep.txt"), "user change");
    assert.match(fixture.run().stderr, /checkout is dirty/);
    mkdirSync(path.join(fixture.work, "publish.lock"));
    assert.equal(fixture.run().status, 75);
    assert.equal(readFileSync(path.join(fixture.checkout, "keep.txt"), "utf8"), "user change");
  } finally { rmSync(fixture.root, { recursive: true }); }
});

test("failed or skipped deployment is not success-shaped", () => {
  for (const extra of [{ FAIL_DEPLOY: "1" }, { DEPLOY_CONCLUSION: "skipped" }]) {
    const fixture = setup();
    try {
      const result = fixture.run(extra);
      assert.notEqual(result.status, 0);
      assert.doesNotMatch(result.stdout, /deployed successfully/);
      assert.equal(fixture.git("status", "--porcelain"), "");
    } finally { rmSync(fixture.root, { recursive: true }); }
  }
});
