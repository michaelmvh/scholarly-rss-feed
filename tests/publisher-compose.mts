import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { copyFile, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

const root = await mkdtemp(path.join(tmpdir(), "publisher-compose-"));
try {
  await copyFile("NAS/static-compose.yml", path.join(root, "static-compose.yml"));
  await copyFile("NAS/publisher.env.example", path.join(root, "publisher.env"));
  await mkdir(path.join(root, "secrets"));
  await writeFile(path.join(root, "secrets/github-token.txt"), "fixture-github");
  await writeFile(path.join(root, "secrets/openalex-api-key.txt"), "fixture-openalex");
  const value: unknown = JSON.parse(execFileSync("docker", [
    "compose", "-f", path.join(root, "static-compose.yml"), "config", "--format", "json",
  ], { encoding: "utf8" }));
  assert.ok(value && typeof value === "object" && "services" in value);
  const services = value.services as Record<string, Record<string, unknown>>;
  assert.deepEqual(Object.keys(services), ["publisher"]);
  const service = services.publisher;
  assert.equal(service.read_only, true);
  assert.equal(service.privileged, undefined);
  assert.equal(service.ports, undefined);
  assert.equal(service.restart, "no");
  assert.deepEqual(service.cap_drop, ["ALL"]);
  const serialized = JSON.stringify(service);
  assert.ok(!serialized.includes("docker.sock"));
  assert.ok(!serialized.includes("fixture-github"));
  assert.ok(!serialized.includes("fixture-openalex"));
  assert.ok(serialized.includes("/run/secrets/github_token"));
  assert.ok(serialized.includes('"PUBLISH_MODE":"prepare"'));
  console.log("Single-container Compose configuration validated; no ports, host tools, inline credentials, or Docker socket.");
} finally {
  await rm(root, { recursive: true });
}
