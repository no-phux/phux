// Choose which draft releases to ship, and which stale drafts to drop.
//
// publish.yml is the only workflow that flips a draft public. Release Please
// has already cut the tag and the notes. This plan runs when that happens and
// again when ci.yml finishes, plus on a schedule, so the two events can arrive
// in either order and a missed event still converges.
//
// Nothing here builds artifacts. It only decides.
import { appendFileSync } from "node:fs";
import { git } from "../lib/git.ts";
import { api as ghApi, ghInherit, ghLines, type Api } from "../lib/github.ts";
import { list, str, table, type Table } from "../lib/value.ts";

export type Version = readonly [number, number, number];

export function compareVersions(left: Version, right: Version): number {
  return left[0] - right[0] || left[1] - right[1] || left[2] - right[2];
}

/**
 * Conclusion of the newest push run of `workflow` on main for `sha`.
 *
 * Anything else (a pull request, a different commit, a run still going) is
 * `pending`, so a draft is never shipped on the wrong evidence.
 */
export function validationState(runs: readonly unknown[], sha: string, workflow: string): string {
  const matching = runs
    .map(table)
    .filter(
      (run) =>
        run["head_sha"] === sha &&
        run["head_branch"] === "main" &&
        run["event"] === "push" &&
        run["path"] === workflow,
    );
  const order = (run: Table): [number, number] => [
    Number(run["id"]),
    Number(run["run_attempt"] ?? 1),
  ];
  const latest = matching.reduce<Table | undefined>((best, run) => {
    if (best === undefined) return run;
    const [[runId, runAttempt], [bestId, bestAttempt]] = [order(run), order(best)];
    return runId > bestId || (runId === bestId && runAttempt > bestAttempt) ? run : best;
  }, undefined);
  if (latest === undefined || latest["status"] !== "completed") return "pending";
  return str(latest["conclusion"]) ?? "unknown";
}

// `vX.Y.Z` is phux; `<component>-vX.Y.Z` is every other release-please package.
const TAG = /^(?:([a-z0-9]+(?:-[a-z0-9]+)*)-)?v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const CI_WORKFLOW = ".github/workflows/ci.yml";
const PENDING = new Set(["pending", "queued", "in_progress", "waiting", "requested", "missing"]);

export interface TagIdentity {
  readonly component: string;
  readonly version: Version;
}

export function parseTag(tag: string): TagIdentity | undefined {
  const match = TAG.exec(tag);
  if (match === null) return undefined;
  return {
    component: match[1] ?? "phux",
    version: [Number(match[2]), Number(match[3]), Number(match[4])],
  };
}

type Lane = "phux" | "desktop" | "integration";

function lane(component: string): Lane {
  return component === "phux" || component === "desktop" ? component : "integration";
}

export interface Release {
  readonly tag: string;
  readonly draft: boolean;
  readonly sha: string;
}

type Row = Release & TagIdentity;

export interface Plan {
  publish: string[];
  delete: string[];
  wait: boolean;
  fail: string | undefined;
  blocked: string;
}

export type CiState = (sha: string) => string;

function newest(rows: readonly Row[]): Row | undefined {
  return rows.reduce<Row | undefined>(
    (best, row) =>
      best === undefined || compareVersions(row.version, best.version) > 0 ? row : best,
    undefined,
  );
}

/** Draft tags to delete, and each component's newest draft ahead of its published release. */
function partition(rows: readonly Row[]): { delete: string[]; candidates: Row[] } {
  const byComponent = new Map<string, Row[]>();
  for (const row of rows)
    byComponent.set(row.component, [...(byComponent.get(row.component) ?? []), row]);
  const remove: string[] = [];
  const candidates: Row[] = [];
  for (const group of byComponent.values()) {
    const latest = newest(group.filter((row) => !row.draft))?.version;
    const drafts = group.filter((row) => row.draft);
    for (const row of drafts) {
      if (latest !== undefined && compareVersions(row.version, latest) < 0) remove.push(row.tag);
    }
    const ahead = newest(
      drafts.filter((row) => latest === undefined || compareVersions(row.version, latest) > 0),
    );
    if (ahead !== undefined) candidates.push(ahead);
  }
  return { delete: remove, candidates };
}

/**
 * Publish tags, draft tags to delete, and whether to wait or fail.
 *
 * `ciState(sha)` is a conclusion string, or `pending` when the ci run has not
 * finished. Tags that are not component versions (the moving `next`
 * prerelease, for one) are ignored.
 */
export function decide(
  releases: readonly Release[],
  ciState: CiState,
  options: { eventSha?: string | undefined; dispatchTag?: string | undefined } = {},
): Plan {
  const parsed: Row[] = releases.flatMap((release) => {
    const identity = parseTag(release.tag);
    return identity === undefined ? [] : [{ ...release, ...identity }];
  });
  const { delete: remove, candidates } = partition(parsed);
  const result: Plan = { publish: [], delete: remove, wait: false, fail: undefined, blocked: "" };
  if (options.dispatchTag)
    return decideDispatch(parsed, candidates, ciState, options.dispatchTag, result);
  if (options.eventSha) return decideEvent(candidates, ciState, options.eventSha, result);
  return decideReconcile(candidates, ciState, result);
}

function decideDispatch(
  parsed: readonly Row[],
  candidates: readonly Row[],
  ciState: CiState,
  tag: string,
  result: Plan,
): Plan {
  const selected = parsed.find((row) => row.tag === tag);
  if (selected === undefined) {
    result.fail = `no release named ${tag}`;
    return result;
  }
  if (!selected.draft) return result;
  if (!candidates.some((row) => row.tag === selected.tag)) {
    result.fail = `${tag} is older than the published ${selected.component} release`;
    return result;
  }
  const state = ciState(selected.sha);
  if (state !== "success") {
    result.fail = `ci.yml is ${state} for ${selected.sha}; ${tag} stays a draft`;
    return result;
  }
  result.publish = [selected.tag];
  return result;
}

/**
 * Ship every draft whose own commit's ci is green, not only the event's.
 *
 * The event names the commit that just finished, but GitHub keeps a single
 * pending run per concurrency group and cancels the older one, so the publish
 * run a draft is waiting for can be replaced before it ever starts. Sweeping
 * every candidate is what lets a later event recover it; without that, a
 * stranded draft sits until the daily reconcile while installers have nothing
 * to resolve. The evidence guard is unchanged: a tag still ships only on a
 * green ci run for its own commit.
 */
function decideEvent(
  candidates: readonly Row[],
  ciState: CiState,
  sha: string,
  result: Plan,
): Plan {
  const mine = candidates.filter((row) => row.sha === sha);
  if (mine.length === 0) return decideReconcile(candidates, ciState, result);
  const state = ciState(sha);
  if (PENDING.has(state)) {
    result.wait = true;
    return result;
  }
  if (state !== "success") {
    const tags = mine.map((row) => row.tag).join(", ");
    result.fail = `ci.yml is ${state} for ${sha}; not publishing ${tags}`;
    return result;
  }
  return decideReconcile(candidates, ciState, result);
}

function decideReconcile(candidates: readonly Row[], ciState: CiState, result: Plan): Plan {
  const blocked: string[] = [];
  for (const row of candidates) {
    const state = ciState(row.sha);
    if (state === "success") result.publish.push(row.tag);
    else if (!PENDING.has(state)) blocked.push(`${row.tag} (ci.yml ${state} for ${row.sha})`);
  }
  if (blocked.length > 0) result.blocked = `still draft, ci is not green: ${blocked.join("; ")}`;
  return result;
}

export interface Lanes {
  phux: string;
  desktop: string;
  integration: string[];
}

export function splitLanes(tags: readonly string[]): Lanes {
  const lanes: Lanes = { phux: "", desktop: "", integration: [] };
  for (const tag of tags) {
    const identity = parseTag(tag);
    if (identity === undefined) continue;
    const destination = lane(identity.component);
    if (destination === "integration") lanes.integration.push(tag);
    else lanes[destination] = tag;
  }
  return lanes;
}

export function ciConclusion(repository: string, sha: string, api: Api = ghApi): string {
  const query = new URLSearchParams({ head_sha: sha, event: "push", per_page: "20" });
  const payload = table(
    api.json(`repos/${repository}/actions/workflows/ci.yml/runs?${query.toString()}`),
  );
  return validationState(list(payload["workflow_runs"]), sha, CI_WORKFLOW);
}

function releasesFromApi(repository: string): { tag: string; draft: boolean }[] {
  return ghLines([
    "api",
    `repos/${repository}/releases`,
    "--paginate",
    "--jq",
    ".[] | {tag:.tag_name,draft:.draft}",
  ]).map((line) => {
    const row = table(line);
    const tag = str(row["tag"]);
    if (tag === undefined) throw new Error(`release row without a tag: ${JSON.stringify(line)}`);
    return { tag, draft: row["draft"] === true };
  });
}

function tagSha(tag: string): string {
  return git(["rev-parse", "--verify", `refs/tags/${tag}^{commit}`], process.cwd()).trim();
}

function deleteDrafts(repository: string, tags: readonly string[]): void {
  for (const tag of tags) {
    const args = ["release", "delete", tag, "--repo", repository, "--yes"];
    if (ghInherit(args) !== 0) throw new Error(`gh ${args.join(" ")} failed`);
    console.log(`deleted superseded draft ${tag}`);
  }
}

function writeOutputs(path: string, lanes: Lanes): void {
  appendFileSync(
    path,
    `phux_tag=${lanes.phux}\ndesktop_tag=${lanes.desktop}\n` +
      `integration_tags=${JSON.stringify(lanes.integration)}\n`,
  );
}

/** publish.yml's `plan` job: env in, `GITHUB_OUTPUT` lanes out. */
export function main(): number {
  const env = process.env;
  const repository = env["GITHUB_REPOSITORY"];
  if (repository === undefined) throw new Error("GITHUB_REPOSITORY is unset");
  const event = env["EVENT_NAME"] ?? "";
  const dispatch = (env["DISPATCH_TAG"] ?? "").trim();
  const sha = (env["EVENT_SHA"] ?? "").trim();
  const eventSha = event === "workflow_run" && sha && !dispatch ? sha : undefined;

  const rows = releasesFromApi(repository)
    .filter((release) => parseTag(release.tag) !== undefined)
    .map((release) => ({ ...release, sha: tagSha(release.tag) }));

  const plan = decide(rows, (commit) => ciConclusion(repository, commit), {
    eventSha,
    dispatchTag: dispatch || undefined,
  });
  if (plan.delete.length > 0) deleteDrafts(repository, plan.delete);

  if (plan.fail !== undefined) {
    console.error(`::error::${plan.fail}`);
    return 1;
  }

  const lanes = splitLanes(plan.publish);
  const output = env["GITHUB_OUTPUT"];
  if (output) writeOutputs(output, lanes);
  if (plan.wait) {
    console.log(`ci.yml still running for ${eventSha}; publish will retry when it finishes`);
  } else if (plan.publish.length > 0) {
    console.log(`publishing ${plan.publish.join(", ")}`);
  } else {
    console.log("nothing to publish");
  }
  if (plan.blocked) console.log(`::warning::${plan.blocked}`);
  return 0;
}
