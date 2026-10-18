import { describe, expect, test } from "bun:test";
import { cpSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import { table } from "../lib/value.ts";
import { integrationViolations } from "./versions.ts";

/** A copy of the files the contract reads, so a test can break one. */
function fixture(): string {
  const dir = mkdtempSync(join(tmpdir(), "integrations-"));
  for (const path of [".release-please-manifest.json", ".claude-plugin", ".grok-plugin"]) {
    cpSync(join(ROOT, path), join(dir, path), { recursive: true });
  }
  cpSync(join(ROOT, "integrations"), join(dir, "integrations"), {
    recursive: true,
    filter: (source) => !source.includes("node_modules"),
  });
  return dir;
}

function edit(
  dir: string,
  path: string,
  change: (document: Record<string, unknown>) => void,
): void {
  const document = table(JSON.parse(readFileSync(join(dir, path), "utf8")));
  change(document);
  writeFileSync(join(dir, path), JSON.stringify(document));
}

describe("agent integration versions", () => {
  test("the repository is coherent", () => {
    expect(integrationViolations()).toEqual([]);
  });

  test("version, publish and ownership drift is reported", () => {
    const dir = fixture();
    edit(dir, "integrations/claude/.claude-plugin/plugin.json", (manifest) => {
      manifest["version"] = "0.0.0";
    });
    edit(dir, "integrations/omp/package.json", (omp) => {
      omp["private"] = true;
    });
    writeFileSync(
      join(dir, "integrations/opencode-v2/src/borrowed.ts"),
      'import "../../pi/src/index.ts";\n',
    );
    expect(integrationViolations(dir)).toEqual([
      'Claude plugin manifest version must match package.json (expected "' +
        String(
          table(JSON.parse(readFileSync(join(dir, "integrations/claude/package.json"), "utf8")))[
            "version"
          ],
        ) +
        '", found "0.0.0")',
      "OMP extension must remain publishable (expected nothing, found true)",
      "OpenCode source must not import implementation owned by Pi: borrowed.ts",
    ]);
  });
});
