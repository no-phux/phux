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

test("ci.yml routes every event through plan, with no path filter", () => {
  const on = table(CI["on"]);
  expect(Object.keys(on).toSorted()).toEqual([
    "merge_group",
    "pull_request",
    "push",
    "workflow_dispatch",
  ]);
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

/** The `format()` call one event contributes to ci.yml's concurrency group. */
function concurrencyFormat(
  group: string,
  event: "pull_request" | "workflow_dispatch",
): { pattern: string; args: string[] } {
  const match = group.match(
    new RegExp(String.raw`github\.event_name == '${event}' && format\('([^']*)', ([^)]*)\)`),
  );
  if (match === null) throw new Error(`ci.yml concurrency group has no ${event} key: ${group}`);
  return {
    pattern: match[1] ?? "",
    args: (match[2] ?? "").split(",").map((arg) => arg.trim()),
  };
}

/** GitHub `format('{0}-{1}', repo, branch)` with the pattern the workflow uses. */
function concurrencyKey(pattern: string, repo: string, branch: string): string {
  return pattern.replaceAll("{0}", repo).replaceAll("{1}", branch);
}

test("a pull request and a ci dispatch of its branch share one concurrency group", () => {
  const group = str(table(CI["concurrency"])["group"]) ?? "";
  // Push stays the main-push / release-bot SHA key. A merge group still falls
  // through to github.ref, the queue ref that names one entry.
  expect(group).toContain(
    "github.event_name == 'push' && (github.actor == 'phux-release-bot[bot]' && github.sha || 'main-push')",
  );
  expect(group.endsWith(" || github.ref }}")).toBe(true);
  expect(group).not.toContain("merge_group");
  expect(group).not.toContain("pull_request.number");
  expect(table(CI["concurrency"])["cancel-in-progress"]).toBe("${{ github.event_name != 'push' }}");

  const pr = concurrencyFormat(group, "pull_request");
  const dispatch = concurrencyFormat(group, "workflow_dispatch");
  expect(pr.pattern).toBe(dispatch.pattern);
  expect(pr.args).toEqual(["github.event.pull_request.head.repo.full_name", "github.head_ref"]);
  expect(dispatch.args).toEqual(["github.repository", "github.ref_name"]);

  const branch = "feat/onepassword-broker";
  const upstream = concurrencyKey(dispatch.pattern, "no-phux/phux", branch);
  expect(concurrencyKey(pr.pattern, "no-phux/phux", branch)).toBe(upstream);
  expect(concurrencyKey(pr.pattern, "contributor/phux", branch)).not.toBe(upstream);
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
    const called = uses(owner?.[1] ?? {}).replace("./.github/workflows/", "");
    const laneJobs = called === "" ? [owner?.[1] ?? {}] : Object.values(jobs(workflow(called)));
    const commands = laneJobs.flatMap(steps).map(run).join("\n");
    for (const argv of LANE_RECIPES[lane]) {
      const [recipe] = argv;
      expect([lane, recipe, recipe in RECIPES]).toEqual([lane, recipe, true]);
      // A job without just installed runs the recipe's one-line body instead.
      const body = RECIPES[recipe] ?? [];
      const runs =
        commands.includes(`just ${argv.join(" ")}`) ||
        (argv.length === 1 && body.length === 1 && commands.includes(body[0] ?? "\0"));
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

test("pull requests and merge groups write receipts; content publishes after a green push", () => {
  for (const step of steps(job(CI, "ci")).slice(1)) {
    expect(str(step["if"])).toBe(
      "github.event_name == 'pull_request' || github.event_name == 'merge_group'",
    );
  }
  const content = job(CI, "content");
  expect(needs(content)).toContain("ci");
  expect(str(content["if"])).toContain("needs.ci.result == 'success'");
  expect(str(content["if"])).toContain("github.event_name == 'push'");
});

test("both FFI workflows build only on dispatch and tell mobile only about releases", () => {
  // phux-mobile pins a release (release.yml dispatches both with the tag) or,
  // for an override, one commit it dispatches both for. A main push building
  // them cost 45 macOS minutes a time for archives mobile mostly never pinned.
  for (const name of FFI) {
    const document = workflow(name);
    expect(Object.keys(table(document["on"]))).toEqual(["workflow_dispatch"]);
    expect(Object.keys(jobs(document))).not.toContain("changes");
    expect(str(job(document, "notify-mobile")["if"])).toContain("inputs.tag != ''");
  }
  expect(workflowText("release.yml")).toContain(
    "for workflow in ffi-xcframework.yml ffi-android.yml",
  );
});

test("the merge queue gets both required checks", () => {
  // A queue that never sees `ci` and `commitlint` on its commit waits forever.
  for (const name of ["ci.yml", "commitlint.yml"]) {
    const trigger = table(table(workflow(name)["on"])["merge_group"]);
    expect([name, trigger["types"]]).toEqual([name, ["checks_requested"]]);
  }
  const lint = workflowText("commitlint.yml");
  expect(lint).toContain("github.event.merge_group.base_sha");
  expect(lint).toContain("github.event.merge_group.head_sha");
});

test("the Rune lane's iOS job follows the plan, gated on the merge queue variable", () => {
  const rune = job(CI, "rune");
  expect(withInput(rune, "ios")).toBe("${{ needs.plan.outputs.rune_ios == 'true' }}");
  const planStep = steps(job(CI, "plan")).find((step) => run(step).includes("ci-plan"));
  expect(table(planStep?.["env"])["PHUX_MERGE_QUEUE"]).toBe("${{ vars.PHUX_MERGE_QUEUE }}");
  const build = workflow("rune-build.yml");
  const input = table(table(table(table(build["on"])["workflow_call"])["inputs"])["ios"]);
  // rune-client.yml calls it for a tag without inputs: the release keeps iOS.
  expect(input).toMatchObject({ type: "boolean", default: true });
  expect(str(job(build, "ios-native")["if"])).toBe("inputs.ios");
});

test("post-merge workflows follow ci on main pushes only", () => {
  // A merge group's run is on gh-readonly-queue/main/*, never `main`.
  for (const name of ["release.yml", "next.yml", "rune-auto-release.yml"]) {
    const trigger = table(table(workflow(name)["on"])["workflow_run"]);
    expect(trigger["workflows"]).toEqual(["ci"]);
    expect(trigger["branches"]).toEqual(["main"]);
    expect(workflowText(name)).toContain("github.event.workflow_run.event == 'push'");
  }
});
