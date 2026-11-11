// The CI routing table: which lanes a changed path can affect. Patterns are
// globs (`*` stays within a directory, `**` crosses them) and additive: a path
// collects the surfaces of every pattern it matches. A path matched by none
// requests every lane, so an unknown input fails closed. Keep `/` out of
// `{a,b}` alternatives: Bun.Glob mismatches some of them.
import { pathClosure, type BuildRoot } from "../lib/cargo.ts";
import { ROOT } from "../lib/repo.ts";

export const SURFACES = ["phux", "web", "web_engine", "integrations", "native", "rune"] as const;
export type Surface = (typeof SURFACES)[number];
export type Route = readonly [pattern: string, surfaces: readonly Surface[]];

const SHARED: Surface[] = ["phux", "web", "native"];
const ZIG: Surface[] = ["native", "web_engine"];

/**
 * Documentation that code or a guard reads: Rust tests byte-compare the spec,
 * generated reference, agent guide and recorded casts; the guards read
 * the root changelog. These leave the docs-only fast path.
 */
const DOC_INPUTS: Record<string, Surface[]> = {
  // README and the wire schema are Rust inputs. Other files under
  // docs/spec, including the plugin index schema, are documentation.
  "README.md": ["phux"],
  "docs/spec/wire-schema.json": ["phux"],
  "docs/{reference,assets}/**": ["phux"],
  "docs/agents.md": ["phux"],
  // examples_e2e runs this page's automation example.
  "docs/scripting.md": ["phux"],
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
  // First-party plugins and runnable CLI examples are native test inputs.
  "plugins/**": ["phux"],
  "examples/**": ["phux"],
  "scripts/examples-smoke.sh": ["phux"],
  "scripts/agent-verify/**": ["phux"],
  "scripts/test-agent-examples.py": ["phux"],
  "scripts/{demo-record,record-readme}.sh": ["phux"],
  // Workflow infrastructure is compile-free (guards only), except ci.yml and
  // its shared actions: a change to how lanes run reruns every lane.
  ".github/workflows/*.{yml,yaml}": [],
  ".github/{actionlint.yaml,dependabot.yml}": [],
  // Commit-message policy is enforced by commitlint, not product builds.
  "commitlint.config.mjs": [],
  ".github/cockpit-history-tip": [],
  // Issue forms are GitHub metadata, not a product input.
  ".github/ISSUE_TEMPLATE/**": [],
  ".github/workflows/ci.yml": [...SURFACES],
  ".github/actions/**": [...SURFACES],
  "scripts/ci/setup-linux-release-userspace.sh": [],
  "scripts/ci/prune-build-cache.sh": [],
  // The separate Windows WSL workflow exercises PowerShell and real WSL;
  // these distribution scripts do not change the Unix Rust build.
  "scripts/{install-windows,test-install-windows,test-windows-wsl}.ps1": [],
  "scripts/test-windows-mcp.py": [],
  "scripts/test-windows-terminal.py": [],
  // Tools code runs in the lanes whose builds import it.
  "tools/**": [],
  // Passive maintainer task exports/configuration are not product inputs.
  ".beads/**": [],
  "tools/{main.ts,package.json}": ["web"],
  "tools/src/lib/**": ["web"],
  "tools/src/web/**": ["web"],
  "tools/src/integrations/**": ["integrations"],
  "tools/src/cargo/build-features.ts": ["phux"],
  ".agents/skills/{using-phux,using-phux-mcp}/**": ["phux"],
  ".agents/lib/**": ["integrations"],
  "clients/{phux-web,phux-vt-web}/**": ["web"],
  // The Rune client has its own workspace, lock and lane (rune-build.yml).
  "clients/rune/**": ["rune"],
  ".github/workflows/rune-build.yml": ["rune"],
  "just/rune.just": ["rune"],
  // What that lane packages and qualifies with, outside its tree.
  "scripts/check-binary-portability.sh": ["rune"],
  "clients/phux-vt-web/vendor/**": ["web", "web_engine"],
  "scripts/build-vt-wasm.sh": ["web", "web_engine"],
  "scripts/{prepare,smoke,test}-vt-wasm.mjs": ["web", "web_engine"],
  "{integrations,.claude-plugin,.grok-plugin}/**": ["integrations"],
  "scripts/ci/agent-integrations.sh": ["integrations"],
  ".release-please-manifest.json": ["integrations"],
  "just/web.just": ["web"],
  // Root crates reach the web and Rune lanes only through Cargo (see
  // LANE_BUILDS).
  "crates/**": ["phux"],
  "crates/**/{Cargo.toml,Cargo.lock,build.rs,*.ld,*.lds,*.c,*.h}": ["native"],
  // Golden release manifests are include_str! inputs of the updaters' tests.
  "scripts/fixtures/release-manifest/**": ["phux"],
  // Path crates inherit `workspace = true` keys from the root manifest, and
  // the Rune build reads the root toolchain and Cargo config. Its lock is
  // its own, so the root Cargo.lock stays off the Rune lane.
  "{Cargo.toml,rust-toolchain.toml}": [...SHARED, "rune"],
  ".cargo/**": [...SHARED, "rune"],
  "Cargo.lock": SHARED,
  "flake.{nix,lock}": ["phux", "web"],
  // The native lane; the Rune lane provisions with setup-rust.sh too.
  "just/setup.just": ["native"],
  "scripts/{doctor,test-dev-setup,native-smoke}.sh": ["native"],
  "scripts/setup-rust.sh": ["native", "rune"],
  // How the native and web-engine lanes get libghostty's Zig; the Nix
  // lanes use flake.nix's compiler.
  ".config/zig-toolchain.json": [...SHARED, ...ZIG],
  "scripts/install-zig.sh": [...ZIG, "rune"],
  "scripts/lib/dev-toolchain.sh": ZIG,
  // The lint lane runs build-features-check against the real graph.
  "just/{gates,test,build}.just": ["phux"],
  // Compile-free recipes and scripts that guards or release workflows run.
  // The root justfile is imports and `default`.
  justfile: [],
  "just/{perf,release,mutation}.just": [],
  "release-please-config.json": [],
  "scripts/check-*": [],
  // Lint lane only for a generator-only diff (classify skips the unit pool).
  // plugin-index.yml runs the generator tests. A phux-config change still
  // compiles this tree because the rust lane cargo-checks it.
  "scripts/plugin-index/**": ["phux"],
  // Brand release plumbing is verified by guards. Changed application artwork
  // still routes through its destination's packaging/build checks.
  "brand.{config,lock}.json": [],
  "scripts/{brand,update-brand,test_brand_update}.py": [],
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
  ["rune", [{ dir: "clients/rune", dev: true }]],
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
