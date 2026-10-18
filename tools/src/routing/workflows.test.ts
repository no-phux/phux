// The workflows consume the classifier the way its outputs promise.
import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { repoPath } from "../lib/repo.ts";
import { str, table } from "../lib/value.ts";
import {
  action,
  job,
  jobs,
  needs,
  run,
  steps,
  uses,
  withInput,
  workflow,
  workflowNames,
  workflowText,
} from "../policy/workflows.ts";

const CLASSIFY = "./.github/actions/classify-changes";
const OUTPUTS = new Set(Object.keys(table(action("classify-changes")["outputs"])));
const LANES = ["ci.yml", "native-setup.yml", "web-check.yml", "desktop-test.yml"];
const FFI = ["ffi-xcframework.yml", "ffi-android.yml"];
const classified = workflowNames().filter((name) => workflowText(name).includes(CLASSIFY));

test("the root justfile holds only imports and default", () => {
  // A product recipe there would classify as cheap and skip its lane.
  const recipes = readFileSync(repoPath("justfile"), "utf8")
    .split("\n")
    .filter((line) => line.trim() !== "" && !/^(import |#|\[|\s|default:)/.test(line))
    .filter((line) => line.includes(":"));
  expect(recipes).toEqual([]);
});

/** Output names `text` reads as `<prefix>.outputs.<name>`. */
function referenced(text: string, prefix: string): string[] {
  return [...text.matchAll(new RegExp(`${prefix}\\.outputs\\.(\\w+)`, "g"))].map(
    (match) => match[1] ?? "",
  );
}

test("classified workflows read real outputs, from full history, with no path filter", () => {
  expect(classified.toSorted()).toEqual([...LANES, ...FFI].toSorted());
  const consumed = new Set<string>();
  for (const name of classified) {
    const document = workflow(name);
    const text = workflowText(name);
    // An outer path filter would skip the fail-closed classifier.
    const filters = Object.values(table(document["on"])).flatMap((trigger) =>
      Object.keys(table(trigger)),
    );
    expect([name, filters.filter((key) => key.startsWith("paths"))]).toEqual([name, []]);
    const [id, changes = {}] =
      Object.entries(jobs(document)).find(([, lane]) =>
        steps(lane).some((s) => uses(s) === CLASSIFY),
      ) ?? [];
    const checkout = steps(changes).find((step) => uses(step).startsWith("actions/checkout@"));
    expect([name, withInput(checkout ?? {}, "fetch-depth")]).toEqual([name, 0]);
    // A misspelled output reads as empty and silently skips its lane.
    const step = steps(changes).find((candidate) => uses(candidate) === CLASSIFY)?.["id"];
    const read = referenced(text, `steps\\.${String(step)}`);
    for (const key of read) consumed.add(key);
    expect([name, read.filter((key) => !OUTPUTS.has(key))]).toEqual([name, []]);
    const declared = Object.keys(table(changes["outputs"]));
    expect([
      name,
      referenced(text, `needs\\.${id}`).filter((key) => !declared.includes(key)),
    ]).toEqual([name, []]);
  }
  // Every output has a reader.
  expect([...OUTPUTS].filter((key) => !consumed.has(key))).toEqual([]);
});

test("validation lanes run on merge groups, PRs latest-wins, desktop-test one main run", () => {
  for (const name of LANES) {
    const document = workflow(name);
    expect([name, "merge_group" in table(document["on"])]).toEqual([name, true]);
    const cancel = name === "desktop-test.yml" ? "${{ github.ref != 'refs/heads/main' }}" : true;
    expect([name, table(document["concurrency"])["cancel-in-progress"]]).toEqual([name, cancel]);
  }
  // Per-SHA main runs stacked xcode-27 jobs ahead of the release lanes.
  expect(table(workflow("desktop-test.yml")["concurrency"])["group"]).toBe(
    "mini-v1-desktop-test-${{ github.ref }}",
  );
});

test("cheap changes allocate no Rust setup", () => {
  const gate = steps(job(workflow("ci.yml"), "workflow-gate"));
  expect(
    gate.some(
      (step) => uses(step).includes("setup-rust-lane") || run(step).includes("nix develop"),
    ),
  ).toBe(false);
});

test("the ci aggregate needs every lane and rejects failed and cancelled ones", () => {
  const ci = workflow("ci.yml");
  const aggregate = job(ci, "ci");
  // prune-build-cache is main-only cleanup that runs after the lanes.
  const lanes = Object.keys(jobs(ci)).filter((name) => !["ci", "prune-build-cache"].includes(name));
  expect(needs(aggregate).toSorted()).toEqual(lanes.toSorted());
  const script =
    run(steps(aggregate)[0] ?? {})
      .split("node -e '")[1]
      ?.split("\n'")[0] ?? "";
  const baseline = {
    ...Object.fromEntries(lanes.map((lane) => [lane, "skipped"])),
    changes: "success",
    "workflow-gate": "success",
  };
  for (const [lane, result, code] of [
    ["check", "skipped", 0],
    ["integrations", "success", 0],
    ["integrations", "failure", 1],
    ["test", "cancelled", 1],
    ["workflow-gate", "failure", 1],
    ["workflow-gate", "skipped", 1],
    ["changes", "skipped", 1],
    ["changes", "failure", 1],
  ] as const) {
    const results = Object.entries({ ...baseline, [lane]: result }).map(([job, value]) => [
      job,
      { result: value },
    ]);
    const exit = Bun.spawnSync([process.execPath, "-e", script], {
      env: { ...process.env, RESULTS: JSON.stringify(Object.fromEntries(results)) },
    }).exitCode;
    expect([lane, result, exit]).toEqual([lane, result, code]);
  }
});

test("both FFI workflows build on mobile_ffi_needed or a failed classification", () => {
  // phux-mobile pins one revision and fetches both artifacts at it.
  for (const name of FFI) {
    const condition = str(job(workflow(name), "build")["if"]) ?? "";
    expect(condition).toContain("needs.changes.result == 'failure'");
    expect(condition).toContain("needs.changes.outputs.mobile_ffi_needed == 'true'");
  }
});
