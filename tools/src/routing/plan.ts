// What one ci.yml run does: the lanes a diff needs under the event's policy.
//
// A pull request runs exactly what its diff can affect. A push to main or a
// merge group widens the Rust lanes to the full pool (a non-strict merge can
// combine two green diffs into a red tree). With the merge queue on, a pull
// request that reaches Rune only through the Cargo closure leaves the iOS
// simulator job to the queue. `just check` runs the pull-request plan
// locally, through the same recipes the jobs call.
import type { Classification } from "./classify.ts";

export const LANES = [
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
] as const;
export type Lane = (typeof LANES)[number];
export type Trigger = "pull_request" | "push";

/**
 * The just recipes a lane runs, in order. CI gives each its own step:
 * Bun leaves a shared stdout non-blocking, and a Cargo build that follows it
 * in one process panics on EAGAIN (os error 35). The web lane runs its two
 * recipes as parallel jobs; `just web-check` still runs both, in this order.
 */
export const LANE_RECIPES: Record<Lane, readonly (readonly [string, ...string[]])[]> = {
  guards: [["guards"]],
  lint: [["rust-lint"]],
  tests: [["test"]],
  e2e: [["e2e"]],
  ffi: [["binding-uniffi-test"], ["ffi-abi"]],
  icount: [["perf-icount"]],
  integrations: [["agent-integrations-check"]],
  web: [["web-vt-check"], ["web-client-check"]],
  native: [["native-smoke"]],
  rune: [["rune-check"], ["rune-deny"]],
};

/** Lanes a workstation cannot reproduce: x86_64 valgrind, a clean machine. */
export const CI_ONLY: ReadonlySet<Lane> = new Set(["icount", "native"]);

export interface Plan {
  readonly lanes: ReadonlySet<Lane>;
  /** The Rune lane runs its iOS simulator job. */
  readonly runeIos: boolean;
  /**
   * The pull-request lane set, plus `rune_ios` when that job ran: what an
   * identical tree's receipt must cover. Main policy always counts iOS, so
   * a pull request that skipped it never stands in for a main push.
   */
  readonly coverage: string;
  readonly unitMode: Classification["unitMode"];
  readonly unitTargets: string;
  readonly testFilterset: string;
}

function needed(c: Classification, trigger: Trigger): Record<Lane, boolean> {
  const push = trigger === "push";
  const rust = c.needed.has("phux");
  const fullUnit = c.unitMode === "workspace" || c.unitMode === "rdeps";
  return {
    guards: true,
    lint: rust,
    tests: rust && (push || c.unitMode !== "skip"),
    e2e: rust && (push || c.e2eNeeded),
    ffi: rust && (push || (c.uniffiNeeded && fullUnit)),
    icount: c.perfIcountNeeded,
    integrations: c.needed.has("integrations"),
    web: c.needed.has("web"),
    native: c.needed.has("native"),
    rune: c.needed.has("rune"),
  };
}

const chosen = (flags: Record<Lane, boolean>): Set<Lane> =>
  new Set(LANES.filter((lane) => flags[lane]));

/**
 * `mergeQueue`: a pull request's diff is validated again by the merge queue
 * (`vars.PHUX_MERGE_QUEUE`), so a closure-only Rune change skips iOS here.
 */
export function plan(c: Classification, trigger: Trigger, mergeQueue = false): Plan {
  const push = trigger === "push";
  const runeIos = c.needed.has("rune") && (push || !mergeQueue || c.runeDirect);
  return {
    lanes: chosen(needed(c, trigger)),
    runeIos,
    coverage: [...chosen(needed(c, "pull_request")), ...(runeIos ? ["rune_ios"] : [])].join(","),
    unitMode: push ? "workspace" : c.unitMode,
    unitTargets: push ? "" : c.unitTargets,
    testFilterset: push ? "" : c.testFilterset,
  };
}

/**
 * What a main push still runs when a pull request validated its exact tree.
 * A receipt never suppresses the binding lane: a PR runs it only inside the
 * binding's closure, while main always qualifies it.
 */
export function reused(p: Plan): Plan {
  return { ...p, lanes: new Set([...p.lanes].filter((lane) => lane === "ffi")) };
}

/** Step outputs for ci.yml's `plan` job. */
export function planOutputs(p: Plan, c: Classification): Record<string, string | boolean> {
  return {
    ...Object.fromEntries(LANES.map((lane) => [lane, p.lanes.has(lane)])),
    rune_ios: p.lanes.has("rune") && p.runeIos,
    docs_only: c.docsOnly,
    web_engine: p.lanes.has("web") && c.needed.has("web_engine"),
    content: c.contentNeeded,
    unit_mode: p.unitMode,
    unit_targets: p.unitTargets,
    test_filterset: p.testFilterset,
    coverage: p.coverage,
  };
}
