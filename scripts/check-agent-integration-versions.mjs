#!/usr/bin/env node

import assert from "node:assert/strict";
import { status } from "./lib/output.mjs";
import { readFile, readdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const releaseManifest = await json(".release-please-manifest.json");

for (const directory of ["integrations/pi", "integrations/claude"]) {
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

const opencode = await json("integrations/opencode-v2/package.json");
assert.equal(opencode.name, "@phux/opencode", "OpenCode V2 plugin publishes under the existing npm package");
assert.equal(opencode.private, undefined, "OpenCode plugin must remain publishable");
assert.equal(opencode.version, releaseManifest["integrations/opencode-v2"], "integrations/opencode-v2 package version must match release-please");
assert.equal(opencode.publishConfig?.access, "public");
assert.equal(opencode.publishConfig?.provenance, true);
assert.match(opencode.dependencies?.["@opencode/plugin"] ?? "", /^\d+\.\d+\.\d+$/, "OpenCode V2 plugin API must be pinned exactly");

const grokPackage = await json("integrations/grok/package.json");
const grokManifest = await json("integrations/grok/.grok-plugin/plugin.json");
const grokMarketplace = await json(".grok-plugin/marketplace.json");
const grokEntry = grokMarketplace.plugins.find((plugin) => plugin.name === "phux");
assert.equal(grokPackage.private, true, "the Grok plugin ships from its marketplace in this repository, not npm");
assert.ok(grokEntry, "Grok marketplace must contain the phux plugin");
assert.equal(grokManifest.version, grokPackage.version, "Grok plugin manifest version must match package.json");
assert.equal(grokEntry.version, grokPackage.version, "Grok marketplace version must match package.json");
assert.equal(grokEntry.source, "./integrations/grok", "Grok marketplace source must remain repository-relative");

const omp = await json("integrations/omp/package.json");
assert.equal(omp.private, undefined, "OMP extension must remain publishable");
assert.equal(omp.publishConfig?.access, "public");
assert.equal(omp.publishConfig?.provenance, true);

// Release Please records Grok and OMP in the manifest at their first release.
for (const [directory, manifest] of [["integrations/grok", grokPackage], ["integrations/omp", omp]]) {
  const released = releaseManifest[directory];
  if (released !== undefined) assert.equal(manifest.version, released, `${directory} package version must match release-please`);
}
assert.match(omp.devDependencies?.["@oh-my-pi/pi-coding-agent"] ?? "", /^\d+\.\d+\.\d+$/, "OMP SDK must be pinned exactly");
assert.deepEqual(omp.omp?.extensions, ["./dist/index.js"]);
assert.ok(omp.files?.includes("skills"), "OMP must ship its native-tool skill");
assert.deepEqual(omp.dependencies ?? {}, {}, "OMP bundles the neutral runtime without runtime dependencies");

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

const openCodeSource = await readdir(join(root, "integrations/opencode-v2/src"), { recursive: true });
for (const relative of openCodeSource.filter((path) => path.endsWith(".ts"))) {
  const source = await readFile(join(root, "integrations/opencode-v2/src", relative), "utf8");
  assert.doesNotMatch(
    source,
    /(?:@phux\/pi|integrations\/pi|\.\.\/\.\.\/pi\/)/,
    `OpenCode source must not import implementation owned by Pi: ${relative}`,
  );
}

status("ok", "agent integration versions and neutral runtime ownership are coherent");

async function json(path) {
  return JSON.parse(await readFile(join(root, path), "utf8"));
}
