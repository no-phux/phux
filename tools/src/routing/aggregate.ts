// The required ci check: plan succeeded, and every other job passed or was
// not selected. A filtered skip passes. Main, the merge queue and a release
// bot's PR also require native setup and rust icount. The queue and a
// Rune-affecting release PR require Apple; a release PR also requires its
// selected Rune Linux lane. Other pull requests gate on their selected lanes
// only: Apple, Linux packaging and the mandatory lanes qualify on main.
// Draft notices never qualify or write receipts. The renamed draft job cannot
// invalidate an older same-SHA check; automated arming also needs pr-ready-ci.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { isTable, str, table, type Table } from "../lib/value.ts";

/** The plan outputs that must each be a literal "true" or "false". */
const FLAGS = [
  "guards",
  "lint",
  "tests",
  "e2e",
  "ffi",
  "icount",
  "integrations",
  "web",
  "native",
  "rune",
  "rune_qualify",
  "release_qualify",
] as const;

const PASSING = new Set(["success", "skipped"]);

export interface AggregateInput {
  /** `toJSON(needs)`: each job id to its `result` and, for plan, `outputs`. */
  results: Table;
  event: string;
  draft: boolean;
}

export interface Verdict {
  ok: boolean;
  /** Workflow-command lines for the log, in order. */
  lines: string[];
}

const result = (results: Table, id: string): string | undefined =>
  str(table(results[id])["result"]);

/** Which jobs must succeed (true) or may also skip (false) for this event and plan. */
export function requiredJobs(event: string, outputs: Table): Record<string, boolean> {
  const on = (key: string) => outputs[key] === "true";
  const release = event === "pull_request" && on("release_qualify");
  const mandatory = event === "push" || event === "merge_group" || release;
  const linux = release && on("rune");
  return {
    guards: on("guards"),
    lint: on("lint"),
    "test-build": on("tests") || on("e2e"),
    tests: on("tests"),
    e2e: on("e2e"),
    ffi: on("ffi"),
    integrations: on("integrations"),
    "web-vt": on("web"),
    "web-client": on("web"),
    native: mandatory || on("native"),
    icount: mandatory || on("icount"),
    rune: on("rune"),
    "queue-apple": event === "merge_group" || linux,
  };
}

/** Why a non-draft run fails the gate; empty when it passes. */
export function gateFailures(event: string, results: Table): string[] {
  const plan = table(results["plan"]);
  const outputs = table(plan["outputs"]);
  const failures: string[] = [];
  if (str(plan["result"]) !== "success") failures.push("plan did not succeed");
  const malformed = FLAGS.filter((key) => outputs[key] !== "true" && outputs[key] !== "false");
  if (malformed.length > 0) failures.push(`plan outputs not true/false: ${malformed.join(", ")}`);
  const release = event === "pull_request" && outputs["release_qualify"] === "true";
  if (release && outputs["rune"] === "true" && outputs["rune_qualify"] !== "true") {
    failures.push("a Rune-affecting release PR must qualify Rune");
  }
  for (const id of Object.keys(results)) {
    if (!PASSING.has(result(results, id) ?? "")) failures.push(`${id} neither passed nor skipped`);
  }
  for (const [id, mustSucceed] of Object.entries(requiredJobs(event, outputs))) {
    const state = result(results, id) ?? "";
    if (mustSucceed ? state !== "success" : !PASSING.has(state)) {
      failures.push(`${id} is ${mustSucceed ? "required" : "expected"} and was ${state || "missing"}`);
    }
  }
  return failures;
}

/** The gate's verdict, with the log lines it prints. */
export function aggregate({ results, event, draft }: AggregateInput): Verdict {
  const lines = Object.entries(results).map(
    ([id, job]) => `${str(table(job)["result"]) ?? "null"}\t${id}`,
  );
  if (draft) {
    const idle = Object.keys(results).every((id) => result(results, id) === "skipped");
    lines.push(
      idle
        ? "::notice::draft PR: lanes skipped; ready the PR to run ci"
        : "::error::a draft must not run qualification",
    );
    return { ok: idle, lines };
  }
  const failures = gateFailures(event, results);
  lines.push(...failures.map((failure) => `::error::${failure}`));
  return { ok: failures.length === 0, lines };
}

/** Parse `toJSON(needs)`; anything but an object is a failed gate. */
export function parseResults(text: string): Table | undefined {
  try {
    const value: unknown = JSON.parse(text);
    return isTable(value) ? value : undefined;
  } catch {
    return undefined;
  }
}

/** The validation receipt the ci job uploads for a passing PR or queue run. */
export function writeReceipt(receipt: string, runnerTemp: string): void {
  mkdirSync(join(runnerTemp, "receipt"), { recursive: true });
  writeFileSync(join(runnerTemp, "receipt", "receipt.json"), `${receipt}\n`);
}
