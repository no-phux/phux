// What one ci.yml run does: the lanes a diff needs under the event's policy.
//
// A pull request runs exactly what its diff can affect. A push to main or a
// merge group widens the Rust lanes to the full pool (a non-strict merge can
// combine two green diffs into a red tree). Native setup and rust icount are
// path-scoped only on queue-verified PRs, otherwise mandatory, and never removed by reuse.
// Rune's Apple legs belong to rune-macos.yml, not this Linux lane receipt.
// `just check` runs the PR plan through the same recipes the jobs call.
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
  ffi: [["binding-uniffi-test"]],
  icount: [["perf-icount"]],
  integrations: [["agent-integrations-check"]],
  web: [["web-vt-check"], ["web-client-check"]],
  native: [["native-smoke"]],
  rune: [["rune-check"], ["rune-deny"]],
};

/** Lanes a workstation cannot reproduce: x86_64 valgrind, a clean machine. */
export const CI_ONLY: ReadonlySet<Lane> = new Set(["icount", "native"]);

/** Full Linux qualification is absent from queue-verified fast-path PR receipts. */
export const LINUX_QUALIFY = ["linux-aarch64", "packaging"] as const;

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
 * Derived from `.github/workflows/ci.yml`'s `native` job. That job is a clean
 * machine: apt, `setup-rust.sh` (rustup), and `install-zig.sh`, then
 * `native-smoke.sh`. It does not use Nix. The Nix Rust lanes do not cover
 * these inputs.
 *
 * `native-smoke.sh` compiles with that rustup toolchain, so the lock, the
 * root manifest, `.cargo`, and crate build scripts, C sources, headers, and
 * linker scripts are inputs. A crate `Cargo.toml` is not: `Cargo.lock` is
 * what moves dependencies. `cargo test` on that machine runs
 * `scripts/test-env-runner.sh` through `.cargo/config.toml`. Merge groups and
 * main always run the job.
 *
 * `scripts/lib/apple-toolchain-env.sh` is copied into the setup fixture and
 * never sourced. The copy fails if the file is gone, so it stays on the list.
 * `clients/phux-vt-web/vendor/ghostty-vt.wasm` is not: the fixture creates its
 * own empty stand-in. `just/setup.just` is not: the job calls bash, not just.
 */
export const NATIVE_SETUP_PATHS = [
  ".github/workflows/ci.yml",
  "Cargo.toml",
  "Cargo.lock",
  ".cargo/**",
  "rust-toolchain.toml",
  "mise.toml",
  ".config/zig-toolchain.json",
  "clients/phux-web/Cargo.toml",
  "crates/**/build.rs",
  "crates/**/*.c",
  "crates/**/*.h",
  "crates/**/*.ld",
  "crates/**/*.lds",
  "scripts/test-dev-setup.sh",
  "scripts/setup-rust.sh",
  "scripts/install-zig.sh",
  "scripts/native-smoke.sh",
  "scripts/test-env-runner.sh",
  "scripts/doctor.sh",
  "scripts/check-docs.sh",
  "scripts/build-vt-wasm.sh",
  "scripts/zig-mirrors/build.zig",
  "scripts/zig-mirrors/build.zig.zon",
  "scripts/prepare-vt-wasm.mjs",
  "scripts/smoke-vt-wasm.mjs",
  "scripts/check-ghostty-pin.sh",
  "scripts/check-ghostty-pin_test.sh",
  "scripts/lib/dev-toolchain.sh",
  "scripts/lib/output.sh",
  "scripts/lib/apple-toolchain-env.sh",
] as const;

/**
 * Bench sources, the baseline, and the harness `rust icount` runs. The job
 * builds `phux-server`'s `icount` bench and `phux-terminal`'s `terminal_icount`
 * bench through `scripts/ci/perf-icount.sh` (`just perf-icount`) and compares
 * them to `benchmarks/icount-baseline.json`. A pull request also runs the job
 * when `ci.yml` changes or the PR has the `perf-baseline` label. Adding that
 * label does not start a run; the next push does. Other hot-path edits wait
 * for merge_group and main, which always run the job.
 */
export const ICOUNT_PR_PATHS = [
  "benchmarks/icount-baseline.json",
  "crates/phux-server/benches/icount.rs",
  "crates/phux-terminal/benches/terminal_icount.rs",
  "scripts/ci/perf-icount.sh",
  "just/perf.just",
  "tools/src/perf/**",
  ".github/workflows/ci.yml",
] as const;

/** What the diff is, for the two lanes a pull request may deliberately skip. */
export type PathScope =
  | { readonly kind: "diff"; readonly paths: readonly string[] }
  | { readonly kind: "full" }
  | { readonly kind: "none" };

export interface PlanOptions {
  /** Affirmative live base-branch rules evidence; unknown defaults to full qualification. */
  readonly queueVerified?: boolean;
  /** Legacy queue option; Apple qualification is now owned by rune-macos.yml. */
  readonly mergeQueue?: boolean;
  /** Request Linux qualification; only verified-queue PRs may default it off. */
  readonly runeQualify?: boolean;
  /**
   * Omitted and `full` fail closed (every pull-request lane). `none` is a
   * release-metadata diff: these two lanes are not selected by paths.
   */
  readonly scope?: PathScope;
  /** The pull request carries `perf-baseline`. */
  readonly perfBaseline?: boolean;
  /**
   * The diff adds, renames, or deletes a top-level `docs/*.md`. Omitted fails
   * closed. A release-metadata scope has no such page, so it stays off.
   */
  readonly docsCatalog?: boolean;
}

export interface Plan {
  readonly lanes: ReadonlySet<Lane>;
  readonly runeQualify: boolean;
  /**
   * PR lanes plus linux-aarch64 and packaging when Linux qualified.
   * Apple coverage belongs to its own workflow receipt.
   */
  readonly coverage: string;
  readonly unitMode: Classification["unitMode"];
  readonly unitTargets: string;
  readonly testFilterset: string;
  /** Run the website docs catalog job. Not a recipe lane and not receipt coverage. */
  readonly docsCatalog: boolean;
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

function docsCatalogNeeded(options: PlanOptions): boolean {
  if (options.docsCatalog !== undefined) return options.docsCatalog;
  return options.scope?.kind !== "none";
}

/** Main always qualifies these lanes; only PRs may use their path scope. */
function qualificationRequired(
  trigger: Trigger,
  options: PlanOptions,
  patterns: readonly string[],
): boolean {
  return trigger === "push" || options.queueVerified !== true || touches(options.scope, patterns);
}

function needed(c: Classification, trigger: Trigger, options: PlanOptions): Record<Lane, boolean> {
  const push = trigger === "push";
  const rust = c.needed.has("phux");
  const fullUnit = c.unitMode === "workspace" || c.unitMode === "rdeps";
  // Only queue-verified pull requests may path-scope these. The aggregate
  // rejects a skip on fallback PRs and main/queue, even for docs/metadata.
  const icount =
    options.perfBaseline === true || qualificationRequired(trigger, options, ICOUNT_PR_PATHS);
  const native = qualificationRequired(trigger, options, NATIVE_SETUP_PATHS);
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

/** Keep the old boolean argument compatible; it never means native-path scope. */
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
  // Coverage stays the pull-request lane set. A push that reuses a receipt
  // still runs native and icount (see `reused`), so the receipt does not claim
  // a pull request validated them when that pull request skipped them.
  const covered = needed(c, "pull_request", {
    ...options,
    // Main's receipt contract stays path-scoped; its mandatory lanes run again.
    queueVerified: push || options.queueVerified === true,
  });
  const runeQualify =
    (push || options.queueVerified !== true || options.runeQualify === true) && covered.rune;
  return {
    lanes: chosen(needed(c, trigger, options)),
    runeQualify,
    coverage: [...chosen(covered), ...(runeQualify ? LINUX_QUALIFY : [])].join(","),
    unitMode: push ? "workspace" : c.unitMode,
    unitTargets: push ? "" : c.unitTargets,
    testFilterset: push ? "" : c.testFilterset,
    docsCatalog: docsCatalogNeeded(options),
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
    rune_qualify: p.lanes.has("rune") && p.runeQualify,
    docs_only: c.docsOnly,
    docs_catalog: p.docsCatalog,
    web_engine: p.lanes.has("web") && c.needed.has("web_engine"),
    content: c.contentNeeded,
    unit_mode: p.unitMode,
    unit_targets: p.unitTargets,
    test_filterset: p.testFilterset,
    coverage: p.coverage,
  };
}
