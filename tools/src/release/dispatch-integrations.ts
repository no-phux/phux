// Dispatch agent-integration-release.yml and wait for it.
//
// npm trusted publishing matches the entry workflow filename. Calling that
// workflow with workflow_call makes the token say release.yml, and npm
// answers 404. A workflow_dispatch keeps the filename npm trusts.
import { Failure } from "../lib/command.ts";
import { gh, ghInherit } from "../lib/github.ts";
import { list, table } from "../lib/value.ts";

const WORKFLOW = "agent-integration-release.yml";
const POLLS = 36;
const POLL_MS = 5_000;

export interface Dispatcher {
  /** The ids of recent `workflow_dispatch` runs of the workflow. */
  dispatchRuns(repo: string): unknown[];
  /** Run `gh` with these arguments; a non-zero exit throws. */
  run(args: string[]): void;
  sleep(ms: number): Promise<void>;
}

const GH: Dispatcher = {
  dispatchRuns(repo) {
    const args = ["run", "list", "--repo", repo, "--workflow", WORKFLOW, "--limit", "30"];
    const runs = list(JSON.parse(gh([...args, "--json", "databaseId,event"]))).map(table);
    return runs
      .filter((run) => run["event"] === "workflow_dispatch")
      .map((run) => run["databaseId"]);
  },
  run: (args) => void gh(args),
  sleep: (ms) => Bun.sleep(ms),
};

/** Dispatch the workflow for `tag` and return the id of the run it created. */
export async function dispatch(repo: string, tag: string, deps = GH): Promise<unknown> {
  const before = new Set(deps.dispatchRuns(repo));
  const inputs = ["-f", `tag=${tag}`, "-f", "dry_run=false"];
  deps.run(["workflow", "run", WORKFLOW, "--repo", repo, "--ref", "main", ...inputs]);
  for (let poll = 0; poll < POLLS; poll++) {
    await deps.sleep(POLL_MS);
    const created = deps.dispatchRuns(repo).find((id) => !before.has(id));
    if (created !== undefined) return created;
  }
  throw new Failure(`no ${WORKFLOW} run appeared for ${tag}`);
}

/** release.yml's `integrations` job: TAGS (a JSON list) in, one watched run each. */
export async function main(): Promise<number> {
  const [repo, raw] = [process.env["GITHUB_REPOSITORY"], process.env["TAGS"]];
  if (!repo || raw === undefined) throw new Failure("GITHUB_REPOSITORY and TAGS are required");
  const failed: string[] = [];
  for (const tag of list(JSON.parse(raw)).map(String)) {
    const runId = String(await dispatch(repo, tag));
    console.log(`watching ${tag} in run ${runId}`);
    if (ghInherit(["run", "watch", runId, "--repo", repo, "--exit-status"]) !== 0) {
      failed.push(`${tag} (run ${runId})`);
    }
  }
  if (failed.length > 0) throw new Failure(`integration publish failed: ${failed.join(", ")}`);
  return 0;
}
