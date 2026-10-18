import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { repoPath } from "../lib/repo.ts";

test("the uncovered e2e diagnostic prints just stress without running it", () => {
  const line = readFileSync(repoPath("scripts/check-e2e-lanes.sh"), "utf8")
    .split("\n")
    .find((text) => text.includes("recipe (run locally with"));
  expect(line).toBeDefined();
  const result = Bun.spawnSync([
    "bash",
    "-euo",
    "pipefail",
    "-c",
    `phux_hint() { printf '%s\\n' "$1"; }; just() { echo UNEXPECTED_EXECUTION >&2; }; ${line}`,
  ]);
  expect(result.exitCode).toBe(0);
  expect(result.stdout.toString()).toContain("`just stress`");
  expect(result.stderr.toString()).not.toContain("UNEXPECTED_EXECUTION");
});
