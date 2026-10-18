import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { workspaceMembers } from "../lib/cargo.ts";
import { ROOT } from "../lib/repo.ts";
import { table } from "../lib/value.ts";
import { Classifier, toOutputs } from "./classify.ts";
import { SURFACES, type Surface } from "./routes.ts";

const classifier = new Classifier();
const ALL: Surface[] = [...SURFACES];

function lanes(paths: string[]): Surface[] {
  const { needed } = classifier.classify(paths);
  return SURFACES.filter((surface) => needed[surface]);
}

function rustSources(dir: string): string[] {
  return readdirSync(dir, { recursive: true, encoding: "utf8" })
    .filter((path) => path.endsWith(".rs"))
    .map((path) => join(dir, path));
}

describe("surface policy", () => {
  const cases: [string[], Surface[]][] = [
    [[], ALL],
    [["new-product/source.xyz"], ALL],
    [["docs/RELEASING.md"], []],
    [["docs/spec/proto.md"], ["phux"]],
    [["docs/spec/wire-schema.json"], ["phux"]],
    [["docs/reference/cli.md"], ["phux"]],
    [["docs/assets/pi-live-fleet.cast"], ["phux"]],
    [["docs/consumers/agents.md"], ["phux"]],
    [["CHANGELOG.md"], []],
    [[".agents/skills/using-phux/SKILL.md"], ["phux"]],
    [[".agents/skills/using-phux-mcp/SKILL.md"], ["phux"]],
    [["clients/desktop/native/src/main.rs"], ALL],
    [["clients/phux-web/src/lib.rs"], ["web"]],
    [["clients/phux-vt-web/src/lib.rs"], ["web"]],
    [["tools/src/web/browser.ts"], ["web"]],
    [["clients/phux-vt-web/vendor/ghostty-vt.wasm"], ["web", "web_engine"]],
    [["scripts/build-vt-wasm.sh"], ["web", "web_engine"]],
    [["scripts/prepare-vt-wasm.mjs"], ["web", "web_engine"]],
    [["scripts/smoke-vt-wasm.mjs"], ["web", "web_engine"]],
    [["integrations/pi/src/index.ts"], ["integrations"]],
    [["scripts/ci/agent-integrations.sh"], ["integrations"]],
    [["tools/src/integrations/versions.ts"], ["integrations"]],
    [["integrations/claude/skills/phux/SKILL.md"], ["integrations"]],
    [[".claude-plugin/marketplace.json"], ["integrations"]],
    [["crates/phux-server/src/lib.rs"], ["phux", "web"]],
    [["crates/phux-protocol/Cargo.toml"], ["phux", "web", "native", "desktop"]],
    [["crates/phux-protocol/src/lib.rs"], ["phux", "web", "desktop"]],
    [["crates/phux-protocol/tests/wire.rs"], ["phux"]],
    [["crates/phux-client-core/src/lib.rs"], ["phux", "web", "desktop"]],
    [["crates/phux-perf/src/lib.rs"], ["phux", "web", "desktop"]],
    [["crates/phux-client-ffi/src/lib.rs"], ["phux", "web", "desktop"]],
    [["crates/phux-mcp/src/lib.rs"], ["phux"]],
    [["crates/phux/build.rs"], ["phux", "web", "native"]],
    [["crates/phux/src/vendor/x.c"], ["phux", "web", "native"]],
    [["Cargo.toml"], ["phux", "web", "native", "desktop"]],
    [["Cargo.lock"], ["phux", "web", "native"]],
    [[".cargo/config.toml"], ["phux", "web", "native", "desktop"]],
    [[".config/zig-toolchain.json"], ["phux", "web", "web_engine", "native", "desktop"]],
    [["scripts/install-zig.sh"], ["web_engine", "native", "desktop"]],
    [["scripts/lib/dev-toolchain.sh"], ["web_engine", "native", "desktop"]],
    [["scripts/setup-rust.sh"], ["native", "desktop"]],
    [["clients/desktop/native/Cargo.lock"], ALL],
    [["just/desktop.just"], ALL],
    [["scripts/fixtures/release-manifest/stable.json"], ["phux", "desktop"]],
    [[".github/workflows/desktop-test.yml"], ["desktop"]],
    [["scripts/doctor.sh"], ["native"]],
    [["scripts/test-dev-setup.sh"], ["native"]],
    [["scripts/native-smoke.sh"], ["native"]],
    [["just/setup.just"], ["native"]],
    [["just/gates.just"], ["phux"]],
    [["just/test.just"], ["phux"]],
    [["just/build.just"], ["phux"]],
    [["justfile"], []],
    [["just/perf.just"], []],
    [["just/release.just"], []],
    [["just/mutation.just"], []],
    [["release-please-config.json"], []],
    [["just/unknown.just"], ALL],
    [["docs/SETUP.md", "scripts/doctor.sh"], ["native"]],
    [["docs/SETUP.md", "justfile"], []],
    [["scripts/check-e2e-lanes.sh"], []],
    [["scripts/check-docs.sh"], []],
    [["scripts/check-install-surface.sh"], []],
    [["scripts/check-product-skills-export.sh"], []],
    [["scripts/export-product-skills.sh"], []],
    [["scripts/product-skills"], []],
    [["scripts/skills-package/README.md"], []],
    [["scripts/skills-package/NOTICE"], []],
    [["scripts/build-ffi-xcframework.sh"], []],
    [["scripts/build-mobile-ffi-android.sh"], []],
    [["scripts/ci/setup-android-ndk.sh"], []],
    [[".github/workflows/ffi-xcframework.yml"], []],
    [[".github/workflows/ffi-android.yml"], []],
    [[".github/workflows/release.yml"], []],
    [[".github/workflows/native-setup.yml"], []],
    [[".github/actions/setup-rust-lane/action.yml"], []],
    [["scripts/ci/setup-linux-release-userspace.sh"], []],
    [["scripts/ci/mbx-export-failed-lane.sh"], []],
    [["tools/main.ts"], []],
    [["tools/src/routing/routes.ts"], []],
    [["tools/src/release/manifest.ts"], []],
    [["docs/SETUP.md", "clients/phux-web/src/lib.rs"], ["web"]],
  ];
  for (const [paths, wanted] of cases) {
    test(paths.join(", ") || "(empty diff)", () => {
      expect(lanes(paths)).toEqual(SURFACES.filter((surface) => wanted.includes(surface)));
    });
  }
});

test("every workspace crate's library runs the root lane and not native setup", () => {
  for (const { dir } of workspaceMembers()) {
    const { needed } = classifier.classify([`${dir}/src/lib.rs`]);
    expect([dir, needed.phux, needed.native]).toEqual([dir, true, false]);
  }
});

test("an integration test builds only its own binary", () => {
  const testFile = classifier.classify(["crates/phux-server/tests/hub_relay_federation.rs"]);
  expect(testFile.needed.web).toBe(false);
  expect([testFile.unitMode, testFile.unitTargets, testFile.e2eNeeded]).toEqual([
    "narrow",
    "phux-server:hub_relay_federation",
    false,
  ]);
  expect(classifier.classify(["crates/phux-server/src/lib.rs"]).unitMode).toBe("rdeps");
});

test("a gated test target names its required features", () => {
  expect(classifier.classify(["crates/phux-server/tests/stress/stress_churn.rs"]).unitTargets).toBe(
    "phux-server:stress:stress",
  );
  expect(classifier.classify(["crates/phux-client-ffi/tests/connected_flood.rs"]).unitTargets).toBe(
    "phux-client-ffi:connected_flood:measure",
  );
});

test("docs_only holds only for documentation no build reads", () => {
  const cases: [string[], boolean][] = [
    [["docs/SETUP.md", "clients/desktop/README.md"], true],
    [["docs/adr/0001-x.md", "README.md", "docs/INSTALL.md"], true],
    [["docs/SETUP.md", "docs/spec/proto.md"], false],
    [["docs/reference/cli.md"], false],
    [["CHANGELOG.md"], false],
    [[".agents/skills/using-phux/SKILL.md"], false],
    [[".github/workflows/ci.yml"], false],
    [[], false],
  ];
  for (const [paths, docsOnly] of cases) {
    expect([paths, classifier.classify(paths).docsOnly]).toEqual([paths, docsOnly]);
  }
});

test("docs a crate reads leave the docs-only path", () => {
  // A quoted docs/ path in Rust source is a test fixture or an embedded
  // input; editing it must run the lane that reads it.
  const literal = /"(?:\.\.\/)*(docs\/[^"\s{}]+)"/g;
  const read = new Set<string>();
  for (const source of rustSources(join(ROOT, "crates"))) {
    for (const match of readFileSync(source, "utf8").matchAll(literal)) {
      const path = match[1] ?? "";
      if (statSync(join(ROOT, path), { throwIfNoEntry: false })) read.add(path.replace(/\/$/, ""));
    }
  }
  expect(read.size).toBeGreaterThan(0);
  for (const path of read) {
    const probe = statSync(join(ROOT, path)).isFile() ? path : `${path}/probe`;
    const result = classifier.classify([probe]);
    expect([probe, result.docsOnly, result.needed.phux]).toEqual([probe, false, true]);
  }
});

test("the rdeps filterset is exact for crate diffs and fails safe otherwise", () => {
  const cases: [string[], string][] = [
    [[], ""],
    [["docs/SETUP.md"], ""],
    [["Cargo.toml"], ""],
    [["justfile"], ""],
    [[".agents/skills/using-phux/SKILL.md"], ""],
    [[".github/workflows/ci.yml"], ""],
    [["new-product/source.xyz"], ""],
    [["crates/phux-protocol/src/lib.rs"], "rdeps(=phux-protocol)"],
    [["crates/phux-tui/src/lib.rs", "docs/SETUP.md"], "rdeps(=phux-tui)"],
    [
      ["crates/phux-protocol/src/lib.rs", "crates/phux-tui/src/lib.rs"],
      "rdeps(=phux-protocol) + rdeps(=phux-tui)",
    ],
    [["crates/phux-protocol/src/lib.rs", "Cargo.lock"], ""],
  ];
  for (const [paths, filterset] of cases) {
    expect([paths, classifier.classify(paths).testFilterset]).toEqual([paths, filterset]);
  }
  for (const { dir, name } of workspaceMembers()) {
    expect(classifier.classify([`${dir}/src/lib.rs`]).testFilterset).toBe(`rdeps(=${name})`);
  }
});

test("desktop file inputs outside its tree run the desktop lane", () => {
  // include_str!/include_bytes! reach past the Cargo graph; each target
  // outside clients/desktop must still request the desktop lane.
  const literal = /include_(?:str|bytes)!\(\s*"([^"]+)"/g;
  const native = join(ROOT, "clients/desktop/native");
  const inputs = new Set<string>();
  for (const directory of ["src", "tests", "examples", "benches"]) {
    const base = join(native, directory);
    if (!statSync(base, { throwIfNoEntry: false })) continue;
    for (const source of rustSources(base)) {
      for (const match of readFileSync(source, "utf8").matchAll(literal)) {
        const target = relative(ROOT, resolve(dirname(source), match[1] ?? ""));
        if (!target.startsWith("clients/desktop/")) inputs.add(target);
      }
    }
  }
  expect(inputs.size).toBeGreaterThan(0);
  for (const path of inputs)
    expect([path, classifier.classify([path]).needed.desktop]).toEqual([path, true]);
});

test("outputs carry exactly the classify-changes action's keys", () => {
  const action = readFileSync(join(ROOT, ".github/actions/classify-changes/action.yml"), "utf8");
  const keys = Object.keys(table(table(Bun.YAML.parse(action))["outputs"])).toSorted();
  expect(Object.keys(toOutputs(classifier.classify([]))).toSorted()).toEqual(keys);
});

test("the mobile FFI artifacts rebuild on their crate closure only", () => {
  const cases: [string[], boolean][] = [
    [[], true],
    [["crates/phux-client-ffi/src/lib.rs"], true],
    // Reached through phux-client-ffi's dependencies, not listed anywhere.
    [["crates/phux-core/src/lib.rs"], true],
    [["crates/quictun/src/lib.rs"], true],
    [["scripts/build-mobile-ffi-android.sh"], true],
    [[".github/workflows/ffi-xcframework.yml"], true],
    [["crates/phux-tui/src/lib.rs"], false],
    [["clients/desktop/native/src/main.rs"], false],
    [["new-product/source.xyz"], false],
  ];
  for (const [paths, wanted] of cases) {
    expect([paths, classifier.classify(paths).mobileFfiNeeded]).toEqual([paths, wanted]);
  }
});

test("tools code routes to the lane that runs it", () => {
  expect(lanes(["tools/src/desktop/source.ts"])).toEqual(["desktop"]);
  expect(lanes(["tools/src/mutation/runner.ts"])).toEqual([]);
  expect(lanes(["tools/src/cargo/build-features.ts"])).toEqual(["phux"]);
});
