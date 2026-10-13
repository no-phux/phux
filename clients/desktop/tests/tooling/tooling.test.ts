import { expect, test } from "bun:test";
import { resolve } from "node:path";

const cwd = resolve(import.meta.dir, "../..");
const oxlint = resolve(cwd, "node_modules/.bin/oxlint");
const lintArgs = [
  "--format",
  "default",
  "-c",
  "oxlint.config.ts",
  "--type-aware",
  "--deny-warnings",
  "--report-unused-disable-directives-severity",
  "error",
  "--no-ignore",
];

function lint(path: string) {
  const result = Bun.spawnSync([oxlint, ...lintArgs, path], { cwd });
  return { exitCode: result.exitCode, output: result.stdout.toString() + result.stderr.toString() };
}

test("script tooling passes the remaining type-aware rules", () => {
  const result = lint("scripts/host.ts");
  expect(result.exitCode).toBe(0);
});

const rejected = [
  ["unsafe-cast.ts", "anti-slop(no-chained-type-assertions)"],
  ["unsafe-cast.ts", "anti-slop(require-safety-comment-for-type-assertion)"],
  ["floating-promise.ts", "typescript(no-floating-promises)"],
  ["void-promise.ts", "typescript(no-floating-promises)"],
  ["unsafe-assignment.ts", "typescript(no-unsafe-assignment)"],
  ["misused-promise.ts", "typescript(no-misused-promises)"],
  ["module-mock.ts", "anti-slop(no-module-mocking)"],
  ["bun-module-mock.ts", "no-restricted-imports"],
  ["bun-namespace-mock.ts", "no-restricted-imports"],
  ["unused-suppression.ts", "Unused oxlint-disable directive"],
  ["invalid-suppression.ts", "typescript(ban-ts-comment)"],
  ["cross-client-import.ts", "no-restricted-imports"],
  ["empty-safety-comment.ts", "anti-slop(require-safety-comment-for-type-assertion)"],
];

test.each(rejected)("%s rejects %s", (file, rule) => {
  const result = lint(`tests/tooling/negative/${file}`);
  expect(result.exitCode).toBe(1);
  expect(result.output).toContain(rule);
});
