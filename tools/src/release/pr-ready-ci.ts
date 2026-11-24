import { parseArgs } from "node:util";
import { Failure, UsageError, type Command } from "../lib/command.ts";
import { api, gh, type Api } from "../lib/github.ts";
import { list, table, type Table } from "../lib/value.ts";
import {
  afterReady,
  latestPullRun,
  legacyStatuses,
  normalRun,
  readyBoundary,
  readyPull,
  strictCiJobs,
  successfulJob,
} from "./pr-ready-proof.ts";

/** Bounded complete pagination: malformed/truncated evidence fails closed. */
function pages(client: Api, path: string, key?: string): unknown[] {
  const found: unknown[] = [];
  const separator = path.includes("?") ? "&" : "?";
  for (let page = 1; page <= 20; page += 1) {
    const response = client.json(`${path}${separator}per_page=100&page=${page}`);
    const batch = key === undefined ? response : table(response)[key];
    if (!Array.isArray(batch)) throw new Failure(`malformed GitHub evidence: ${path}`);
    found.push(...list(batch));
    if (batch.length < 100) return found;
  }
  throw new Failure(`GitHub evidence exceeds pagination bound: ${path}`);
}

function positiveInteger(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 1) {
    throw new Failure("missing or invalid workflow run identity");
  }
  return value;
}

function jobs(client: Api, repository: string, run: Table): unknown[] {
  const id = positiveInteger(run["id"]);
  const attempt = positiveInteger(run["run_attempt"]);
  return pages(client, `repos/${repository}/actions/runs/${id}/attempts/${attempt}/jobs`, "jobs");
}

function latest(
  client: Api,
  repository: string,
  number: number,
  sha: string,
  workflow: string,
): Table {
  const query = new URLSearchParams({ head_sha: sha, event: "pull_request" });
  const runs = pages(
    client,
    `repos/${repository}/actions/workflows/${workflow}/runs?${query.toString()}`,
    "workflow_runs",
  );
  const run = latestPullRun(runs, number, sha);
  normalRun(run, repository, workflow);
  return run;
}

function unchangedRun(before: Table, after: Table): void {
  if (before["id"] !== after["id"] || before["run_attempt"] !== after["run_attempt"]) {
    throw new Failure("new workflow run or attempt appeared while checking readiness");
  }
}

/** Only GETs. The caller must still condition its later mutation on the expected head. */
export function prReadyCi(
  repository: string,
  number: number,
  sha: string,
  client: Api = api,
): number {
  const pullPath = `repos/${repository}/pulls/${number}`;
  const timelinePath = `repos/${repository}/issues/${number}/timeline`;
  const before = readyPull(client.json(pullPath), repository, number, sha);
  const boundary = readyBoundary(before, pages(client, timelinePath));
  const ci = latest(client, repository, number, sha, "ci.yml");
  afterReady(ci, boundary);
  strictCiJobs(jobs(client, repository, ci));
  const lint = latest(client, repository, number, sha, "commitlint.yml");
  successfulJob(jobs(client, repository, lint), "commitlint");
  legacyStatuses(pages(client, `repos/${repository}/commits/${sha}/status`, "statuses"));
  unchangedRun(ci, latest(client, repository, number, sha, "ci.yml"));
  unchangedRun(lint, latest(client, repository, number, sha, "commitlint.yml"));
  const finalBoundary = readyBoundary(before, pages(client, timelinePath));
  const after = readyPull(client.json(pullPath), repository, number, sha);
  if (before["updated_at"] !== after["updated_at"] || boundary !== finalBoundary) {
    throw new Failure("PR changed while checking readiness");
  }
  console.log(`PR #${number} ${sha}: normal post-Ready strict CI and commitlint succeeded`);
  return 0;
}

function repository(): string {
  const name =
    process.env["GITHUB_REPOSITORY"] ??
    process.env["GH_REPO"] ??
    gh(["repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"]).trim();
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(name)) {
    throw new UsageError("expected OWNER/REPO in GITHUB_REPOSITORY or GH_REPO");
  }
  return name;
}

function readyArguments(args: string[]): [number, string] {
  const values = parseArgs({ args, allowPositionals: true }).positionals;
  if (values.length !== 2) throw new UsageError("expected PR_NUMBER EXPECTED_FULL_SHA");
  const number = values[0] ?? "";
  const sha = values[1] ?? "";
  if (!/^[1-9]\d*$/.test(number)) throw new UsageError("expected a positive PR number");
  if (!/^[a-f0-9]{40}$/.test(sha)) throw new UsageError("expected one full lowercase 40-hex SHA");
  const pr = Number(number);
  if (!Number.isSafeInteger(pr)) throw new UsageError("PR number is too large");
  return [pr, sha];
}

export const prReadyCommand: Command = {
  summary:
    "Read-only: refuse auto-merge arming without fresh normal Ready CI on the expected head.",
  usage: "PR_NUMBER EXPECTED_FULL_SHA",
  help: "Exit 0 is evidence, not a merge. Recheck head at mutation with --match-head-commit.",
  run(args) {
    const [number, sha] = readyArguments(args);
    return prReadyCi(repository(), number, sha);
  },
};
