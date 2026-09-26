import { expect, test } from "bun:test";
import { resolve } from "node:path";
import solidPlugin from "@gpuix/solid/bun-plugin";

const cwd = resolve(import.meta.dir, "../..");
const oxlint = resolve(cwd, "node_modules/.bin/oxlint");
const lintArgs = [
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

test("native JSX, GPUIX callbacks, Bun types and explicit unknown parsing pass", () => {
  const result = lint("tests/tooling/positive");
  expect(result.output).toContain("0 warnings and 0 errors");
  expect(result.exitCode).toBe(0);
});

const rejected = [
  ["reactivity.tsx", "solid(reactivity)"],
  ["destructure.tsx", "solid(no-destructure)"],
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
  ["forbidden-import.ts", "no-restricted-imports"],
  ["cross-client-import.ts", "no-restricted-imports"],
  ["transport-import.ts", "import(no-nodejs-modules)"],
  ["services/transport-import.ts", "import(no-nodejs-modules)"],
  ["dom-deep-import.ts", "no-restricted-imports"],
  ["dom-html-import.ts", "no-restricted-imports"],
  ["empty-safety-comment.ts", "anti-slop(require-safety-comment-for-type-assertion)"],
];

test.each(rejected)("%s rejects %s", (file, rule) => {
  const result = lint(`tests/tooling/negative/${file}`);
  expect(result.exitCode).toBe(1);
  expect(result.output).toContain(rule);
});

test("TS7 rejects a nonexistent native event property", () => {
  const result = Bun.spawnSync(
    [
      resolve(cwd, "node_modules/.bin/tsc"),
      "--noEmit",
      "-p",
      "tests/tooling/negative/tsconfig.native.json",
    ],
    { cwd },
  );
  expect(result.exitCode).not.toBe(0);
  expect(result.stdout.toString()).toContain("TS2339");
  expect(result.stdout.toString()).toContain("notAGpuixEventProperty");
});

test("the GPUIX universal plugin compiles native JSX without a DOM renderer", async () => {
  const result = await Bun.build({
    entrypoints: [resolve(cwd, "tests/tooling/positive/native.tsx")],
    target: "bun",
    packages: "external",
    plugins: [solidPlugin],
  });
  expect(result.success).toBe(true);
  const artifact = result.outputs[0];
  if (!artifact) throw new Error("Solid build returned no artifact");
  const source = await artifact.text();
  expect(source).toContain("@gpuix/solid");
  expect(source).toContain("createElement");
  expect(source).not.toContain("solid-js/web");
  expect(source).not.toContain("jsx-runtime");
});
