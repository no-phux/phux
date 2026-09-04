import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const packageRoot = new URL("../", import.meta.url);
const temporaryRoot = await mkdtemp(join(tmpdir(), "phux-opencode-pack-"));

try {
  const packed = JSON.parse(execFileSync(
    "npm",
    // --no-audit: npm pack consults the live advisory endpoint; a transient
    // 503 there must not fail pack verification (see .npmrc). The npm_config_*
    // env set by `just agent-integrations-check` already covers this when
    // driven through just; the flag keeps the script safe standalone.
    ["pack", "--json", "--ignore-scripts", "--no-audit", "--pack-destination", temporaryRoot],
    { cwd: packageRoot, encoding: "utf8" },
  ));
  assert.equal(packed.length, 1);
  const manifest = packed[0];
  const names = manifest.files.map((file) => file.path).sort();
  assert.deepEqual(names, [
    "README.md",
    "dist/index.d.ts",
    "dist/index.js",
    "package.json",
  ]);

  const tarball = join(temporaryRoot, manifest.filename);
  const consumerRoot = join(temporaryRoot, "consumer");
  // --no-audit: the advisory bulk endpoint has returned 503s (observed
  // 2026-09-04) and a transient registry audit outage must not fail a CI
  // pack-verification lane. Dependency hygiene is cargo-deny's contract.
  execFileSync(
    "npm",
    ["install", "--ignore-scripts", "--omit=dev", "--no-audit", "--no-fund", "--prefix", consumerRoot, tarball],
    { stdio: "pipe" },
  );

  const installedEntry = join(consumerRoot, "node_modules", "@phux", "opencode", "dist", "index.js");
  const bundledSource = await readFile(installedEntry, "utf8");
  assert.doesNotMatch(bundledSource, /(?:from\s+|import\s*\()["'](?:@phux\/pi|\.\.\/\.\.\/pi)/);
  assert.match(bundledSource, /@opencode-ai\/plugin/, "the package must use OpenCode's public V2 runtime dependency");
  assert.match(bundledSource, /child_process/);

  const plugin = await import(pathToFileURL(installedEntry).href);
  assert.equal(plugin.default.id, "phux.terminal");
  assert.equal(typeof plugin.default.setup, "function");
  assert.equal(typeof plugin.PhuxCli, "function");
} finally {
  await rm(temporaryRoot, { recursive: true, force: true });
}
