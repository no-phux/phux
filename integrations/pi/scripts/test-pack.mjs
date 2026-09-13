import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const packageDir = dirname(dirname(fileURLToPath(import.meta.url)));
const temp = await mkdtemp(join(tmpdir(), "phux-pi-pack-"));

try {
  const packed = run("npm", ["pack", "--json", "--ignore-scripts", "--pack-destination", temp], packageDir);
  const report = JSON.parse(packed.stdout);
  assert.equal(report.length, 1, "npm pack must produce exactly one archive");
  const entry = report[0];
  assert.equal(entry.name, "@phux/pi");

  const files = new Set(entry.files.map((file) => file.path));
  for (const required of [
    "README.md",
    "package.json",
    "extensions/index.ts",
    "dist/extensions/index.js",
    "dist/src/index.js",
    "dist/src/index.d.ts",
    "node_modules/@phux/integration-runtime/dist/src/adapter.js",
    "node_modules/@phux/integration-runtime/dist/src/adapter.d.ts",
    "node_modules/@phux/integration-runtime/package.json",
  ]) {
    assert.ok(files.has(required), `packed archive is missing ${required}`);
  }
  for (const path of files) {
    assert.ok(!path.startsWith("test/"), `tests must not ship in the archive: ${path}`);
    assert.ok(!path.startsWith("scripts/"), `validation scripts must not ship in the archive: ${path}`);
    if (path.startsWith("node_modules/")) {
      assert.ok(
        path.startsWith("node_modules/@phux/integration-runtime/"),
        `only the private integration runtime may ship as a bundled dependency: ${path}`,
      );
    }
  }

  const archive = join(temp, entry.filename);
  run("tar", ["-xzf", archive, "-C", temp], packageDir);
  const packedRoot = join(temp, "package");
  const metadata = JSON.parse(await readFile(join(packedRoot, "package.json"), "utf8"));
  assert.equal(metadata.private, undefined, "the release package must be publishable");
  assert.deepEqual(metadata.publishConfig, { access: "public", provenance: true });
  assert.deepEqual(metadata.pi?.extensions, ["./extensions/index.ts"]);
  assert.equal(metadata.exports?.["."]?.import, "./dist/src/index.js");

  const adapterTypes = await readFile(
    join(packedRoot, "node_modules", "@phux", "integration-runtime", "dist", "src", "adapter.d.ts"),
    "utf8",
  );
  for (const method of ["insertPane", "movePane", "swapPane"]) {
    assert.match(adapterTypes, new RegExp(`\\b${method}\\(`), `packed adapter types are missing ${method}`);
  }
  const toolTypes = await readFile(join(packedRoot, "dist", "src", "tools.d.ts"), "utf8");
  for (const schema of ["PhuxInsertPaneParams", "PhuxMovePaneParams", "PhuxSwapPaneParams"]) {
    assert.match(toolTypes, new RegExp(`\\b${schema}\\b`), `packed tool types are missing ${schema}`);
  }

  process.stdout.write(`pack check passed: ${entry.filename} (${String(entry.files.length)} files)\n`);
} finally {
  await rm(temp, { recursive: true, force: true });
}

function run(command, args, cwd) {
  const result = spawnSync(command, args, { cwd, encoding: "utf8", env: process.env });
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed (${String(result.status)}):\n${result.stderr}`);
  }
  return result;
}
