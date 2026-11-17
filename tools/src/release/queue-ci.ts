// A merge-queue ci.yml run can stand in for a missing push run, and only then.
// The push gate stays push-on-main. This path accepts one successful
// gh-readonly-queue run of the exact SHA when main contains that SHA, the
// jobs match the push plan, and that same run's rune-macos call passed.
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, Failure } from "../lib/command.ts";
import { git } from "../lib/git.ts";
import { api } from "../lib/github.ts";
import { list, str, table, type Table } from "../lib/value.ts";
import { jobs, workflow } from "../policy/workflows.ts";
import { classifyDetection, detect, type Detection } from "../routing/detect.ts";
import { LANES, plan, reused, type Lane, type PathScope, type Plan } from "../routing/plan.ts";
import { findValidation, type Receipt } from "../routing/receipt.ts";

const CI = ".github/workflows/ci.yml";
const RUNE_MACOS = ".github/workflows/rune-macos.yml";
const QUEUE_REF = "gh-readonly-queue/";
const FALLBACK = "fallback (C): get a push-equivalent ci run on that exact SHA";

export type NativeProof = (
  sha: string,
  state: string,
  qualified: boolean,
) => { readonly state: string };

interface RequiredJob {
  readonly id: string;
  readonly name: string;
  readonly lanes: readonly Lane[];
  readonly reusable: boolean;
}

const laneName = (value: string): Lane | undefined => LANES.find((lane) => lane === value);

const callsRuneMacos = (spec: Table): boolean =>
  (str(spec["uses"]) ?? "").endsWith("/rune-macos.yml");

/**
 * Jobs ci.yml runs when the push plan selects their lane. Read from the workflow.
 * The rune-macos caller is judged on its own (see `appleState`), even though
 * its PR fallback gate also names the rune lane.
 */
export function jobsRequiredBy(pushPlan: Plan): readonly RequiredJob[] {
  const selected = pushPlan.lanes;
  const required: RequiredJob[] = [];
  for (const [id, spec] of Object.entries(jobs(workflow("ci.yml")))) {
    if (callsRuneMacos(spec)) continue;
    const gate = str(spec["if"]) ?? "";
    const lanes = [...gate.matchAll(/needs\.plan\.outputs\.([A-Za-z0-9_]+) == 'true'/g)]
      .map((match) => laneName(match[1] ?? ""))
      .filter((lane): lane is Lane => lane !== undefined);
    if (lanes.length === 0 || !lanes.some((lane) => selected.has(lane))) continue;
    required.push({
      id,
      name: str(spec["name"]) ?? id,
      lanes,
      reusable: (str(spec["uses"]) ?? "") !== "",
    });
  }
  return required;
}

/** Display name of the ci.yml job that calls rune-macos.yml. */
export function queueAppleName(): string {
  const found = Object.entries(jobs(workflow("ci.yml"))).find(([, spec]) => callsRuneMacos(spec));
  if (found === undefined) throw new Failure("ci.yml does not call rune-macos.yml");
  const [id, spec] = found;
  return str(spec["name"]) ?? id;
}

function refuse(reason: string): string {
  return `${reason}; ${FALLBACK}`;
}

function matchesJob(template: string, apiName: string): boolean {
  if (apiName === template || apiName.startsWith(`${template} / `)) return true;
  const head = template.split("${{")[0] ?? "";
  return head !== "" && head !== template && apiName.startsWith(head);
}

type JobState = "pass" | "miss" | "fail";

function jobState(rows: readonly Table[], required: RequiredJob): JobState {
  const conclusions = rows.flatMap((row) => {
    const name = str(row["name"]);
    return name !== undefined && matchesJob(required.name, name)
      ? [str(row["conclusion"]) ?? "missing"]
      : [];
  });
  if (conclusions.length === 0) return "miss";
  if (conclusions.some((item) => item === "failure" || item === "timed_out")) return "fail";
  if (
    required.reusable &&
    conclusions.some((item) => item === "success") &&
    conclusions.every((item) => item === "success" || item === "skipped")
  ) {
    return "pass";
  }
  return conclusions.every((item) => item === "success") ? "pass" : "miss";
}

/** A receipt may excuse a lane reuse would skip. ffi, icount and native still run. */
function excused(required: RequiredJob, pushPlan: Plan, lanesProved: boolean): boolean {
  if (!lanesProved) return false;
  const covered = new Set(pushPlan.coverage.split(",").filter((lane) => lane !== ""));
  const kept = reused(pushPlan).lanes;
  const triggering = required.lanes.filter((lane) => pushPlan.lanes.has(lane));
  return triggering.length > 0 && triggering.every((lane) => covered.has(lane) && !kept.has(lane));
}

function appleState(rows: readonly Table[]): string {
  const caller = queueAppleName();
  const related = rows.filter((row) => {
    const name = str(row["name"]) ?? "";
    return name === caller || name.startsWith(`${caller} / `);
  });
  const aggregate =
    related.find((row) => str(row["name"]) === caller) ??
    related.find((row) => (str(row["name"]) ?? "").endsWith("/ rune-macos"));
  const conclusion = aggregate === undefined ? undefined : str(aggregate["conclusion"]);
  if (conclusion === undefined || conclusion === "skipped") return "missing";
  return conclusion;
}

/**
 * Why this merge_group evidence cannot tag `sha`, or undefined when it can.
 * `jobs` are that run's jobs. `lanesProved` is an identical-tree ci receipt of
 * `pushPlan.coverage`. `nativeQualified` is the full-coverage Apple receipt.
 */
export function evaluateQueue(
  sha: string,
  run: Table | undefined,
  jobs: readonly unknown[],
  compareStatus: string,
  pushPlan: Plan,
  lanesProved: boolean,
  nativeQualified: boolean,
  proof: NativeProof,
): string | undefined {
  if (run === undefined) return refuse(`no finished merge_group ci.yml run for ${sha}`);
  if (run["head_sha"] !== sha) {
    return refuse(
      `merge_group ci.yml run ${String(run["id"])} is for ${String(run["head_sha"])}, not ${sha}`,
    );
  }
  if (run["event"] !== "merge_group") {
    return refuse(
      `ci.yml run ${String(run["id"])} event is ${String(run["event"])}, not merge_group`,
    );
  }
  const branch = str(run["head_branch"]) ?? "";
  if (!branch.startsWith(QUEUE_REF)) {
    return refuse(
      `merge_group ci.yml run ${String(run["id"])} ref ${branch || "missing"} is not under ${QUEUE_REF}`,
    );
  }
  if (run["status"] !== "completed" || run["conclusion"] !== "success") {
    const state = str(run["conclusion"]) ?? str(run["status"]) ?? "pending";
    return refuse(`merge_group ci.yml run ${String(run["id"])} is ${state}`);
  }
  if (compareStatus !== "behind" && compareStatus !== "identical") {
    return refuse(`${sha} is not reachable from main (compare status ${compareStatus})`);
  }

  const rows = jobs.map(table);
  const missed: string[] = [];
  const failed: string[] = [];
  for (const required of jobsRequiredBy(pushPlan)) {
    const state = jobState(rows, required);
    if (state === "pass") continue;
    if (state === "miss" && excused(required, pushPlan, lanesProved)) continue;
    (state === "fail" ? failed : missed).push(required.name);
  }
  if (failed.length > 0) {
    return refuse(`merge_group ci.yml run has failed required jobs: ${failed.join(", ")}`);
  }
  if (missed.length > 0) {
    return refuse(`merge_group ci.yml run did not run required jobs: ${missed.join(", ")}`);
  }

  const child = appleState(rows);
  const reported = child === "missing" ? "pending" : child;
  const native = proof(sha, reported, child === "success" && nativeQualified).state;
  if (native === "success") return undefined;
  if (child === "missing") {
    return refuse(`no successful rune-macos child in the merge_group run for ${sha}`);
  }
  if (child !== "success") return refuse(`rune-macos child is ${child} for ${sha}`);
  return refuse(`no full-coverage native receipt for ${sha}`);
}

function newestQueueRun(runs: readonly unknown[]): Table | undefined {
  const attempt = (run: Table): number => Number(run["run_attempt"] ?? 1);
  const [latest] = runs
    .map(table)
    .filter((run) => run["event"] === "merge_group" && run["path"] === CI)
    .toSorted((a, b) => Number(b["id"]) - Number(a["id"]) || attempt(b) - attempt(a));
  return latest;
}

function planFor(detection: Detection): Plan {
  const classified = classifyDetection(detection);
  const scope: PathScope =
    detection.kind === "paths"
      ? { kind: "diff", paths: detection.paths }
      : detection.kind === "release-metadata"
        ? { kind: "none" }
        : { kind: "full" };
  return plan(classified, "push", { scope, runeQualify: true });
}

/** The push plan for `sha`: its parent diff, failing closed to every lane. */
export function pushPlanFor(sha: string): Plan {
  const dir = mkdtempSync(join(tmpdir(), "ci-green-"));
  try {
    const parent = git(["rev-parse", "--verify", `${sha}^`]).trim();
    const eventPath = join(dir, "push.json");
    writeFileSync(eventPath, JSON.stringify({ before: parent, after: sha }));
    return planFor(detect("push", eventPath));
  } catch (error) {
    console.error(`push plan for ${sha} failed closed: ${describe(error)}`);
    return planFor({ kind: "full", reason: describe(error) });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function runJobs(repository: string, id: number): unknown[] {
  const found: unknown[] = [];
  for (let page = 1; page <= 5; page += 1) {
    const batch = list(
      table(api.json(`repos/${repository}/actions/runs/${id}/jobs?per_page=100&page=${page}`))[
        "jobs"
      ],
    );
    found.push(...batch);
    if (batch.length < 100) break;
  }
  return found;
}

function compareStatus(repository: string, sha: string): string {
  try {
    return str(table(api.json(`repos/${repository}/compare/main...${sha}`))["status"]) ?? "unknown";
  } catch (error) {
    console.error(`compare main...${sha} failed: ${describe(error)}`);
    return "unknown";
  }
}

function commitTree(sha: string): string | undefined {
  try {
    return git(["rev-parse", "--verify", `${sha}^{tree}`]).trim();
  } catch (error) {
    console.error(`no tree for ${sha}: ${describe(error)}`);
    return undefined;
  }
}

function receiptFound(receipt: Receipt, repository: string, pushSha?: string): boolean {
  return findValidation(receipt, repository, undefined, pushSha) !== undefined;
}

function safely(read: () => boolean): boolean {
  try {
    return read();
  } catch (error) {
    console.error(`validation receipt unavailable: ${describe(error)}`);
    return false;
  }
}

/** Refusal for a qualifying merge_group run, or undefined when the SHA may tag. */
export function mergeGroupGate(
  repository: string,
  sha: string,
  proof: NativeProof,
): string | undefined {
  const query = new URLSearchParams({ head_sha: sha, event: "merge_group", per_page: "20" });
  const runs = list(
    table(api.json(`repos/${repository}/actions/workflows/ci.yml/runs?${query.toString()}`))[
      "workflow_runs"
    ],
  );
  const run = newestQueueRun(runs);
  // The plan is unused when there is no run. Avoid classifying the repo just to refuse.
  const idle: Plan = {
    lanes: new Set(),
    runeQualify: false,
    coverage: "",
    unitMode: "skip",
    unitTargets: "",
    testFilterset: "",
    docsCatalog: false,
  };
  if (run === undefined)
    return evaluateQueue(sha, undefined, [], "unknown", idle, false, false, proof);

  const id = Number(run["id"]);
  const planned = pushPlanFor(sha);
  const tree = commitTree(sha);
  const lanesProved =
    tree !== undefined &&
    safely(() =>
      receiptFound({ schema: 2, tree, workflow: CI, coverage: planned.coverage }, repository),
    );
  const native: Receipt = {
    schema: 2,
    tree: tree ?? "",
    workflow: RUNE_MACOS,
    coverage: "macos,rune_ios",
  };
  const nativeQualified =
    tree !== undefined &&
    safely(
      () => findValidation(native, repository) === id || receiptFound(native, repository, sha),
    );
  return evaluateQueue(
    sha,
    run,
    runJobs(repository, id),
    compareStatus(repository, sha),
    planned,
    lanesProved,
    nativeQualified,
    proof,
  );
}
