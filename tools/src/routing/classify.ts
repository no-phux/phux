// One CI routing contract: changed paths in, lane outputs out.
//
// `tests/`, `benches/` and `examples/` are not binary inputs, so they do not
// rebuild the browser, native setup or desktop. Native is the clean setup
// assurance lane, not all native Rust. Desktop is the GPUI host under
// clients/desktop/native: its own Cargo workspace and lock, built against the
// root crates it names by path.
//
// `test_filterset` is the PR rdeps expression for a library change; empty
// means the full pool. `unit_mode=narrow` builds one integration-test binary
// instead. Pushes to main ignore `unit_mode` and keep the full pool.
import { existsSync } from "node:fs";
import { join } from "node:path";
import { pathClosure, readManifest, workspaceMembers } from "../lib/cargo.ts";
import { ROOT } from "../lib/repo.ts";
import { strings, table } from "../lib/value.ts";
import { isDoc, matchesAny, routes, SURFACES, type Route, type Surface } from "./routes.ts";

export type UnitMode = "workspace" | "rdeps" | "narrow" | "skip";

export interface Classification {
  readonly needed: Record<Surface, boolean>;
  readonly docsOnly: boolean;
  readonly perfIcountNeeded: boolean;
  readonly mobileFfiNeeded: boolean;
  readonly testFilterset: string;
  readonly unitMode: UnitMode;
  readonly unitTargets: string;
  readonly e2eNeeded: boolean;
}

interface Package {
  readonly name: string;
  readonly dir: string;
}

const NON_LIBRARY = new Set(["tests", "benches", "examples"]);
const DROP_FOR_NON_LIBRARY = new Set<Surface>(["web", "web_engine", "native", "desktop"]);
// nextest package matchers treat an unprefixed name as a glob; `=` is exact.
const CRATE_NAME = /^[A-Za-z0-9_-]+$/;

/**
 * The instruction-count guard's inputs. Docs and unrelated crates (the TUI,
 * for example) stay off this lane; an empty diff fails closed.
 */
const PERF_ICOUNT_PATHS = [
  "crates/phux-server/**",
  "crates/phux-protocol/**",
  "crates/phux-client-core/**",
  "crates/phux-core/**",
  "Cargo.toml",
  "Cargo.lock",
  "rust-toolchain.toml",
  ".cargo/**",
  "flake.nix",
  "flake.lock",
  "benchmarks/icount-baseline.json",
  "scripts/ci/perf-icount.sh",
  "tools/src/perf/**",
  "tools/src/lib/**",
  "tools/main.ts",
  "just/perf.just",
  ".github/workflows/ci.yml",
];

/**
 * The mobile FFI artifacts, which ffi-xcframework.yml and ffi-android.yml
 * build on main: phux-mobile pins one revision and fetches both, so both
 * workflows read this one output. phux-client-ffi's build closure, its
 * builders, and the two workflows. Not a lane: an unknown path does not
 * rebuild the artifacts.
 */
function mobileFfiPaths(root: string): string[] {
  const crates = [...pathClosure([{ dir: "crates/phux-client-ffi", dev: false }], root)];
  return [
    ...crates.toSorted().map((dir) => `${dir}/**`),
    "scripts/build-ffi-xcframework.sh",
    "scripts/build-mobile-ffi-xcframework.sh",
    "scripts/build-mobile-ffi-android.sh",
    "scripts/ci/setup-android-ndk.sh",
    ".github/workflows/ffi-android.yml",
    ".github/workflows/ffi-xcframework.yml",
  ];
}

export class Classifier {
  private readonly table: Route[];
  private readonly packages: Package[];
  private readonly mobileFfi: string[];

  constructor(private readonly root = ROOT) {
    this.table = routes(root);
    this.mobileFfi = mobileFfiPaths(root);
    this.packages = workspaceMembers(".", root)
      .flatMap(({ name, dir }) => (name && CRATE_NAME.test(name) ? [{ name, dir }] : []))
      // Longest directory first, so a nested crate wins over its parent.
      .sort((left, right) => right.dir.length - left.dir.length);
  }

  /** The lanes one path can affect. */
  surfaces(path: string): Set<Surface> {
    if (isDoc(path)) return new Set();
    const selected = new Set<Surface>();
    let matched = false;
    for (const route of this.table) {
      if (!matchesAny(path, route.paths)) continue;
      matched = true;
      for (const surface of route.surfaces) selected.add(surface);
    }
    // An explicit empty route is cheap (workflow gate only). An unknown path
    // fails closed into every surface.
    const surfaces = matched ? selected : new Set(SURFACES);
    if (nonLibraryKind(path) === undefined) return surfaces;
    const kept = new Set([...surfaces].filter((surface) => !DROP_FOR_NON_LIBRARY.has(surface)));
    return kept.add("phux");
  }

  classify(paths: readonly string[]): Classification {
    const files = paths.filter((path) => path !== "");
    const selected = new Set<Surface>(files.length === 0 ? SURFACES : []);
    for (const path of files) for (const surface of this.surfaces(path)) selected.add(surface);
    const needed = Object.fromEntries(SURFACES.map((surface) => [surface, selected.has(surface)]));
    return {
      // SAFETY: built from SURFACES, so every Surface key is present.
      needed: needed as Record<Surface, boolean>,
      docsOnly: files.length > 0 && files.every(isDoc),
      perfIcountNeeded:
        files.length === 0 || files.some((path) => matchesAny(path, PERF_ICOUNT_PATHS)),
      mobileFfiNeeded: files.length === 0 || files.some((path) => matchesAny(path, this.mobileFfi)),
      testFilterset: this.testFilterset(files),
      ...this.unitPlan(files),
    };
  }

  /** nextest `-E` expression, or empty to run the full unit pool. */
  testFilterset(files: readonly string[]): string {
    const crates = new Set<string>();
    for (const path of files) {
      if (isDoc(path)) continue;
      const crate = this.crateFor(path);
      if (crate === undefined) return "";
      crates.add(crate.name);
    }
    return [...crates]
      .sort()
      .map((name) => `rdeps(=${name})`)
      .join(" + ");
  }

  private unitPlan(
    files: readonly string[],
  ): Pick<Classification, "unitMode" | "unitTargets" | "e2eNeeded"> {
    const plan = (unitMode: UnitMode, unitTargets: string, e2eNeeded: boolean) => ({
      unitMode,
      unitTargets,
      e2eNeeded,
    });
    if (files.length === 0) return plan("workspace", "", true);
    const productive = files.filter((path) => !isDoc(path));
    if (productive.length === 0) return plan("skip", "", false);
    if (productive.some((path) => nonLibraryKind(path) === undefined)) {
      return plan(this.testFilterset(files) === "" ? "workspace" : "rdeps", "", true);
    }
    if (productive.every((path) => nonLibraryKind(path) !== "tests"))
      return plan("skip", "", false);
    const targets = this.narrowTargets(productive);
    if (targets === undefined) return plan("workspace", "", true);
    return plan("narrow", [...new Set(targets)].sort().join(","), productive.some(isE2ePath));
  }

  /** `package:binary[:features]` per path, or undefined to fall back to the workspace. */
  private narrowTargets(paths: readonly string[]): string[] | undefined {
    const targets: string[] = [];
    for (const path of paths) {
      const target =
        nonLibraryKind(path) === "tests" ? integrationTarget(path, this.root) : undefined;
      const name = target && this.packages.find((pkg) => pkg.dir === `crates/${target.dir}`)?.name;
      if (!target || !name) return undefined;
      const features =
        target.binary === "*" ? "" : gatingFeatures(target.dir, target.binary, this.root);
      targets.push([name, target.binary, ...(features ? [features] : [])].join(":"));
    }
    return targets;
  }

  private crateFor(path: string): Package | undefined {
    return this.packages.find(({ dir }) => path === dir || path.startsWith(`${dir}/`));
  }
}

function nonLibraryKind(path: string): string | undefined {
  const parts = path.split("/");
  if (parts.length < 4 || parts[0] !== "crates" || !NON_LIBRARY.has(parts[2] ?? ""))
    return undefined;
  return parts[2];
}

function isE2ePath(path: string): boolean {
  return path.split("/").some((part) => {
    const stem = part.endsWith(".rs") ? part.slice(0, -3) : part;
    return stem === "e2e" || stem.endsWith("_e2e") || stem.startsWith("e2e_");
  });
}

/** The integration-test binary a tests/ path belongs to; `*` means every one. */
function integrationTarget(
  path: string,
  root: string,
): { dir: string; binary: string } | undefined {
  const [, dir = "", , first = "", ...rest] = path.split("/");
  const stem = first.endsWith(".rs") ? first.slice(0, -3) : first;
  if (rest.length === 0 && first.endsWith(".rs")) return { dir, binary: stem };
  if (existsSync(join(root, "crates", dir, "tests", stem, "main.rs"))) return { dir, binary: stem };
  return { dir, binary: "*" };
}

/**
 * Non-default `required-features` of one `[[test]]` target, `+`-joined. A
 * gated target is skipped unless its features are on, so the narrow lane
 * names them to keep building the target the diff touched.
 */
function gatingFeatures(dir: string, binary: string, root: string): string {
  const { document } = readManifest(`crates/${dir}`, root);
  const defaults = new Set(strings(table(document["features"])["default"]));
  const target = (Array.isArray(document["test"]) ? document["test"] : [])
    .map(table)
    .find((entry) => entry["name"] === binary);
  return strings(target?.["required-features"])
    .filter((feature) => !defaults.has(feature))
    .join("+");
}

/** The step outputs `.github/actions/classify-changes` declares. */
export function toOutputs(result: Classification): Record<string, string | boolean> {
  return {
    ...Object.fromEntries(SURFACES.map((surface) => [`${surface}_needed`, result.needed[surface]])),
    docs_only: result.docsOnly,
    perf_icount_needed: result.perfIcountNeeded,
    mobile_ffi_needed: result.mobileFfiNeeded,
    test_filterset: result.testFilterset,
    unit_mode: result.unitMode,
    unit_targets: result.unitTargets,
    e2e_needed: result.e2eNeeded,
  };
}
