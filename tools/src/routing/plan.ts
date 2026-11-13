// What one ci.yml run does: the lanes a diff needs under the event's policy.
//
// A pull request runs exactly what its diff can affect. A push to main or a
// merge group widens the Rust lanes to the full pool (a non-strict merge can
// combine two green diffs into a red tree). With the merge queue on, a pull
// request that reaches Rune only through the Cargo closure leaves the iOS
// simulator job to the queue. `native setup` and `rust icount` stay off a
// pull request unless their own inputs change; both events that land on main
// always run them. `just check` runs the pull-request plan locally, through
// the same recipes the jobs call.
import { list, str, table } from "../lib/value.ts";
import type { Classification } from "./classify.ts";
import { matchesAny } from "./routes.ts";

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

/**
 * The `perf-baseline` label. On a pull request it runs `rust icount`; the job
 * also reads it to authorize an edit of the checked-in baseline.
 */
export const PERF_BASELINE_LABEL = "perf-baseline";

/** Whether a pull_request payload carries `label`. Other events do not. */
export function eventHasLabel(event: unknown, label: string): boolean {
  return list(table(table(event)["pull_request"])["labels"]).some(
    (item) => str(table(item)["name"]) === label,
  );
}

/**
 * Files the `native setup` job actually executes or reads.
 *
 * Derived from `.github/workflows/ci.yml`'s `native` job, not from the broader
 * `native` surface. That job is a clean machine: it runs `test-dev-setup.sh`,
 * `setup-rust.sh`, `install-zig.sh`, and `native-smoke.sh`, and those scripts
 * source or copy the rest of this list. `native-smoke.sh` also compiles
 * `phux-core`, `phux-protocol`, `phux`, and `phux-mcp`; those crate inputs are
 * not listed here. A pull request that only changes them is covered by the
 * Rust lanes, and merge_group and main still run this job.
 *
 * `scripts/lib/apple-toolchain-env.sh` is copied into the setup fixture and
 * never sourced. The copy fails if the file is gone, so it stays on the list.
 * `clients/phux-vt-web/vendor/ghostty-vt.wasm` is not: the fixture creates its
 * own empty stand-in. `just/setup.just` is not: the job calls bash, not just.
 */
export const NATIVE_SETUP_PATHS = [
  ".github/workflows/ci.yml",
  "rust-toolchain.toml",
  "mise.toml",
  ".config/zig-toolchain.json",
  "clients/phux-web/Cargo.toml",
  "scripts/test-dev-setup.sh",
  "scripts/setup-rust.sh",
  "scripts/install-zig.sh",
  "scripts/native-smoke.sh",
  "scripts/doctor.sh",
  "scripts/check-docs.sh",
  "scripts/build-vt-wasm.sh",
  "scripts/prepare-vt-wasm.mjs",
  "scripts/smoke-vt-wasm.mjs",
  "scripts/check-ghostty-pin.sh",
  "scripts/check-ghostty-pin_test.sh",
  "scripts/lib/dev-toolchain.sh",
  "scripts/lib/output.sh",
  "scripts/lib/apple-toolchain-env.sh",
] as const;

/**
 * Bench sources and the baseline `rust icount` compares. The job builds
 * `phux-server`'s `icount` bench and `phux-terminal`'s `terminal_icount` bench
 * (`scripts/ci/perf-icount.sh`) and reads `benchmarks/icount-baseline.json`.
 * A pull request runs the job when one of these changes. Other hot-path edits
 * wait for merge_group and main, which always run it.
 */
export const ICOUNT_PR_PATHS = [
  "benchmarks/icount-baseline.json",
  "crates/phux-server/benches/icount.rs",
  "crates/phux-terminal/benches/terminal_icount.rs",
] as const;

/** What the diff is, for the two lanes a pull request may deliberately skip. */
export type PathScope =
  | { readonly kind: "diff"; readonly paths: readonly string[] }
  | { readonly kind: "full" }
  | { readonly kind: "none" };

export interface PlanOptions {
  readonly mergeQueue?: boolean;
  /**
   * Omitted and `full` fail closed (every pull-request lane). `none` is a
   * release-metadata diff: these two lanes are not selected by paths.
   */
  readonly scope?: PathScope;
  /** The pull request carries `perf-baseline`. */
  readonly perfBaseline?: boolean;
}

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

/** `push` here is main's policy: a push to main or a merge group. */
export function triggerFor(event: string): Trigger {
  return event === "push" || event === "merge_group" ? "push" : "pull_request";
}

function touches(scope: PathScope | undefined, patterns: readonly string[]): boolean {
  // No scope and a full validation fail closed. An empty diff does too: the
  // classifier cannot see a path it was not given.
  if (scope === undefined || scope.kind === "full") return true;
  if (scope.kind === "none") return false;
  if (scope.paths.length === 0) return true;
  return scope.paths.some((path) => matchesAny(path, patterns));
}

function needed(c: Classification, trigger: Trigger, options: PlanOptions): Record<Lane, boolean> {
  const push = trigger === "push";
  const rust = c.needed.has("phux");
  const fullUnit = c.unitMode === "workspace" || c.unitMode === "rdeps";
  // Pull requests run these two only for their own inputs. Main policy always
  // runs them, including a docs-only or release-metadata push: the aggregate
  // rejects a skip there.
  const icount = push || options.perfBaseline === true || touches(options.scope, ICOUNT_PR_PATHS);
  const native = push || touches(options.scope, NATIVE_SETUP_PATHS);
  return {
    guards: true,
    lint: rust,
    tests: rust && (push || c.unitMode !== "skip"),
    e2e: rust && (push || c.e2eNeeded),
    ffi: rust && (push || (c.uniffiNeeded && fullUnit)),
    icount,
    integrations: c.needed.has("integrations"),
    web: c.needed.has("web"),
    native,
    rune: c.needed.has("rune"),
  };
}

const chosen = (flags: Record<Lane, boolean>): Set<Lane> =>
  new Set(LANES.filter((lane) => flags[lane]));

/**
 * `mergeQueue`: a pull request's diff is validated again by the merge queue
 * (`vars.PHUX_MERGE_QUEUE`), so a closure-only Rune change skips iOS here.
 */
function asOptions(value: boolean | PlanOptions): PlanOptions {
  return typeof value === "boolean" ? { mergeQueue: value } : value;
}

export function plan(
  c: Classification,
  trigger: Trigger,
  mergeQueueOrOptions: boolean | PlanOptions = false,
): Plan {
  const options = asOptions(mergeQueueOrOptions);
  const push = trigger === "push";
  const mergeQueue = options.mergeQueue === true;
  const runeIos = c.needed.has("rune") && (push || !mergeQueue || c.runeDirect);
  // Coverage stays the pull-request lane set. A push that reuses a receipt
  // still runs native and icount (see `reused`), so the receipt does not claim
  // a pull request validated them when that pull request skipped them.
  const covered = needed(c, "pull_request", options);
  return {
    lanes: chosen(needed(c, trigger, options)),
    runeIos,
    coverage: [...chosen(covered), ...(runeIos ? ["rune_ios"] : [])].join(","),
    unitMode: push ? "workspace" : c.unitMode,
    unitTargets: push ? "" : c.unitTargets,
    testFilterset: push ? "" : c.testFilterset,
  };
}

/**
 * What a main push still runs when a pull request validated its exact tree.
 * A receipt never suppresses a lane main always qualifies: the binding (a PR
 * runs it only inside its closure), and native setup and rust icount (a PR
 * runs them only for their own inputs).
 */
const KEPT_ON_REUSE: ReadonlySet<Lane> = new Set(["ffi", "icount", "native"]);

export function reused(p: Plan): Plan {
  return {
    ...p,
    lanes: new Set([...p.lanes].filter((lane) => KEPT_ON_REUSE.has(lane))),
  };
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
