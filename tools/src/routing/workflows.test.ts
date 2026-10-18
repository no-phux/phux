// ci.yml runs the plan the way plan.ts promises: one job per lane, gated on
// the plan's own output, running the recipes `just check` runs locally.
import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { repoPath } from "../lib/repo.ts";
import { capture } from "../lib/process.ts";
import { list, str, table } from "../lib/value.ts";
import {
  job,
  jobs,
  needs,
  run,
  steps,
  uses,
  withInput,
  workflow,
  workflowText,
} from "../policy/workflows.ts";
import { classifyEvent } from "./detect.ts";
import { LANE_RECIPES, LANES, plan, planOutputs } from "./plan.ts";

const CI = workflow("ci.yml");
const FFI = ["ffi-xcframework.yml", "ffi-android.yml"];
const AFTER_LANES = ["ci", "content", "prune-build-cache"];

/** Recipe name to its one-line bodies, from just's own parser. */
const RECIPES: Record<string, string[]> = Object.fromEntries(
  Object.entries(
    table(
      table(
        JSON.parse(capture(["just", "--dump", "--dump-format", "json"], { cwd: repoPath(".") })),
      )["recipes"],
    ),
  ).map(([name, recipe]) => [
    name,
    list(table(recipe)["body"]).map((line) => list(line).map(String).join("")),
  ]),
);

test("the root justfile holds only imports and default", () => {
  // A product recipe there would classify as cheap and skip its lane.
  const recipes = readFileSync(repoPath("justfile"), "utf8")
    .split("\n")
    .filter((line) => line.trim() !== "" && !/^(import |#|\[|\s|default:)/.test(line))
    .filter((line) => line.includes(":"));
  expect(recipes).toEqual([]);
});

test("ci.yml routes every event through plan, with no path filter or merge queue", () => {
  const on = table(CI["on"]);
  expect(Object.keys(on).toSorted()).toEqual(["pull_request", "push", "workflow_dispatch"]);
  for (const trigger of Object.values(on)) {
    expect(Object.keys(table(trigger)).filter((key) => key.startsWith("paths"))).toEqual([]);
  }
  expect(list(table(on["pull_request"])["types"])).toContain("closed");
  const gate = str(job(CI, "plan")["if"]) ?? "";
  expect(gate).toContain("github.event.action != 'closed'");
  expect(gate).toContain("draft != true");
  const checkout = steps(job(CI, "plan")).find((s) => uses(s).startsWith("actions/checkout@"));
  expect(withInput(checkout ?? {}, "fetch-depth")).toBe(0);
});

test("plan declares every output the planner writes, and jobs read only those", () => {
  const c = classifyEvent("workflow_dispatch", undefined);
  const written = [
    ...Object.keys(planOutputs(plan(c, "pull_request"), c)),
    "validated",
    "receipt",
    "receipt_name",
  ];
  const declared = Object.keys(table(job(CI, "plan")["outputs"]));
  const read = [...workflowText("ci.yml").matchAll(/needs\.plan\.outputs\.(\w+)/g)].map(
    (m) => m[1],
  );
  expect(declared.filter((key) => !written.includes(key))).toEqual([]);
  expect(read.filter((key) => !declared.includes(key ?? ""))).toEqual([]);
});

test("each lane is one job gated on its plan output that runs the lane's recipes", () => {
  for (const lane of LANES) {
    const gated = Object.entries(jobs(CI)).filter(([, lane_job]) =>
      (str(lane_job["if"]) ?? "").includes(`needs.plan.outputs.${lane} == 'true'`),
    );
    const owner = gated.find(([id]) => id !== "test-build");
    expect([lane, owner === undefined]).toEqual([lane, false]);
    const commands = steps(owner?.[1] ?? {})
      .map(run)
      .join("\n");
    for (const recipe of LANE_RECIPES[lane]) {
      expect([lane, recipe, recipe in RECIPES]).toEqual([lane, recipe, true]);
      // A job without just installed runs the recipe's one-line body instead.
      const body = RECIPES[recipe] ?? [];
      const runs =
        new RegExp(`\\bjust ${recipe}\\b`).test(commands) ||
        (body.length === 1 && commands.includes(body[0] ?? "\0"));
      expect([lane, recipe, runs]).toEqual([lane, recipe, true]);
    }
  }
});

test("the ci aggregate needs every lane and accepts only success or a skip", () => {
  const aggregate = job(CI, "ci");
  const lanes = Object.keys(jobs(CI)).filter((id) => !AFTER_LANES.includes(id));
  expect(needs(aggregate).toSorted()).toEqual(lanes.toSorted());
  const script = run(steps(aggregate)[0] ?? {});
  const baseline = Object.fromEntries(lanes.map((id) => [id, { result: "skipped" }]));
  baseline["plan"] = { result: "success" };
  for (const [id, result, code] of [
    ["lint", "skipped", 0],
    ["lint", "success", 0],
    ["integrations", "failure", 1],
    ["tests", "cancelled", 1],
    ["guards", "failure", 1],
    ["plan", "skipped", 1],
    ["plan", "failure", 1],
  ] as const) {
    const results = JSON.stringify({ ...baseline, [id]: { result } });
    const exit = Bun.spawnSync(["bash", "-euo", "pipefail", "-c", script], {
      env: { ...process.env, RESULTS: results },
      stdout: "ignore",
    }).exitCode;
    expect([id, result, exit]).toEqual([id, result, code]);
  }
});

test("only pull requests write receipts; content publishes after a green push", () => {
  for (const step of steps(job(CI, "ci")).slice(1)) {
    expect(str(step["if"])).toBe("github.event_name == 'pull_request'");
  }
  const content = job(CI, "content");
  expect(needs(content)).toContain("ci");
  expect(str(content["if"])).toContain("needs.ci.result == 'success'");
  expect(str(content["if"])).toContain("github.event_name == 'push'");
});

test("both FFI workflows build on mobile_ffi_needed or a failed classification", () => {
  // phux-mobile pins one revision and fetches both artifacts at it, by
  // push run of these two file names.
  for (const name of FFI) {
    const document = workflow(name);
    expect(Object.keys(table(document["on"]))).toContain("push");
    const condition = str(job(document, "build")["if"]) ?? "";
    expect(condition).toContain("needs.changes.result == 'failure'");
    expect(condition).toContain("needs.changes.outputs.mobile_ffi_needed == 'true'");
    const detect = steps(job(document, "changes"));
    expect(detect.some((step) => run(step).includes("bun tools/main.ts detect-changes"))).toBe(
      true,
    );
    const checkout = detect.find((step) => uses(step).startsWith("actions/checkout@"));
    expect(withInput(checkout ?? {}, "fetch-depth")).toBe(0);
  }
});

test("post-merge workflows follow ci on main pushes only", () => {
  for (const name of ["release.yml", "next.yml"]) {
    const trigger = table(table(workflow(name)["on"])["workflow_run"]);
    expect(trigger["workflows"]).toEqual(["ci"]);
    expect(trigger["branches"]).toEqual(["main"]);
    expect(workflowText(name)).toContain("github.event.workflow_run.event == 'push'");
  }
});
