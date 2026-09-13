#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const releaseManifest = await json(".release-please-manifest.json");

for (const directory of ["integrations/opencode", "integrations/pi", "integrations/claude"]) {
  const manifest = await json(join(directory, "package.json"));
  const lock = await json(join(directory, "package-lock.json"));
  assert.equal(manifest.version, releaseManifest[directory], `${directory} package version must match release-please`);
  assert.equal(lock.version, manifest.version, `${directory} lockfile version must match package.json`);
  assert.equal(lock.packages?.[""]?.version, manifest.version, `${directory} lockfile root package must match package.json`);
}

const claudePackage = await json("integrations/claude/package.json");
const claudeManifest = await json("integrations/claude/.claude-plugin/plugin.json");
const marketplace = await json(".claude-plugin/marketplace.json");
const marketplaceEntry = marketplace.plugins.find((plugin) => plugin.name === "phux");
assert.ok(marketplaceEntry, "Claude marketplace must contain the phux plugin");
assert.equal(claudeManifest.version, claudePackage.version, "Claude plugin manifest version must match package.json");
assert.equal(marketplaceEntry.version, claudePackage.version, "Claude marketplace version must match package.json");
assert.equal(marketplaceEntry.source, "./integrations/claude", "Claude marketplace source must remain repository-relative");

const opencode = await json("integrations/opencode/package.json");
assert.match(opencode.dependencies?.["@opencode-ai/plugin"] ?? "", /^\d+\.\d+\.\d+$/, "OpenCode plugin API must be pinned exactly");
assert.equal(opencode.publishConfig?.access, "public");
assert.equal(opencode.publishConfig?.provenance, true);

const pi = await json("integrations/pi/package.json");
assert.equal(pi.private, undefined, "Pi extension must remain publishable");
assert.equal(pi.publishConfig?.access, "public");
assert.equal(pi.publishConfig?.provenance, true);
assert.equal(pi.dependencies?.["@phux/integration-runtime"], "file:../runtime");
assert.ok(
  pi.bundledDependencies?.includes("@phux/integration-runtime"),
  "Pi must carry the private runtime in its independently installable artifact",
);

const runtime = await json("integrations/runtime/package.json");
const runtimeLock = await json("integrations/runtime/package-lock.json");
assert.equal(runtime.private, true, "the shared runtime is an implementation module, not a public product");
assert.equal(runtimeLock.version, runtime.version);
assert.equal(runtimeLock.packages?.[""]?.version, runtime.version);

const openCodeSource = await readdir(join(root, "integrations/opencode/src"), { recursive: true });
for (const relative of openCodeSource.filter((path) => path.endsWith(".ts"))) {
  const source = await readFile(join(root, "integrations/opencode/src", relative), "utf8");
  assert.doesNotMatch(
    source,
    /(?:@phux\/pi|integrations\/pi|\.\.\/\.\.\/pi\/)/,
    `OpenCode source must not import implementation owned by Pi: ${relative}`,
  );
}

process.stdout.write("agent integration versions and neutral runtime ownership are coherent\n");

async function json(path) {
  return JSON.parse(await readFile(join(root, path), "utf8"));
}
