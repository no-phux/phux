// Read-only admission evidence for automated PR arming, not a merge operation.
import { Failure } from "../lib/command.ts";
import { list, str, table, type Table } from "../lib/value.ts";

export const STRICT_AGGREGATE_STEP = "Strict CI aggregate v2";

export function timestamp(value: unknown): number {
  const time = Date.parse(str(value) ?? "");
  if (!Number.isFinite(time)) throw new Failure("missing or invalid GitHub evidence timestamp");
  return time;
}

/** Reject closed/draft/forked/moved PRs before reading any qualification. */
export function readyPull(value: unknown, repository: string, number: number, sha: string): Table {
  const pr = table(value);
  if (pr["number"] !== number || pr["state"] !== "open" || pr["merged"] !== false) {
    throw new Failure(`PR #${number} is not open`);
  }
  if (pr["draft"] !== false) throw new Failure(`PR #${number} is a draft`);
  if (table(pr["base"])["ref"] !== "main") throw new Failure(`PR #${number} does not target main`);
  if (
    table(table(pr["base"])["repo"])["full_name"] !== repository ||
    table(table(pr["head"])["repo"])["full_name"] !== repository
  ) {
    throw new Failure(`PR #${number} is not a same-repository PR`);
  }
  if (table(pr["head"])["sha"] !== sha) throw new Failure(`PR #${number} head changed`);
  timestamp(pr["updated_at"]);
  return pr;
}

/** Timeline order disambiguates lifecycle events; initially Ready PRs use creation. */
export function readyBoundary(pr: Table, events: readonly unknown[]): number {
  let boundary = timestamp(pr["created_at"]);
  let draft = false;
  for (const value of events) {
    const event = table(value);
    if (!["ready_for_review", "converted_to_draft"].includes(str(event["event"]) ?? "")) continue;
    const time = timestamp(event["created_at"]);
    if (time < boundary) throw new Failure("PR timeline is not chronological");
    boundary = time;
    draft = event["event"] === "converted_to_draft";
  }
  if (draft) throw new Failure("latest Ready transition is not yet visible in the PR timeline");
  return boundary;
}

function belongsToPull(run: Table, number: number, sha: string): boolean {
  return (
    run["head_sha"] === sha &&
    list(run["pull_requests"]).some((value) => table(value)["number"] === number)
  );
}

/** Newest evidence wins, including a pending or failed newer run. */
export function latestPullRun(runs: readonly unknown[], number: number, sha: string): Table {
  const [run] = runs
    .map(table)
    .filter((candidate) => belongsToPull(candidate, number, sha))
    .toSorted((a, b) => Number(b["id"]) - Number(a["id"]));
  if (run === undefined) throw new Failure("normal current-head PR run is not registered");
  return run;
}

export function normalRun(run: Table, repository: string, workflow: string): void {
  if (
    run["path"] !== `.github/workflows/${workflow}` ||
    run["event"] !== "pull_request" ||
    table(run["repository"])["full_name"] !== repository ||
    table(run["head_repository"])["full_name"] !== repository
  ) {
    throw new Failure(`${workflow} run has the wrong workflow, event or repository`);
  }
  if (run["status"] !== "completed" || run["conclusion"] !== "success") {
    throw new Failure(`${workflow} run is not completed successfully`);
  }
}

/** A rerun keeps its original creation time and cannot become fresh Ready proof. */
export function afterReady(run: Table, boundary: number): void {
  // GitHub timestamps have second precision. Equal times are ambiguous, so
  // refuse rather than treating a same-second old run as post-transition.
  if (timestamp(run["created_at"]) <= boundary) {
    throw new Failure(
      "ci.yml run is not provably after the latest Ready transition; " +
        "same-second evidence is ambiguous. Keep auto-merge off; a later normal " +
        "current-head run is required, not a rerun of this old run.",
    );
  }
}

export function successfulJob(jobs: readonly unknown[], name: string): Table {
  const matching = jobs.map(table).filter((candidate) => candidate["name"] === name);
  const [job] = matching;
  if (matching.length !== 1 || job?.["status"] !== "completed" || job["conclusion"] !== "success") {
    throw new Failure(`normal ${name} job did not succeed`);
  }
  return job;
}

/** v2's source contract checks every selected job against the actual plan outputs. */
export function strictCiJobs(jobs: readonly unknown[]): void {
  successfulJob(jobs, "plan");
  const aggregate = successfulJob(jobs, "ci");
  successfulJob(list(aggregate["steps"]), STRICT_AGGREGATE_STEP);
  if (
    jobs.some((value) => !["success", "skipped"].includes(str(table(value)["conclusion"]) ?? ""))
  ) {
    throw new Failure("ci.yml contains incomplete or unsuccessful jobs");
  }
}

/** Absence is fine; a separate pending status is not cleared by a green check run. */
export function legacyStatuses(statuses: readonly unknown[]): void {
  for (const value of statuses) {
    const status = table(value);
    if (!["ci", "commitlint"].includes(str(status["context"]) ?? "")) continue;
    if (status["state"] !== "success") {
      throw new Failure(`legacy ${String(status["context"])} status is not successful`);
    }
  }
}
