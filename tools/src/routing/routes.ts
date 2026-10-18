// The CI routing table: which lanes a changed path can affect. Patterns are
// globs (`*` stays within a directory, `**` crosses them). A path matched by
// no route requests every lane, so an unknown input fails closed.
import { pathClosure, type BuildRoot } from "../lib/cargo.ts";
import { ROOT } from "../lib/repo.ts";

export const SURFACES = ["phux", "web", "web_engine", "integrations", "native", "desktop"] as const;
export type Surface = (typeof SURFACES)[number];

export interface Route {
  readonly paths: readonly string[];
  readonly surfaces: readonly Surface[];
}

const RUST: Surface[] = ["phux"];
const SHARED: Surface[] = [...RUST, "web", "native"];
const ZIG: Surface[] = [...SHARED, "web_engine", "desktop"];

/**
 * Documentation that code or a guard reads as an input. Each leaves the
 * docs-only fast path and routes like its reader: Rust tests byte-compare
 * the spec, generated reference, agent guide and recorded casts; the full
 * workflow gate reads the root changelog. A test fails when a crate starts
 * reading a doc missing here.
 */
export const DOC_INPUTS: readonly Route[] = [
  {
    paths: ["docs/spec/**", "docs/reference/**", "docs/assets/**", "docs/consumers/agents.md"],
    surfaces: RUST,
  },
  { paths: ["CHANGELOG.md"], surfaces: [] },
];

/** Skills are executable product inputs, shipped agent prompts included. */
const PRODUCT_PROSE = [
  ".agents/skills/using-phux/**",
  ".agents/skills/using-phux-mcp/**",
  "integrations/*/skills/**",
];
const DOCS = ["docs/**", "**/*.md"];

/**
 * What each separately built lane compiles from the Cargo graph. The web lane
 * is the browser workspaces plus the browser harness build: phux-server's
 * `ws_demo_server` example (dev dependencies) and the `phux` binary.
 */
export const LANE_BUILDS: Readonly<Partial<Record<Surface, readonly BuildRoot[]>>> = {
  web: [
    { dir: "clients/phux-web", dev: true },
    { dir: "clients/phux-vt-web", dev: true },
    { dir: "crates/phux-server", dev: true },
    { dir: "crates/phux", dev: false },
  ],
  desktop: [{ dir: "clients/desktop/native", dev: true }],
};

/** `crates/<dir>/**` routes for every root crate a lane's build compiles. */
export function crateRoutes(root = ROOT): Route[] {
  return Object.entries(LANE_BUILDS).flatMap(([lane, builds]) =>
    [...pathClosure(builds ?? [], root)]
      .filter((dir) => dir.startsWith("crates/"))
      .sort()
      .map((dir) => ({ paths: [`${dir}/**`], surfaces: [lane as Surface] })),
  );
}

/** Additive routes: a path collects the surfaces of every route it matches. */
export function routes(root = ROOT): Route[] {
  return [
    ...DOC_INPUTS,
    // Compile-free workflow infrastructure: only the workflow gate runs. The
    // tools package is CI logic that `just tools-check` tests there; its web
    // harness and integration checks route to their lanes below as well.
    {
      paths: [
        ".github/workflows/*.yml",
        ".github/workflows/*.yaml",
        ".github/actions/**",
        ".github/actionlint.yaml",
        ".github/dependabot.yml",
        "tools/**",
        "scripts/ci/setup-linux-release-userspace.sh",
        "scripts/ci/mbx-export-failed-lane.sh",
      ],
      surfaces: [],
    },
    // A lane's own workflow runs that lane, so an edit to it is proven on the
    // PR that makes it.
    {
      paths: [".github/workflows/desktop-test.yml", ".github/actions/setup-linux-desktop/**"],
      surfaces: ["desktop"],
    },
    { paths: PRODUCT_PROSE.slice(0, 2), surfaces: RUST },
    {
      paths: ["clients/phux-web/**", "clients/phux-vt-web/**", "tools/src/web/**"],
      surfaces: ["web"],
    },
    {
      paths: [
        "clients/phux-vt-web/vendor/**",
        "scripts/build-vt-wasm.sh",
        "scripts/prepare-vt-wasm.mjs",
        "scripts/smoke-vt-wasm.mjs",
        "scripts/test-vt-wasm.mjs",
      ],
      surfaces: ["web", "web_engine"],
    },
    {
      paths: [
        "integrations/**",
        ".claude-plugin/**",
        ".grok-plugin/**",
        "tools/src/integrations/**",
        "scripts/ci/agent-integrations.sh",
        ".release-please-manifest.json",
      ],
      surfaces: ["integrations"],
    },
    { paths: ["crates/**"], surfaces: RUST },
    // The golden release manifests (ADR-0175) are include_str! inputs of the
    // root and desktop updaters' tests; the installers that also read them
    // route on their own. A test checks every desktop file input outside
    // clients/desktop against this table.
    { paths: ["scripts/fixtures/release-manifest/**"], surfaces: [...RUST, "desktop"] },
    // Root crates reach the web and desktop lanes only through Cargo. Every
    // other clients/desktop path is unrouted, so it requests every surface.
    ...crateRoutes(root),
    {
      paths: [
        "crates/**/Cargo.toml",
        "crates/**/Cargo.lock",
        "crates/**/build.rs",
        "crates/**/*.ld",
        "crates/**/*.lds",
        "crates/**/*.c",
        "crates/**/*.h",
      ],
      surfaces: ["native"],
    },
    // Path crates inherit `workspace = true` keys from the root manifest, and
    // the desktop build reads the root toolchain and Cargo config. Its lock is
    // its own, so the root Cargo.lock stays off the desktop lane.
    { paths: ["Cargo.toml", "rust-toolchain.toml", ".cargo/**"], surfaces: [...SHARED, "desktop"] },
    { paths: ["Cargo.lock"], surfaces: SHARED },
    // Setup helpers and just/setup.just are the native-setup assurance lane.
    // They do not rebuild the browser or the root Rust graph.
    {
      paths: [
        "just/setup.just",
        "scripts/setup-rust.sh",
        "scripts/doctor.sh",
        "scripts/test-dev-setup.sh",
        "scripts/native-smoke.sh",
      ],
      surfaces: ["native"],
    },
    // desktop-test.yml provisions its runner with the same contributor setup.
    { paths: ["scripts/setup-rust.sh"], surfaces: ["desktop"] },
    { paths: [".config/zig-toolchain.json"], surfaces: ZIG },
    // install-zig.sh is how native-setup, web-engine and desktop get
    // libghostty's Zig; the Nix phux lanes use flake.nix's compiler instead.
    {
      paths: ["scripts/install-zig.sh", "scripts/lib/dev-toolchain.sh"],
      surfaces: ["native", "web_engine", "desktop"],
    },
    { paths: ["flake.nix", "flake.lock"], surfaces: [...RUST, "web"] },
    // The check job runs build-features-check against the real graph.
    {
      paths: [
        "just/gates.just",
        "just/test.just",
        "just/build.just",
        "tools/src/cargo/build-features.ts",
      ],
      surfaces: ["phux"],
    },
    // The root justfile is imports + `default`; product recipes live in
    // just/*.just. Perf, release, mutation and release-please config are
    // compile-free, as are the product-skills publish mirror and the FFI
    // artifact builders, which only their own workflows run.
    {
      paths: [
        "justfile",
        "just/perf.just",
        "just/release.just",
        "just/mutation.just",
        "release-please-config.json",
        "scripts/check-*",
        "scripts/bench/perf-nightly.sh",
        "scripts/export-product-skills.sh",
        "scripts/product-skills",
        "scripts/skills-package/**",
        "scripts/build-ffi-xcframework.sh",
        "scripts/build-mobile-ffi-android.sh",
        "scripts/ci/setup-android-ndk.sh",
      ],
      surfaces: [],
    },
  ];
}

/**
 * Documentation no build reads. Product prose and doc inputs are excluded,
 * so editing them leaves the docs-only path.
 */
export function isDoc(path: string): boolean {
  if (matchesAny(path, PRODUCT_PROSE)) return false;
  if (DOC_INPUTS.some((route) => matchesAny(path, route.paths))) return false;
  return matchesAny(path, DOCS);
}

const GLOBS = new Map<string, Bun.Glob>();

export function matchesAny(path: string, patterns: readonly string[]): boolean {
  return patterns.some((pattern) => {
    let glob = GLOBS.get(pattern);
    if (glob === undefined) {
      glob = new Bun.Glob(pattern);
      GLOBS.set(pattern, glob);
    }
    return glob.match(path);
  });
}
