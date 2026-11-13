// Choose which draft releases to ship, and which stale drafts to drop.
//
// release.yml is the only workflow that flips a draft public. It runs after
// each green main ci, right after Release Please cuts any new tags and notes,
// plus on a schedule, so a missed or superseded run still converges. It
// builds nothing.
import { Failure } from "../lib/command.ts";
import { writeOutputs } from "../lib/actions.ts";
import { git } from "../lib/git.ts";
import { api, gh, ghLines } from "../lib/github.ts";
import { list, str, table, type Table } from "../lib/value.ts";
import { classifyRange } from "../routing/detect.ts";

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
  const attempt = (run: Table): number => Number(run["run_attempt"] ?? 1);
  const [latest] = runs
    .map(table)
    .filter(
      (run) =>
        run["head_sha"] === sha &&
        run["head_branch"] === "main" &&
        run["event"] === "push" &&
        run["path"] === workflow,
    )
    .toSorted((a, b) => Number(b["id"]) - Number(a["id"]) || attempt(b) - attempt(a));
  if (latest === undefined || latest["status"] !== "completed") return "pending";
  return str(latest["conclusion"]) ?? "unknown";
}

// `vX.Y.Z` is phux; `<component>-vX.Y.Z` is every other release-please package.
const TAG = /^(?:([a-z0-9]+(?:-[a-z0-9]+)*)-)?v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const PENDING = new Set(["pending", "queued", "in_progress", "waiting", "requested", "missing"]);

export interface TagIdentity {
  readonly component: string;
  readonly version: Version;
}

export function parseTag(tag: string): TagIdentity | undefined {
  const match = TAG.exec(tag);
  if (match === null) return undefined;
  const version: Version = [Number(match[2]), Number(match[3]), Number(match[4])];
  return { component: match[1] ?? "phux", version };
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
    (best, row) => (best && compareVersions(row.version, best.version) <= 0 ? best : row),
    undefined,
  );
}

/** Draft tags to delete, and each component's newest draft ahead of its published release. */
function partition(rows: readonly Row[]): { delete: string[]; candidates: Row[] } {
  const remove: string[] = [];
  const candidates: Row[] = [];
  for (const group of Map.groupBy(rows, (row) => row.component).values()) {
    const latest = newest(group.filter((row) => !row.draft))?.version;
    const vsPublished = (row: Row): number => (latest ? compareVersions(row.version, latest) : 1);
    const drafts = group.filter((row) => row.draft);
    remove.push(...drafts.filter((row) => vsPublished(row) < 0).map((row) => row.tag));
    const ahead = newest(drafts.filter((row) => vsPublished(row) > 0));
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
    return identity ? [{ ...release, ...identity }] : [];
  });
  const { delete: remove, candidates } = partition(parsed);
  const plan: Plan = { publish: [], delete: remove, wait: false, fail: undefined, blocked: "" };
  if (options.dispatchTag)
    return dispatchGate(plan, parsed, candidates, ciState, options.dispatchTag);
  if (options.eventSha) {
    eventGate(plan, candidates, ciState, options.eventSha);
    if (plan.wait || plan.fail !== undefined) return plan;
  }
  return reconcile(plan, candidates, ciState);
}

/** Ship a dispatched draft only when it is the newest and its own commit's ci is green. */
function dispatchGate(
  plan: Plan,
  parsed: readonly Row[],
  candidates: readonly Row[],
  ciState: CiState,
  tag: string,
): Plan {
  const selected = parsed.find((row) => row.tag === tag);
  if (selected === undefined) plan.fail = `no release named ${tag}`;
  else if (!selected.draft) return plan;
  else if (!candidates.includes(selected)) {
    plan.fail = `${tag} is older than the published ${selected.component} release`;
  } else {
    const state = ciState(selected.sha);
    if (state === "success") plan.publish = [tag];
    else plan.fail = `ci.yml is ${state} for ${selected.sha}; ${tag} stays a draft`;
  }
  return plan;
}

/**
 * Wait or fail on the event's own commit; on green, fall through to sweep
 * every candidate. GitHub keeps one pending run per concurrency group and
 * cancels the older one, so the publish run a draft waits on can vanish before
 * it starts; the sweep lets a later event recover it. A tag still ships only
 * on a green ci run for its own commit.
 */
function eventGate(plan: Plan, candidates: readonly Row[], ciState: CiState, sha: string): void {
  const mine = candidates.filter((row) => row.sha === sha);
  if (mine.length === 0) return;
  const state = ciState(sha);
  if (PENDING.has(state)) plan.wait = true;
  else if (state !== "success") {
    plan.fail = `ci.yml is ${state} for ${sha}; not publishing ${mine.map((row) => row.tag).join(", ")}`;
  }
}

function reconcile(plan: Plan, candidates: readonly Row[], ciState: CiState): Plan {
  const blocked: string[] = [];
  for (const row of candidates) {
    const state = ciState(row.sha);
    if (state === "success") plan.publish.push(row.tag);
    else if (!PENDING.has(state)) blocked.push(`${row.tag} (ci.yml ${state} for ${row.sha})`);
  }
  if (blocked.length > 0) plan.blocked = `still draft, ci is not green: ${blocked.join("; ")}`;
  return plan;
}

export function splitLanes(tags: readonly string[]): {
  phux: string;
  integration: string[];
} {
  const lanes = { phux: "", integration: [] as string[] };
  for (const tag of tags) {
    const component = parseTag(tag)?.component;
    if (component === "phux") lanes.phux = tag;
    else if (component !== undefined) lanes.integration.push(tag);
  }
  return lanes;
}

const CI_WORKFLOW = ".github/workflows/ci.yml";
const RUNE_MACOS_WORKFLOW = ".github/workflows/rune-macos.yml";
const RELEASE_BOT = /^(?:phux-release-bot\[bot\]|phux-release\[bot\])$/m;

function workflowConclusion(repository: string, sha: string, file: string, path: string): string {
  const query = new URLSearchParams({ head_sha: sha, event: "push", per_page: "20" });
  const runs = list(
    table(api.json(`repos/${repository}/actions/workflows/${file}/runs?${query.toString()}`))[
      "workflow_runs"
    ],
  );
  return validationState(runs, sha, path);
}

function ciConclusion(repository: string, sha: string): string {
  return workflowConclusion(repository, sha, "ci.yml", CI_WORKFLOW);
}

function runeMacosConclusion(repository: string, sha: string): string {
  return workflowConclusion(repository, sha, "rune-macos.yml", RUNE_MACOS_WORKFLOW);
}

/** Why `sha` must not ship, or undefined when main's ci passed on that exact commit. */
export function ciRefusal(state: string, sha: string): string | undefined {
  if (state === "success") return undefined;
  if (PENDING.has(state)) return `no finished ci.yml push run on main for ${sha}; rerun when green`;
  return `ci.yml is ${state} on main for ${sha}`;
}

/** Why `sha` must not ship, or undefined when that commit's rune-macos push run passed. */
export function runeMacosRefusal(state: string, sha: string): string | undefined {
  if (state === "success") return undefined;
  if (PENDING.has(state))
    return `no finished rune-macos push run on main for ${sha}; rerun when green`;
  return `rune-macos is ${state} on main for ${sha}`;
}

/**
 * ci and, when `runeSha` is set, rune-macos. ci is always the release commit.
 * `runeSha` is that commit or, for a release-bot version bump that does not
 * itself build Rune, the parent whose ci already passed.
 */
export function releaseRefusal(
  ci: string,
  sha: string,
  rune: string,
  runeSha: string | undefined,
): string | undefined {
  const ciNo = ciRefusal(ci, sha);
  if (ciNo !== undefined) return ciNo;
  if (runeSha === undefined) return undefined;
  return runeMacosRefusal(rune, runeSha);
}

export interface RuneEvidence {
  readonly sha: string;
  readonly bot: boolean;
  readonly rune: boolean;
  readonly parent?: string;
}

/** True when `identity` is the release app (`git log --format=%an%n%ae`). */
export function isReleaseBot(identity: string): boolean {
  return RELEASE_BOT.test(identity);
}

/**
 * The commit whose rune-macos run proves this release, or undefined when
 * Rune is outside the closure. Select the required evidence independently
 * of its state: pending or failed parent ci must never erase the requirement.
 */
export function runeMacosTarget(
  commit: RuneEvidence,
  parent: RuneEvidence | undefined,
): string | undefined {
  if (commit.rune) return commit.sha;
  if (commit.bot && parent !== undefined && commit.parent === parent.sha && parent.rune) {
    return parent.sha;
  }
  return undefined;
}

/** Conjoin the inherited parent's ci and native proof; missing proof is not a waiver. */
export function runeMacosProof(
  commit: RuneEvidence,
  parent: RuneEvidence | undefined,
  parentCi: string,
  native: string,
): { evidence: string; state: string } | undefined {
  const target = runeMacosTarget(commit, parent);
  if (target === undefined) return undefined;
  const state = target === commit.sha || parentCi === "success" ? native : parentCi;
  return { evidence: target, state };
}

function evidence(sha: string): RuneEvidence {
  const identity = git(["log", "-1", "--format=%an%n%ae", sha]).trim();
  let parent: string | undefined;
  try {
    parent = git(["rev-parse", "--verify", `${sha}^`]).trim();
  } catch {
    parent = undefined;
  }
  const rune = parent === undefined ? true : classifyRange(parent, sha).needed.has("rune");
  const row: RuneEvidence = { sha, bot: isReleaseBot(identity), rune };
  return parent === undefined ? row : { ...row, parent };
}

function runeMacosOn(
  repository: string,
  sha: string,
): { evidence: string; state: string } | undefined {
  const commit = evidence(sha);
  if (commit.rune) return { evidence: sha, state: runeMacosConclusion(repository, sha) };
  if (!commit.bot || commit.parent === undefined) return undefined;
  const parent = evidence(commit.parent);
  const target = runeMacosTarget(commit, parent);
  if (target === undefined) return undefined;
  return runeMacosProof(
    commit,
    parent,
    ciConclusion(repository, commit.parent),
    runeMacosConclusion(repository, target),
  );
}

/**
 * Drop or hold tags whose rune-macos proof is missing. A pending or red
 * proof of the event's own commit stops the whole publish, the way ci does.
 * Any other draft stays a draft and is named on `blocked`.
 */
export function applyRuneGate(
  plan: Plan,
  strictSha: string | undefined,
  lookup: (sha: string) => { evidence: string; state: string } | undefined,
  shaOf: (tag: string) => string,
): Plan {
  if (plan.fail !== undefined || plan.wait) return plan;
  if (strictSha !== undefined) {
    const own = plan.publish.filter((tag) => shaOf(tag) === strictSha);
    if (own.length > 0) {
      const rune = lookup(strictSha);
      if (rune !== undefined && rune.state !== "success") {
        if (PENDING.has(rune.state)) return { ...plan, publish: [], wait: true };
        return {
          ...plan,
          publish: [],
          fail: `rune-macos is ${rune.state} for ${rune.evidence}; not publishing ${own.join(", ")}`,
        };
      }
    }
  }
  const publish: string[] = [];
  const held: string[] = [];
  for (const tag of plan.publish) {
    const rune = lookup(shaOf(tag));
    if (rune === undefined || rune.state === "success") publish.push(tag);
    else held.push(`${tag} (rune-macos ${rune.state} for ${rune.evidence})`);
  }
  const note = held.length === 0 ? "" : `still draft, rune-macos is not green: ${held.join("; ")}`;
  const blocked = note === "" ? plan.blocked : plan.blocked ? `${plan.blocked}; ${note}` : note;
  return { ...plan, publish, blocked };
}

/** Fail a tag whose commit main hasn't proved: rune-client.yml's preflight. */
export function ciGreen(sha: string): number {
  const repository = process.env["GITHUB_REPOSITORY"];
  if (!repository) throw new Failure("GITHUB_REPOSITORY is unset");
  const rune = runeMacosOn(repository, sha);
  const refusal = releaseRefusal(
    ciConclusion(repository, sha),
    sha,
    rune?.state ?? "success",
    rune?.evidence,
  );
  if (refusal === undefined) return 0;
  console.error(`::error::${refusal}`);
  return 1;
}

const tagSha = (tag: string): string =>
  git(["rev-parse", "--verify", `refs/tags/${tag}^{commit}`], process.cwd()).trim();

function releases(repository: string): Release[] {
  const jq = ".[] | {tag:.tag_name,draft:.draft}";
  return ghLines(["api", `repos/${repository}/releases`, "--paginate", "--jq", jq])
    .map(table)
    .map((row) => ({ tag: str(row["tag"]) ?? "", draft: row["draft"] === true }))
    .filter((row) => parseTag(row.tag) !== undefined)
    .map((row) => ({ ...row, sha: tagSha(row.tag) }));
}

/** release.yml's `plan` job: env in, Actions outputs out. */
export function main(): number {
  const env = process.env;
  const repository = env["GITHUB_REPOSITORY"];
  if (!repository) throw new Failure("GITHUB_REPOSITORY is unset");
  const dispatch = (env["DISPATCH_TAG"] ?? "").trim();
  const sha = (env["EVENT_SHA"] ?? "").trim();
  const eventSha =
    env["GITHUB_EVENT_NAME"] === "workflow_run" && sha && !dispatch ? sha : undefined;

  const decided = decide(releases(repository), (commit) => ciConclusion(repository, commit), {
    eventSha,
    dispatchTag: dispatch || undefined,
  });
  const strict =
    eventSha ?? (dispatch && decided.fail === undefined ? tagSha(dispatch) : undefined);
  const waitingOnCi = decided.wait;
  const plan = applyRuneGate(decided, strict, (commit) => runeMacosOn(repository, commit), tagSha);
  for (const tag of plan.delete) {
    gh(["release", "delete", tag, "--repo", repository, "--yes"]);
    console.log(`deleted superseded draft ${tag}`);
  }
  if (plan.fail !== undefined) {
    console.error(`::error::${plan.fail}`);
    return 1;
  }

  const lanes = splitLanes(plan.publish);
  writeOutputs({
    phux_tag: lanes.phux,
    integration_tags: JSON.stringify(lanes.integration),
  });
  if (waitingOnCi)
    console.log(`ci.yml still running for ${eventSha}; publish retries when it ends`);
  else if (plan.wait) console.log("rune-macos still running; publish retries when it ends");
  else console.log(`publishing ${plan.publish.join(", ") || "nothing"}`);
  if (plan.blocked) console.log(`::warning::${plan.blocked}`);
  return 0;
}
