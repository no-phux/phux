// macOS runner budget and the cancel-in-progress guards for release runs.
// Split from workflows.test.ts so that file stays under the token ratchet.
import { expect, test } from "bun:test";
import { list, str, strings, table, type Table } from "../lib/value.ts";
import { jobs, uses, workflow, workflowNames } from "../policy/workflows.ts";

// One line per way a macos-* or xcode-* leg enters the five-slot pool.
// workflow_call does not start a run, so reusable workflows count through each caller.
const MACOS_BUDGET = [
  "cache-warm.yml rune → rune-build.yml macos (macos-arm64) on macos-26 | schedule 17 6 * * *, workflow_dispatch",
  "cache-warm.yml rune → rune-build.yml ios-native (ios-native-simulator) on macos-26 | schedule 17 6 * * *, workflow_dispatch",
  "ci.yml rune-macos → rune-build.yml macos (macos-arm64) on macos-26 | merge_group",
  "ci.yml rune-macos → rune-build.yml ios-native (ios-native-simulator) on macos-26 | merge_group",
  "ffi-xcframework.yml build on xcode-27 | workflow_dispatch",
  "first-run-timing.yml macos on macos-26 | pull_request, push branches main, workflow_dispatch",
  "next.yml cli → build-cli.yml build aarch64-apple-darwin on xcode-27 | workflow_dispatch, workflow_run ci",
  "release.yml cli-build → build-cli.yml build aarch64-apple-darwin on xcode-27 | schedule 5 15 * * *, workflow_dispatch, workflow_run ci rune-macos",
  "rune-client.yml build → rune-build.yml macos (macos-arm64) on macos-26 | push tags rune-v*",
  "rune-client.yml build → rune-build.yml ios-native (ios-native-simulator) on macos-26 | push tags rune-v*",
  "rune-macos.yml build → rune-build.yml macos (macos-arm64) on macos-26 | push branches main, schedule 47 11 * * *, workflow_dispatch",
  "rune-macos.yml build → rune-build.yml ios-native (ios-native-simulator) on macos-26 | push branches main, schedule 47 11 * * *, workflow_dispatch",
];

test("macOS jobs stay on the budget", () => {
  // The five-slot macos-26/xcode-27 pool saturated on 13 Nov 2026.
  const found = macJobs().map(budgetLine).toSorted();
  const extra = found.filter((line) => !MACOS_BUDGET.includes(line));
  expect(extra.map((line) => `add a budget entry for ${line}`)).toEqual([]);
  expect(found).toEqual([...MACOS_BUDGET].toSorted());
});

test("tag, release, and workflow_run runs are not cancelled", () => {
  // Cancelling one drops that commit's publish. true, or any expression, fails.
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
  // Literal true, and `${{ github.event_name != 'push' }}`: a pull_request is not a push.
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
        for (const leg of directApple(called, new Set(), calledHosts(lane))) {
          scheduled.push(budgetEntry(file, id, leg, poolTriggers(lane, triggers)));
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

function calledHosts(lane: Table): "all" | "linux" | "macos" {
  const value = str(table(lane["with"])["hosts"]);
  if (value === "linux" || value === "macos") return value;
  return "all";
}

/** A caller that refuses pull_request does not put that event in the pool. */
function poolTriggers(lane: Table, triggers: string[]): string[] {
  const gate = str(lane["if"]) ?? "";
  if (gate === "github.event_name == 'merge_group'") {
    return triggers.filter((label) => label === "merge_group");
  }
  if (gate.includes("github.event_name != 'pull_request'")) {
    return triggers.filter((label) => label !== "pull_request");
  }
  return triggers;
}

function directApple(
  file: string,
  seen: Set<string>,
  hosts: "all" | "linux" | "macos" = "all",
): Array<AppleLeg & { definedIn: string }> {
  // linux calls of rune-build.yml do not take an Apple runner.
  if (hosts === "linux" || seen.has(file)) return [];
  seen.add(file);
  const legs: Array<AppleLeg & { definedIn: string }> = [];
  for (const [id, lane] of Object.entries(jobs(workflow(file)))) {
    const called = calledWorkflow(uses(lane));
    if (called !== undefined) {
      legs.push(...directApple(called, seen, calledHosts(lane)));
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
    .filter((event) => event !== "workflow_call")
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
  // Dispatch-only publishers join the tag, release, and workflow_run runs.
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
  const expr = value.replace(/\s+/g, "");
  return (
    expr === "${{github.event_name!='push'}}" ||
    expr === '${{github.event_name!="push"}}' ||
    expr === "${{github.event_name=='pull_request'}}" ||
    expr === '${{github.event_name=="pull_request"}}'
  );
}
