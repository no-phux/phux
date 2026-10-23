// The CI routing table: which lanes a changed path can affect. Patterns are
// globs (`*` stays within a directory, `**` crosses them) and additive: a path
// collects the surfaces of every pattern it matches. A path matched by none
// requests every lane, so an unknown input fails closed. Keep `/` out of
// `{a,b}` alternatives: Bun.Glob mismatches some of them.
import { pathClosure, type BuildRoot } from "../lib/cargo.ts";
import { ROOT } from "../lib/repo.ts";

export const SURFACES = ["phux", "web", "web_engine", "integrations", "native", "desktop"] as const;
export type Surface = (typeof SURFACES)[number];
export type Route = readonly [pattern: string, surfaces: readonly Surface[]];

const SHARED: Surface[] = ["phux", "web", "native"];
const ZIG: Surface[] = ["native", "web_engine", "desktop"];

/**
 * Documentation that code or a guard reads: Rust tests byte-compare the spec,
 * generated reference, agent guide and recorded casts; the guards read
 * the root changelog. These leave the docs-only fast path.
 */
const DOC_INPUTS: Record<string, Surface[]> = {
  "docs/{spec,reference,assets}/**": ["phux"],
  "docs/agents.md": ["phux"],
  "CHANGELOG.md": [],
};

/** Shipped agent skills are product inputs, not documentation. */
const PRODUCT_PROSE = [
  "distros/crew/**",
  ".agents/skills/{using-phux,using-phux-mcp}/**",
  "integrations/*/skills/**",
  "integrations/muse/SKILL.md",
];

const ROUTES: Record<string, Surface[]> = {
  // Distro layers and manifests are exercised by native config tests; Crew's
  // credential-free launcher tests also run in guards.
  "distros/**": ["phux"],
  // Workflow infrastructure is compile-free (guards only), except ci.yml and
  // its shared actions: a change to how lanes run reruns every lane.
  ".github/workflows/*.{yml,yaml}": [],
  ".github/{actionlint.yaml,dependabot.yml}": [],
  // Commit-message policy is enforced by commitlint, not product builds.
  "commitlint.config.mjs": [],
  ".github/cockpit-history-tip": [],
  ".github/workflows/ci.yml": [...SURFACES],
  ".github/actions/**": [...SURFACES],
  "scripts/ci/setup-linux-release-userspace.sh": [],
  // Tools code runs in the lanes whose builds import it.
  "tools/**": [],
  // Passive maintainer task exports/configuration are not product inputs.
  ".beads/**": [],
  "tools/{main.ts,package.json}": ["web", "desktop"],
  "tools/src/lib/**": ["web", "desktop"],
  "tools/src/desktop/**": ["desktop"],
  "tools/src/web/**": ["web"],
  "tools/src/integrations/**": ["integrations"],
  "tools/src/cargo/build-features.ts": ["phux"],
  ".agents/skills/{using-phux,using-phux-mcp}/**": ["phux"],
  ".agents/lib/**": ["integrations"],
  "clients/{phux-web,phux-vt-web}/**": ["web"],
  "clients/phux-vt-web/vendor/**": ["web", "web_engine"],
  "scripts/build-vt-wasm.sh": ["web", "web_engine"],
  "scripts/{prepare,smoke,test}-vt-wasm.mjs": ["web", "web_engine"],
  "{integrations,.claude-plugin,.grok-plugin}/**": ["integrations"],
  "scripts/ci/agent-integrations.sh": ["integrations"],
  ".release-please-manifest.json": ["integrations"],
  // The native desktop is a separate workspace. Keep its tests and packages,
  // not unrelated root/browser/integration builds. Other desktop files can be
  // root CLI inputs (e.g. phux-protocol), so they still fail closed.
  "clients/desktop/native/**": ["desktop"],
  "just/desktop.just": ["desktop"],
  "just/web.just": ["web"],
  // Root crates reach the web and desktop lanes only through Cargo (see
  // LANE_BUILDS).
  "crates/**": ["phux"],
  "crates/**/{Cargo.toml,Cargo.lock,build.rs,*.ld,*.lds,*.c,*.h}": ["native"],
  // Golden release manifests are include_str! inputs of the updaters' tests.
  "scripts/fixtures/release-manifest/**": ["phux", "desktop"],
  // Path crates inherit `workspace = true` keys from the root manifest, and
  // the desktop build reads the root toolchain and Cargo config. Its lock is
  // its own, so the root Cargo.lock stays off the desktop lane.
  "{Cargo.toml,rust-toolchain.toml}": [...SHARED, "desktop"],
  ".cargo/**": [...SHARED, "desktop"],
  "Cargo.lock": SHARED,
  "flake.{nix,lock}": ["phux", "web"],
  // The native lane; the desktop lane provisions with setup-rust.sh too.
  "just/setup.just": ["native"],
  "scripts/{doctor,test-dev-setup,native-smoke}.sh": ["native"],
  "scripts/setup-rust.sh": ["native", "desktop"],
  // How the native, web-engine and desktop lanes get libghostty's Zig; the Nix
  // lanes use flake.nix's compiler.
  ".config/zig-toolchain.json": [...SHARED, ...ZIG],
  "scripts/install-zig.sh": ZIG,
  "scripts/lib/dev-toolchain.sh": ZIG,
  // The lint lane runs build-features-check against the real graph.
  "just/{gates,test,build}.just": ["phux"],
  // Compile-free recipes and scripts that guards or release workflows run.
  // The root justfile is imports and `default`.
  justfile: [],
  "just/{perf,release,mutation}.just": [],
  "release-please-config.json": [],
  "scripts/check-*": [],
  "scripts/{export-product-skills.sh,product-skills}": [],
  "scripts/skills-package/**": [],
  "scripts/{build-ffi-xcframework,build-mobile-ffi-android}.sh": [],
  "scripts/ci/setup-android-ndk.sh": [],
};

/**
 * What each separately built lane compiles from the Cargo graph. The web lane
 * is the browser workspaces plus the browser harness build: phux-server's
 * `ws_demo_server` example (dev dependencies) and the `phux` binary. Optional
 * dependencies are followed only when their features are enabled.
 */
export const LANE_BUILDS: readonly [Surface, readonly BuildRoot[]][] = [
  [
    "web",
    [
      { dir: "clients/phux-web", dev: true },
      { dir: "clients/phux-vt-web", dev: true },
      { dir: "crates/phux-server", dev: true },
      { dir: "crates/phux", dev: false },
    ],
  ],
  ["desktop", [{ dir: "clients/desktop/native", dev: true }]],
];

export function routes(root = ROOT): Route[] {
  const crates = LANE_BUILDS.flatMap(([lane, builds]) =>
    [...pathClosure(builds, root)]
      .filter((dir) => dir.startsWith("crates/"))
      .map((dir): Route => [`${dir}/**`, [lane]]),
  );
  return [...Object.entries(DOC_INPUTS), ...Object.entries(ROUTES), ...crates];
}

const GLOBS = new Map<string, Bun.Glob>();

export function matches(path: string, pattern: string): boolean {
  const glob = GLOBS.get(pattern) ?? new Bun.Glob(pattern);
  GLOBS.set(pattern, glob);
  return glob.match(path);
}

export const matchesAny = (path: string, patterns: readonly string[]): boolean =>
  patterns.some((pattern) => matches(path, pattern));

/** Documentation no build reads; product prose and doc inputs are not. */
export function isDoc(path: string): boolean {
  if (matchesAny(path, PRODUCT_PROSE) || matchesAny(path, Object.keys(DOC_INPUTS))) return false;
  return matchesAny(path, ["docs/**", "**/*.md"]);
}
