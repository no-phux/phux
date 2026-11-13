// What one ci.yml run does: the lanes a diff needs under the event's policy.
//
// A pull request runs exactly what its diff can affect. A push to main or a
// merge group widens the Rust lanes to the full pool (a non-strict merge can
// combine two green diffs into a red tree). Rune's macOS and iOS legs are
// not lanes here: rune-macos.yml owns them, and a ci receipt must not claim
// coverage they never ran. A pull request's Rune lane is check.sh and
// cargo-deny on linux-x86_64. Packaging and linux-aarch64 run only when the
// plan qualifies Linux, and the receipt names that coverage so a pull
// request cannot stand in for it. `just check` runs the pull-request plan
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
 * The just recipes one lane's job runs, in order. CI gives each its own step:
 * Bun leaves a shared stdout non-blocking, and a Cargo build that follows it
 * in one process panics on EAGAIN (os error 35).
 */
export const LANE_RECIPES: Record<Lane, readonly (readonly [string, ...string[]])[]> = {
  guards: [["guards"]],
  lint: [["rust-lint"]],
  tests: [["test"]],
  e2e: [["e2e"]],
  ffi: [["binding-uniffi-test"], ["ffi-abi"]],
  icount: [["perf-icount"]],
  integrations: [["agent-integrations-check"]],
  web: [["web-check"]],
  native: [["native-smoke"]],
  rune: [["rune-check"], ["rune-deny"]],
};

/** Lanes a workstation cannot reproduce: x86_64 valgrind, a clean machine. */
export const CI_ONLY: ReadonlySet<Lane> = new Set(["icount", "native"]);

/** Coverage a Linux-qualifying run adds. A pull request never writes these. */
export const LINUX_QUALIFY = ["linux-aarch64", "packaging"] as const;

export interface Plan {
  readonly lanes: ReadonlySet<Lane>;
  /**
   * Package every selected Linux host and launch the archives, including
   * linux-aarch64. False on a pull request.
   */
  readonly runeQualify: boolean;
  /**
   * The pull-request lane set, plus `linux-aarch64` and `packaging` when
   * this run qualified Linux. A pull request omits those tokens, so its
   * receipt cannot satisfy a main push. macOS is absent on purpose, so a
   * receipt cannot claim a leg this workflow did not run.
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
 * `qualify`: package and launch both Linux hosts. Defaults on for a push
 * (and therefore a merge group). A caller turns it on for a dispatch too;
 * a pull request leaves it off.
 */
export function plan(
  c: Classification,
  trigger: Trigger,
  qualify = trigger !== "pull_request",
): Plan {
  const push = trigger === "push";
  const prLanes = chosen(needed(c, "pull_request"));
  const runeQualify = qualify && prLanes.has("rune");
  return {
    lanes: chosen(needed(c, trigger)),
    runeQualify,
    coverage: [...prLanes, ...(runeQualify ? LINUX_QUALIFY : [])].join(","),
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
    rune_qualify: p.lanes.has("rune") && p.runeQualify,
    docs_only: c.docsOnly,
    web_engine: p.lanes.has("web") && c.needed.has("web_engine"),
    content: c.contentNeeded,
    unit_mode: p.unitMode,
    unit_targets: p.unitTargets,
    test_filterset: p.testFilterset,
    coverage: p.coverage,
  };
}
