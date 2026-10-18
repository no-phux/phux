// The workflows consume the classifier the way its outputs promise.
import { expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";

const read = (path: string) => readFileSync(join(ROOT, path), "utf8");
const ci = read(".github/workflows/ci.yml");

test("the root justfile holds only imports and default", () => {
  // A product recipe there would classify as cheap and skip its lane.
  const recipes = read("justfile")
    .split("\n")
    .filter((line) => line.trim() !== "" && !/^(import |#|\[|\s|default:)/.test(line))
    .filter((line) => line.includes(":"));
  expect(recipes).toEqual([]);
});

test("cheap changes allocate no Rust setup", () => {
  const cheap = ci.split("  workflow-gate:")[1]?.split("  check:")[0] ?? "";
  expect(cheap).not.toContain("nix develop");
  expect(cheap).not.toContain("setup-rust-lane");
  expect(cheap).toContain("nix shell --inputs-from .");
  expect(cheap).toContain("if: needs.changes.outputs.integrations_needed == 'true'");
  // check, the test build, its three run jobs, and uniffi.
  expect(ci.split("if: needs.changes.outputs.phux_needed == 'true'").length - 1).toBe(6);
});

test("the PR unit lane uses the classifier's rdeps filterset", () => {
  const recipes = ["justfile", ...readdirSync(join(ROOT, "just")).map((name) => `just/${name}`)]
    .map(read)
    .join("\n");
  expect(ci).toContain("PHUX_NEXTEST_FILTERSET:");
  expect(ci).toContain("github.event_name != 'push'");
  expect(ci).toContain("needs.changes.outputs.test_filterset");
  expect(recipes).toContain("PHUX_NEXTEST_FILTERSET");
  expect(recipes).toContain("cargo nextest run --workspace");
  expect(recipes).toContain('PHUX_UNIT_MODE:-workspace}" == "narrow"');
});

test("lanes share one detection and have no outer path filter", () => {
  for (const name of ["ci", "native-setup", "web-check", "desktop-test"]) {
    const workflow = read(`.github/workflows/${name}.yml`);
    const triggers = workflow.split("concurrency:")[0] ?? "";
    expect([name, triggers.includes("paths:")]).toEqual([name, false]);
    for (const needle of [
      "merge_group:",
      "fetch-depth: 0",
      "uses: ./.github/actions/classify-changes",
      "cancel-in-progress: true",
    ]) {
      expect([name, workflow.includes(needle)]).toEqual([name, true]);
    }
  }
});

test("the archived cockpit has no workflows", () => {
  expect(
    readdirSync(join(ROOT, ".github/workflows")).filter((name) => name.startsWith("cockpit-")),
  ).toEqual([]);
});

test("the ci aggregate rejects failed and cancelled lanes", () => {
  const script = ci.split("node -e '")[1]?.split("\n          '\n")[0] ?? "";
  expect(script).not.toBe("");
  const baseline: Record<string, { result: string }> = {
    check: { result: "skipped" },
    "test-build": { result: "skipped" },
    test: { result: "skipped" },
    integrations: { result: "skipped" },
    changes: { result: "success" },
    "workflow-gate": { result: "success" },
  };
  const cases: [string, string, number][] = [
    ["check", "skipped", 0],
    ["integrations", "success", 0],
    ["integrations", "failure", 1],
    ["test", "cancelled", 1],
    ["test-build", "failure", 1],
    ["test-build", "skipped", 0],
    ["workflow-gate", "failure", 1],
    ["workflow-gate", "skipped", 1],
    ["changes", "skipped", 1],
    ["changes", "failure", 1],
  ];
  for (const [job, result, code] of cases) {
    const needs = { ...baseline, [job]: { result } };
    const run = Bun.spawnSync([process.execPath, "-e", script], {
      env: { ...process.env, RESULTS: JSON.stringify(needs) },
      stdout: "ignore",
      stderr: "ignore",
    });
    expect([job, result, run.exitCode]).toEqual([job, result, code]);
  }
});

test("both FFI workflows build on the one mobile_ffi_needed output", () => {
  // phux-mobile pins one revision and fetches both artifacts at it.
  for (const name of ["ffi-xcframework", "ffi-android"]) {
    const workflow = read(`.github/workflows/${name}.yml`);
    const triggers = workflow.split("permissions:")[0] ?? "";
    expect([name, triggers.includes("paths:")]).toEqual([name, false]);
    expect([name, workflow.includes("needs.changes.outputs.mobile_ffi_needed == 'true'")]).toEqual([
      name,
      true,
    ]);
  }
});

test("a failed classification still builds both FFI artifacts", () => {
  for (const name of ["ffi-xcframework", "ffi-android"]) {
    expect(read(`.github/workflows/${name}.yml`)).toContain("needs.changes.result == 'failure'");
  }
});
