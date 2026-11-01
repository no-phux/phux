// One CI routing contract: changed paths in, lane outputs out.
//
// `tests/`, `benches/` and `examples/` are not binary inputs, so they do not
// rebuild the browser, native setup or desktop. `test_filterset` is the PR
// rdeps expression for a library change; empty means the full pool.
// `unit_mode=narrow` builds one integration-test binary instead. Pushes to
// main ignore `unit_mode` and keep the full pool.
import { existsSync } from "node:fs";
import { join } from "node:path";
import { pathClosure, readManifest, workspaceMembers } from "../lib/cargo.ts";
import { ROOT } from "../lib/repo.ts";
import { strings, table } from "../lib/value.ts";
import {
  isDoc,
  matches,
  matchesAny,
  routes,
  SURFACES,
  type Route,
  type Surface,
} from "./routes.ts";

export type UnitMode = "workspace" | "rdeps" | "narrow" | "skip";

export interface Classification {
  readonly needed: ReadonlySet<Surface>;
  readonly docsOnly: boolean;
  readonly perfIcountNeeded: boolean;
  readonly mobileFfiNeeded: boolean;
  readonly uniffiNeeded: boolean;
  readonly contentNeeded: boolean;
  readonly testFilterset: string;
  readonly unitMode: UnitMode;
  readonly unitTargets: string;
  readonly e2eNeeded: boolean;
}

type UnitPlan = Pick<Classification, "unitMode" | "unitTargets" | "e2eNeeded">;

const NON_LIBRARY = new Set(["tests", "benches", "examples"]);
const NON_LIBRARY_SURFACES = new Set<Surface>(["phux", "integrations"]);
// nextest package matchers treat an unprefixed name as a glob; `=` is exact.
const CRATE_NAME = /^[A-Za-z0-9_-]+$/;
const SKIP: UnitPlan = { unitMode: "skip", unitTargets: "", e2eNeeded: false };
const WORKSPACE: UnitPlan = { unitMode: "workspace", unitTargets: "", e2eNeeded: true };

/** An empty diff fails closed: every lane, the full pool. */
const EVERYTHING: Classification = {
  needed: new Set(SURFACES),
  docsOnly: false,
  perfIcountNeeded: true,
  mobileFfiNeeded: true,
  uniffiNeeded: true,
  contentNeeded: true,
  testFilterset: "",
  ...WORKSPACE,
};

/** The instruction-count guard's inputs. */
const PERF_ICOUNT_PATHS = [
  "crates/{phux-server,phux-terminal,phux-resource,phux-agent-session,phux-protocol,phux-client-core,phux-core}/**",
  "{Cargo.toml,Cargo.lock,rust-toolchain.toml,flake.nix,flake.lock}",
  ".cargo/**",
  "benchmarks/icount-baseline.json",
  "scripts/ci/perf-icount.sh",
  "tools/src/{perf,lib}/**",
  "tools/main.ts",
  "just/perf.just",
  ".github/workflows/ci.yml",
];

/**
 * What no-phux/website reads at build time and the no-phux/skills mirror
 * exports: a green main push touching these republishes both.
 */
export const CONTENT_PATHS = [
  "docs/**",
  ".agents/skills/{using-phux,using-phux-mcp,using-phux-tools}/**",
  "scripts/{install,install-desktop,export-product-skills}.sh",
  "scripts/product-skills",
  "scripts/skills-package/**",
];

/**
 * The mobile FFI artifacts ffi-xcframework.yml and ffi-android.yml build on
 * main: phux-client-ffi's C ABI and UniFFI closures, its builders, and both workflows.
 * Not a lane, so an unknown path does not rebuild them.
 */
function mobileFfiPaths(root: string): string[] {
  return [
    ...[
      ...pathClosure([{ dir: "crates/phux-client-ffi", dev: false, features: ["uniffi"] }], root),
    ].map((dir) => `${dir}/**`),
    "scripts/{build-ffi-xcframework,build-mobile-ffi-xcframework,build-mobile-ffi-android}.sh",
    "scripts/ci/setup-android-ndk.sh",
    ".github/workflows/ffi-{android,xcframework}.yml",
  ];
}

export class Classifier {
  private readonly table: Route[];
  private readonly mobileFfi: string[];
  private readonly uniffiCrates: ReadonlySet<string>;
  private readonly packages: { name: string; dir: string }[];

  constructor(private readonly root = ROOT) {
    this.table = routes(root);
    this.mobileFfi = mobileFfiPaths(root);
    this.uniffiCrates = pathClosure(
      [{ dir: "crates/phux-client-ffi", dev: true, features: ["uniffi"], defaultFeatures: false }],
      root,
    );
    this.packages = workspaceMembers(".", root)
      .flatMap(({ name, dir }) => (name && CRATE_NAME.test(name) ? [{ name, dir }] : []))
      // Longest directory first, so a nested crate wins over its parent.
      .sort((left, right) => right.dir.length - left.dir.length);
  }

  classify(paths: readonly string[]): Classification {
    const files = paths.filter((path) => path !== "");
    if (files.length === 0) return EVERYTHING;
    const productive = files.filter((path) => !isDoc(path));
    const testFilterset = this.testFilterset(productive);
    return {
      needed: new Set(productive.flatMap((path) => this.surfaces(path))),
      docsOnly: productive.length === 0,
      perfIcountNeeded: files.some((path) => matchesAny(path, PERF_ICOUNT_PATHS)),
      mobileFfiNeeded: files.some((path) => matchesAny(path, this.mobileFfi)),
      uniffiNeeded: this.uniffiNeeded(productive),
      contentNeeded: files.some((path) => matchesAny(path, CONTENT_PATHS)),
      testFilterset,
      ...this.unitPlan(productive, testFilterset),
    };
  }

  /** Skip only known crates outside the binding's test dependency closure. */
  private uniffiNeeded(productive: readonly string[]): boolean {
    return productive.some((path) => {
      const crate = this.packages.find(({ dir }) => path.startsWith(`${dir}/`));
      return crate === undefined || this.uniffiCrates.has(crate.dir);
    });
  }

  private surfaces(path: string): readonly Surface[] {
    const matched = this.table.filter(([pattern]) => matches(path, pattern));
    // An explicit empty route is cheap (guards only); an unknown path
    // fails closed into every surface.
    const surfaces = matched.length === 0 ? SURFACES : matched.flatMap(([, lanes]) => lanes);
    if (nonLibraryKind(path) === undefined) return surfaces;
    return ["phux", ...surfaces.filter((surface) => NON_LIBRARY_SURFACES.has(surface))];
  }

  /** nextest `-E` expression, or empty to run the full unit pool. */
  private testFilterset(productive: readonly string[]): string {
    const crates = new Set<string>();
    for (const path of productive) {
      const crate = this.packages.find(({ dir }) => path === dir || path.startsWith(`${dir}/`));
      if (crate === undefined) return "";
      crates.add(crate.name);
    }
    return [...crates]
      .sort()
      .map((name) => `rdeps(=${name})`)
      .join(" + ");
  }

  private unitPlan(productive: readonly string[], testFilterset: string): UnitPlan {
    if (productive.length === 0) return SKIP;
    // The generator is compiled by the lint lane. Its own workflow runs its
    // tests. A diff that is only that tree must not pull the workspace pool,
    // e2e, or the binding lane. The ci aggregate still runs.
    if (productive.every((path) => path.startsWith("scripts/plugin-index/"))) return SKIP;
    if (productive.some((path) => nonLibraryKind(path) === undefined)) {
      return testFilterset === "" ? WORKSPACE : { ...WORKSPACE, unitMode: "rdeps" };
    }
    if (productive.every((path) => nonLibraryKind(path) !== "tests")) return SKIP;
    const targets = productive.map((path) => this.narrowTarget(path));
    if (!targets.every((target) => target !== undefined)) return WORKSPACE;
    return {
      unitMode: "narrow",
      unitTargets: [...new Set(targets)].sort().join(","),
      e2eNeeded: productive.some(isE2ePath),
    };
  }

  /** `package:binary[:features]`, `package:*` for every test, or undefined. */
  private narrowTarget(path: string): string | undefined {
    if (nonLibraryKind(path) !== "tests") return undefined;
    const [, dir = "", , first = "", ...rest] = path.split("/");
    const name = this.packages.find((pkg) => pkg.dir === `crates/${dir}`)?.name;
    if (name === undefined) return undefined;
    const binary = first.replace(/\.rs$/, "");
    const single =
      (rest.length === 0 && first.endsWith(".rs")) ||
      existsSync(join(this.root, "crates", dir, "tests", binary, "main.rs"));
    if (!single) return `${name}:*`;
    const features = gatingFeatures(dir, binary, this.root);
    return [name, binary, ...(features ? [features] : [])].join(":");
  }
}

function nonLibraryKind(path: string): string | undefined {
  const parts = path.split("/");
  const kind = parts[2] ?? "";
  return parts.length >= 4 && parts[0] === "crates" && NON_LIBRARY.has(kind) ? kind : undefined;
}

function isE2ePath(path: string): boolean {
  return path.split("/").some((part) => {
    const stem = part.replace(/\.rs$/, "");
    return stem === "e2e" || stem.endsWith("_e2e") || stem.startsWith("e2e_");
  });
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

/** `detect-changes` and `classify` output lines, one per classification field. */
export function toOutputs(result: Classification): Record<string, string | boolean> {
  return {
    ...Object.fromEntries(
      SURFACES.map((surface) => [`${surface}_needed`, result.needed.has(surface)]),
    ),
    docs_only: result.docsOnly,
    perf_icount_needed: result.perfIcountNeeded,
    mobile_ffi_needed: result.mobileFfiNeeded,
    uniffi_needed: result.uniffiNeeded,
    content_needed: result.contentNeeded,
    test_filterset: result.testFilterset,
    unit_mode: result.unitMode,
    unit_targets: result.unitTargets,
    e2e_needed: result.e2eNeeded,
  };
}
