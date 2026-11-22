import { expect, test } from "bun:test";
import { cpSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import { integrationViolations } from "./versions.ts";

test("the repository is coherent", () => {
  expect(integrationViolations()).toEqual([]);
});

test("version, publish and ownership drift is reported", () => {
  const dir = mkdtempSync(join(tmpdir(), "integrations-"));
  for (const path of [
    ".release-please-manifest.json",
    ".claude-plugin",
    ".grok-plugin",
    "integrations",
  ]) {
    const filter = (source: string) => !source.includes("node_modules");
    cpSync(join(ROOT, path), join(dir, path), { recursive: true, filter });
  }
  const edit = (path: string, change: Record<string, unknown>) => {
    const document = JSON.parse(readFileSync(join(dir, path), "utf8")) as Record<string, unknown>;
    writeFileSync(join(dir, path), JSON.stringify({ ...document, ...change }));
  };
  edit("integrations/claude/.claude-plugin/plugin.json", { version: "0.0.0" });
  edit("integrations/omp/package.json", { private: true });
  writeFileSync(
    join(dir, "integrations/opencode-v2/src/borrowed.ts"),
    'import "../../pi/src/index.ts";\n',
  );
  const [manifest, ...rest] = integrationViolations(dir);
  expect(manifest).toMatch(/^claude plugin manifest version must match package.json .*"0.0.0"\)$/);
  expect(rest).toEqual([
    "omp must remain publishable (expected nothing, found true)",
    "OpenCode source must not import implementation owned by Pi: borrowed.ts",
  ]);
});
