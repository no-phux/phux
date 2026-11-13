// ci.yml runs the plan the way plan.ts promises: one job per lane, gated on
// the plan's own output, running the recipes `just check` runs locally.
import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { repoPath } from "../lib/repo.ts";
import { capture } from "../lib/process.ts";
import { list, str, strings, table, type Table } from "../lib/value.ts";
import {
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

// Every macos-* / xcode-* leg the workflows actually schedule, one line per
// way it enters the five-slot pool. Reusable workflows count through each
// caller, because workflow_call does not start a run.
const MACOS_BUDGET = [
  "cache-warm.yml rune → rune-build.yml build macos-arm64 on macos-26 | schedule 17 6 * * *, workflow_dispatch",
  "cache-warm.yml rune → rune-build.yml ios-native (ios-native-simulator) on macos-26 | schedule 17 6 * * *, workflow_dispatch",
  "ci.yml rune → rune-build.yml build macos-arm64 on macos-26 | merge_group, pull_request, push branches main, workflow_dispatch",
  "ci.yml rune → rune-build.yml ios-native (ios-native-simulator) on macos-26 | merge_group, pull_request, push branches main, workflow_dispatch",
  "ffi-xcframework.yml build on xcode-27 | workflow_dispatch",
  "first-run-timing.yml macos on macos-26 | pull_request, push branches main, workflow_dispatch",
  "next.yml cli → build-cli.yml build aarch64-apple-darwin on xcode-27 | workflow_dispatch, workflow_run ci",
  "release.yml cli-build → build-cli.yml build aarch64-apple-darwin on xcode-27 | schedule 5 15 * * *, workflow_dispatch, workflow_run ci",
  "rune-client.yml build → rune-build.yml build macos-arm64 on macos-26 | push tags rune-v*",
  "rune-client.yml build → rune-build.yml ios-native (ios-native-simulator) on macos-26 | push tags rune-v*",
];

test("macOS jobs stay on the budget", () => {
  // The macos-26 and xcode-27 pool has five slots. It saturated on 13 Nov
  // 2026. A job whose runs-on resolves to macos-* or xcode-* and is not
  // named here fails until someone adds a budget entry.
  const found = macJobs().map(budgetLine).toSorted();
  const extra = found.filter((line) => !MACOS_BUDGET.includes(line));
  expect(extra.map((line) => `add a budget entry for ${line}`)).toEqual([]);
  expect(found).toEqual([...MACOS_BUDGET].toSorted());
});

test("tag, release, and workflow_run runs are not cancelled", () => {
  // Cancelling one of these drops that commit's publish. false, or no
  // cancel-in-progress, keeps the run. true fails, and so does an expression,
  // even one that would evaluate to false.
  const held = workflowNames().filter(releaseRun);
  for (const name of [
    "agent-integration-release.yml",
    "ffi-android.yml",
    "ffi-xcframework.yml",
    "lts.yml",
    "next.yml",
    "phux-auto-release.yml",
    "release.yml",
    "rune-auto-release.yml",
    "rune-client.yml",
  ]) {
    expect(held).toContain(name);
  }
  for (const name of held) {
    const value = table(workflow(name)["concurrency"])["cancel-in-progress"];
    const problem = cancelIsOff(value)
      ? ""
      : `${name} cancel-in-progress is ${shown(value)}; a true or expression value fails`;
    expect(problem).toBe("");
  }
  expect(cancelIsOff(false)).toBe(true);
  expect(cancelIsOff(undefined)).toBe(true);
  expect(cancelIsOff(true)).toBe(false);
  expect(cancelIsOff("${{ false }}")).toBe(false);
  expect(cancelIsOff("${{ github.event_name != 'push' }}")).toBe(false);
});

test("pull request ci stays latest-wins", () => {
  // Literal true cancels the older pull request. So does
  // `${{ github.event_name != 'push' }}`: a pull_request is not a push.
  const value = table(workflow("ci.yml")["concurrency"])["cancel-in-progress"];
  expect(events(workflow("ci.yml"))["pull_request"]).not.toBeUndefined();
  expect(cancelsOnPullRequest(value)).toBe(true);
  expect(cancelsOnPullRequest(true)).toBe(true);
  expect(cancelsOnPullRequest(false)).toBe(false);
  expect(cancelsOnPullRequest("${{ github.event_name != 'push' }}")).toBe(true);
  expect(cancelsOnPullRequest("${{github.event_name!='push'}}")).toBe(true);
  expect(cancelsOnPullRequest('${{ github.event_name != "push" }}')).toBe(true);
  expect(cancelsOnPullRequest("${{ github.event_name == 'pull_request' }}")).toBe(true);
  expect(cancelsOnPullRequest("${{ github.event_name == 'push' }}")).toBe(false);
  expect(cancelsOnPullRequest("${{ github.event_name != 'pull_request' }}")).toBe(false);
});

const APPLE_RUNNER = /^(?:macos|xcode)-/;

type AppleLeg = { job: string; leg?: string; name?: string; runsOn: string };

type MacBudget = {
  workflow: string;
  job: string;
  calls?: string;
  calledJob?: string;
  leg?: string;
  name?: string;
  runsOn: string;
  triggers: string[];
};

function macJobs(): MacBudget[] {
  const scheduled: MacBudget[] = [];
  for (const file of workflowNames()) {
    if (callOnly(file)) continue;
    const triggers = triggerLabels(file);
    for (const [id, lane] of Object.entries(jobs(workflow(file)))) {
      const called = calledWorkflow(uses(lane));
      if (called !== undefined) {
        for (const leg of directApple(called, new Set())) {
          scheduled.push(budgetEntry(file, id, leg, triggers));
        }
        continue;
      }
      for (const leg of appleLegs(id, lane)) {
        scheduled.push(budgetEntry(file, id, { ...leg, definedIn: file }, triggers));
      }
    }
  }
  const covered = new Set(scheduled.map(legKey));
  for (const file of workflowNames()) {
    for (const [id, lane] of Object.entries(jobs(workflow(file)))) {
      if (calledWorkflow(uses(lane)) !== undefined) continue;
      for (const leg of appleLegs(id, lane)) {
        const key = `${file} ${leg.job} ${leg.leg ?? ""} ${leg.runsOn}`;
        if (covered.has(key)) continue;
        scheduled.push(budgetEntry(file, id, { ...leg, definedIn: file }, triggerLabels(file)));
      }
    }
  }
  return scheduled;
}

function budgetEntry(
  file: string,
  callerJob: string,
  leg: AppleLeg & { definedIn: string },
  triggers: string[],
): MacBudget {
  const entry: MacBudget = { workflow: file, job: callerJob, runsOn: leg.runsOn, triggers };
  if (leg.definedIn !== file) {
    entry.calls = leg.definedIn;
    entry.calledJob = leg.job;
  }
  if (leg.leg !== undefined) entry.leg = leg.leg;
  if (leg.name !== undefined) entry.name = leg.name;
  return entry;
}

function budgetLine(entry: MacBudget): string {
  const where =
    entry.calls !== undefined
      ? `${entry.workflow} ${entry.job} → ${entry.calls} ${entry.calledJob ?? ""}`
      : `${entry.workflow} ${entry.job}`;
  const leg = entry.leg !== undefined ? ` ${entry.leg}` : "";
  const name = entry.name !== undefined ? ` (${entry.name})` : "";
  return `${where}${leg}${name} on ${entry.runsOn} | ${entry.triggers.join(", ")}`;
}

function legKey(entry: MacBudget): string {
  const file = entry.calls ?? entry.workflow;
  const id = entry.calledJob ?? entry.job;
  return `${file} ${id} ${entry.leg ?? ""} ${entry.runsOn}`;
}

/** Apple legs defined in `file`, following a called workflow to the job that owns runs-on. */
function directApple(file: string, seen: Set<string>): Array<AppleLeg & { definedIn: string }> {
  if (seen.has(file)) return [];
  seen.add(file);
  const legs: Array<AppleLeg & { definedIn: string }> = [];
  for (const [id, lane] of Object.entries(jobs(workflow(file)))) {
    const called = calledWorkflow(uses(lane));
    if (called !== undefined) {
      legs.push(...directApple(called, seen));
      continue;
    }
    for (const leg of appleLegs(id, lane)) legs.push({ ...leg, definedIn: file });
  }
  return legs;
}

function appleLegs(jobId: string, lane: Table): AppleLeg[] {
  const runsOn = lane["runs-on"];
  if (runnerLabels(runsOn).some((label) => label.includes("matrix.")))
    return matrixLegs(jobId, lane);
  return runnerLabels(runsOn)
    .filter(isAppleRunner)
    .map((label) => namedLeg(jobId, lane, label));
}

function matrixLegs(jobId: string, lane: Table): AppleLeg[] {
  const matrix = table(table(lane["strategy"])["matrix"]);
  const legs: AppleLeg[] = [];
  const seen = new Set<string>();
  const add = (runsOn: string, leg: string | undefined) => {
    if (!isAppleRunner(runsOn)) return;
    const key = `${runsOn}\0${leg ?? ""}`;
    if (seen.has(key)) return;
    seen.add(key);
    const entry = namedLeg(jobId, lane, runsOn);
    if (leg !== undefined && leg !== jobId) entry.leg = leg;
    legs.push(entry);
  };
  for (const label of asStrings(matrix["os"])) add(label, undefined);
  for (const row of list(matrix["include"]).map(table)) {
    const os = str(row["os"]);
    if (os === undefined) continue;
    add(os, str(row["name"]) ?? str(row["target"]));
  }
  return legs;
}

function namedLeg(jobId: string, lane: Table, runsOn: string): AppleLeg {
  const entry: AppleLeg = { job: jobId, runsOn };
  const name = str(lane["name"]);
  if (name !== undefined && !name.includes("${{") && !/\s/.test(name) && name !== jobId) {
    entry.name = name;
  }
  return entry;
}

function isAppleRunner(label: string): boolean {
  return APPLE_RUNNER.test(label);
}

function runnerLabels(value: unknown): string[] {
  if (typeof value === "string") return [value.trim()];
  if (Array.isArray(value)) return value.flatMap(runnerLabels);
  return [];
}

function calledWorkflow(ref: string): string | undefined {
  return /^\.\/\.github\/workflows\/([^/]+\.ya?ml)$/.exec(ref)?.[1];
}

function callOnly(file: string): boolean {
  const keys = Object.keys(events(workflow(file)));
  return keys.length === 1 && keys[0] === "workflow_call";
}

function triggerLabels(file: string): string[] {
  const on = events(workflow(file));
  return Object.keys(on)
    .map((event) => formatTrigger(event, on[event]))
    .toSorted();
}

function formatTrigger(event: string, spec: unknown): string {
  if (event === "push") {
    const push = table(spec);
    const parts: string[] = [];
    const branches = asStrings(push["branches"]);
    const tags = asStrings(push["tags"]);
    if (branches.length > 0) parts.push(`branches ${branches.join(" ")}`);
    if (tags.length > 0) parts.push(`tags ${tags.join(" ")}`);
    return parts.length > 0 ? `push ${parts.join(" ")}` : "push";
  }
  if (event === "schedule") {
    const crons = list(spec)
      .map((item) => str(table(item)["cron"]))
      .filter((cron): cron is string => cron !== undefined);
    return crons.length > 0 ? `schedule ${crons.join(" ")}` : "schedule";
  }
  if (event === "workflow_run") {
    const watched = asStrings(table(spec)["workflows"]);
    return watched.length > 0 ? `workflow_run ${watched.join(" ")}` : "workflow_run";
  }
  if (event === "release") {
    const types = asStrings(table(spec)["types"]);
    return types.length > 0 ? `release ${types.join(" ")}` : "release";
  }
  return event;
}

function events(document: Table): Table {
  const on = document["on"];
  if (typeof on === "string") return { [on]: {} };
  if (Array.isArray(on)) return Object.fromEntries(on.map((event) => [String(event), {}]));
  return table(on);
}

function asStrings(value: unknown): string[] {
  return typeof value === "string" ? [value] : strings(value);
}

function releaseRun(name: string): boolean {
  // Dispatch-only publishers (lts, the auto-release pair, agent integrations,
  // ffi-*) never cancel either. Anything else triggered by tags, a release,
  // or workflow_run joins them.
  const base = name.replace(/\.ya?ml$/, "");
  const named =
    base === "release" ||
    base === "next" ||
    base === "rune-client" ||
    base === "lts" ||
    base === "agent-integration-release" ||
    base.startsWith("ffi-") ||
    base.includes("auto-release");
  if (named) return true;
  const on = events(workflow(name));
  if ("workflow_run" in on || "release" in on) return true;
  return asStrings(table(on["push"])["tags"]).length > 0;
}

function cancelIsOff(value: unknown): boolean {
  return value === false || value === undefined;
}

function shown(value: unknown): string {
  if (value === undefined) return "absent";
  return typeof value === "string" ? value : JSON.stringify(value);
}

function cancelsOnPullRequest(value: unknown): boolean {
  if (value === true) return true;
  if (typeof value !== "string") return false;
  const expr = /^\s*\$\{\{\s*([\s\S]*?)\s*\}\}\s*$/.exec(value)?.[1];
  if (expr === undefined) return false;
  try {
    return evaluate(tokenize(expr), "pull_request");
  } catch {
    return false;
  }
}

function tokenize(expr: string): string[] {
  const tokens: string[] = [];
  let index = 0;
  while (index < expr.length) {
    const ch = expr[index];
    if (ch === undefined) break;
    if (ch === " " || ch === "\n" || ch === "\t") {
      index++;
      continue;
    }
    if (
      expr.startsWith("&&", index) ||
      expr.startsWith("||", index) ||
      expr.startsWith("==", index) ||
      expr.startsWith("!=", index)
    ) {
      tokens.push(expr.slice(index, index + 2));
      index += 2;
      continue;
    }
    if (ch === "!" || ch === "(" || ch === ")") {
      tokens.push(ch);
      index++;
      continue;
    }
    if (ch === "'" || ch === '"') {
      const end = expr.indexOf(ch, index + 1);
      if (end < 0) throw new Error("unterminated string");
      tokens.push(expr.slice(index, end + 1));
      index = end + 1;
      continue;
    }
    const ident = /^[A-Za-z_][\w.]*/.exec(expr.slice(index))?.[0];
    if (ident === undefined) throw new Error(`unexpected ${ch}`);
    tokens.push(ident);
    index += ident.length;
  }
  return tokens;
}

function evaluate(tokens: string[], eventName: string): boolean {
  const at = { i: 0 };
  const value = parseOr(tokens, at, eventName);
  if (at.i !== tokens.length) throw new Error("trailing");
  return value;
}

function parseOr(tokens: string[], at: { i: number }, eventName: string): boolean {
  let left = parseAnd(tokens, at, eventName);
  while (tokens[at.i] === "||") {
    at.i++;
    const right = parseAnd(tokens, at, eventName);
    left = left || right;
  }
  return left;
}

function parseAnd(tokens: string[], at: { i: number }, eventName: string): boolean {
  let left = parseUnary(tokens, at, eventName);
  while (tokens[at.i] === "&&") {
    at.i++;
    const right = parseUnary(tokens, at, eventName);
    left = left && right;
  }
  return left;
}

function parseUnary(tokens: string[], at: { i: number }, eventName: string): boolean {
  if (tokens[at.i] === "!") {
    at.i++;
    return !parseUnary(tokens, at, eventName);
  }
  return parsePrimary(tokens, at, eventName);
}

function parsePrimary(tokens: string[], at: { i: number }, eventName: string): boolean {
  const token = tokens[at.i];
  if (token === "(") {
    at.i++;
    const value = parseOr(tokens, at, eventName);
    if (tokens[at.i] !== ")") throw new Error("paren");
    at.i++;
    return value;
  }
  if (token === "true" || token === "false") {
    at.i++;
    return token === "true";
  }
  if (token !== "github.event_name") throw new Error(token ?? "empty");
  at.i++;
  const op = tokens[at.i];
  if (op !== "==" && op !== "!=") throw new Error(op ?? "empty");
  at.i++;
  const raw = tokens[at.i];
  if (raw === undefined || (raw[0] !== "'" && raw[0] !== '"')) throw new Error("string");
  at.i++;
  const literal = raw.slice(1, -1);
  return op === "==" ? eventName === literal : eventName !== literal;
}
