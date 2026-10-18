// Dispatch agent-integration-release.yml and wait for it.
//
// npm trusted publishing matches the entry workflow filename. Calling that
// workflow with workflow_call makes the token say publish.yml, and npm
// answers 404. A workflow_dispatch keeps the filename npm trusts.
import { gh, ghInherit } from "../lib/github.ts";
import { list, table } from "../lib/value.ts";

const WORKFLOW = "agent-integration-release.yml";
const DEADLINE_SECONDS = 180;
const POLL_SECONDS = 5;

export interface WorkflowRun {
  readonly databaseId: unknown;
  readonly event: unknown;
}

export interface Dispatcher {
  listRuns(repo: string): WorkflowRun[];
  /** Run `gh` with these arguments; a non-zero exit throws. */
  run(args: string[]): void;
  sleep(seconds: number): void;
  /** Seconds since the epoch. */
  clock(): number;
}

function runIds(repo: string): WorkflowRun[] {
  const raw: unknown = JSON.parse(
    gh([
      "run",
      "list",
      "--repo",
      repo,
      "--workflow",
      WORKFLOW,
      "--limit",
      "30",
      "--json",
      "databaseId,event",
    ]),
  );
  return list(raw).map((row) => {
    const run = table(row);
    return { databaseId: run["databaseId"], event: run["event"] };
  });
}

export const GH_DISPATCHER: Dispatcher = {
  listRuns: runIds,
  run(args) {
    if (ghInherit(args) !== 0) throw new Error(`gh ${args.join(" ")} failed`);
  },
  sleep: (seconds) => Bun.sleepSync(seconds * 1000),
  clock: () => Date.now() / 1000,
};

/** Dispatch the workflow for `tag` and return the id of the run it created. */
export function dispatch(repo: string, tag: string, deps: Dispatcher = GH_DISPATCHER): unknown {
  const before = new Set(deps.listRuns(repo).map((run) => run.databaseId));
  deps.run([
    "workflow",
    "run",
    WORKFLOW,
    "--repo",
    repo,
    "--ref",
    "main",
    "-f",
    `tag=${tag}`,
    "-f",
    "dry_run=false",
  ]);
  const deadline = deps.clock() + DEADLINE_SECONDS;
  while (deps.clock() < deadline) {
    deps.sleep(POLL_SECONDS);
    for (const run of deps.listRuns(repo)) {
      if (!before.has(run.databaseId) && run.event === "workflow_dispatch") return run.databaseId;
    }
  }
  throw new Error(`no ${WORKFLOW} run appeared for ${tag}`);
}

function watch(repo: string, runId: unknown): number {
  return ghInherit(["run", "watch", String(runId), "--repo", repo, "--exit-status"]);
}

/** publish.yml's `integration` job: TAGS (a JSON list) in, one watched run each. */
export function main(): number {
  const [repo, raw] = [process.env["GITHUB_REPOSITORY"], process.env["TAGS"]];
  if (repo === undefined || raw === undefined)
    throw new Error("GITHUB_REPOSITORY and TAGS are required");
  const tags = list(JSON.parse(raw));
  const failed: string[] = [];
  for (const tag of tags) {
    const runId = dispatch(repo, String(tag));
    console.log(`watching ${String(tag)} in run ${String(runId)}`);
    if (watch(repo, runId) !== 0) failed.push(`${String(tag)} (run ${String(runId)})`);
  }
  if (failed.length > 0) {
    console.error(`integration publish failed: ${failed.join(", ")}`);
    return 1;
  }
  return 0;
}
