import { expect, test } from "bun:test";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { workspaceMembers } from "../lib/cargo.ts";
import { ROOT } from "../lib/repo.ts";
import { Classifier, toOutputs, type Classification } from "./classify.ts";
import { routes, SURFACES, type Surface } from "./routes.ts";

const classifier = new Classifier();
const ALL: Surface[] = [...SURFACES];

function lanes(paths: string[]): Surface[] {
  const { needed } = classifier.classify(paths);
  return SURFACES.filter((surface) => needed.has(surface));
}

function rustSources(dir: string): string[] {
  return readdirSync(dir, { recursive: true, encoding: "utf8" })
    .filter((path) => path.endsWith(".rs"))
    .map((path) => join(dir, path));
}

/** Assert one Classification field for each diff. */
function expectField<K extends keyof Classification>(
  field: K,
  cases: [string[], Classification[K]][],
): void {
  for (const [paths, want] of cases) {
    expect([paths, classifier.classify(paths)[field]]).toEqual([paths, want]);
  }
}

test.each<[string[], Surface[]]>([
  [[], ALL],
  [["new-product/source.xyz"], ALL],
  [["docs/releasing.md"], []],
  [["docs/spec/wire-schema.json"], ["phux"]],
  [["docs/agents.md"], ["phux"]],
  [["docs/consumers/agents.md"], []],
  [["CHANGELOG.md"], []],
  [[".agents/skills/using-phux/SKILL.md"], ["phux"]],
  [["distros/crew/crew.py"], ["phux"]],
  [["distros/crew/lead.md"], ["phux"]],
  [["distros/crew/skills/crew-ship/SKILL.md"], ["phux"]],
  [["clients/desktop/native/src/main.rs"], ["desktop"]],
  [["clients/desktop/native/Cargo.toml"], ["desktop"]],
  [["clients/desktop/native/tests/window.rs"], ["desktop"]],
  [["clients/desktop/native/assets/icon.png"], ["desktop"]],
  [["just/desktop.just"], ["desktop"]],
  [["clients/phux-web/src/lib.rs"], ["web"]],
  [["tools/src/web/browser.ts"], ["web"]],
  [["clients/phux-vt-web/vendor/ghostty-vt.wasm"], ["web", "web_engine"]],
  [["integrations/pi/src/index.ts"], ["integrations"]],
  [[".agents/lib/phux-agent.sh"], ["integrations"]],
  [["integrations/muse/SKILL.md"], ["integrations"]],
  [["integrations/claude/skills/phux/SKILL.md"], ["integrations"]],
  [["crates/phux-server/src/lib.rs"], ["phux", "web"]],
  [["crates/phux-config-core/src/lib.rs"], ["phux", "web", "desktop"]],
  [["crates/phux-config/src/lib.rs"], ["phux", "web"]],
  [["crates/phux-tui/src/lib.rs"], ["phux", "web"]],
  [["crates/phux-client/src/lib.rs"], ["phux", "web"]],
  [["crates/phux-protocol/Cargo.toml"], ["phux", "web", "native", "desktop"]],
  [["crates/phux-protocol/src/lib.rs"], ["phux", "web", "desktop"]],
  [["crates/phux-protocol/tests/wire.rs"], ["phux"]],
  [["crates/phux-mcp/src/lib.rs"], ["phux"]],
  [["crates/phux/build.rs"], ["phux", "web", "native"]],
  [["Cargo.toml"], ["phux", "web", "native", "desktop"]],
  [["Cargo.lock"], ["phux", "web", "native"]],
  [[".config/zig-toolchain.json"], ["phux", "web", "web_engine", "native", "desktop"]],
  [["scripts/install-zig.sh"], ["web_engine", "native", "desktop"]],
  [["scripts/setup-rust.sh"], ["native", "desktop"]],
  [["scripts/install-windows.ps1"], []],
  [["scripts/test-install-windows.ps1"], []],
  [["scripts/test-windows-wsl.ps1"], []],
  [["scripts/test-windows-mcp.py"], []],
  [["scripts/test-windows-terminal.py"], []],
  [["scripts/install-desktop-windows.ps1"], []],
  [["scripts/test-install-desktop-windows.ps1"], []],
  [["scripts/install-zig-windows.ps1"], []],
  [["scripts/test-windows-bridge-teardown.ps1"], []],
  [["scripts/test-windows-bridge-fixture.py"], []],
  [["clients/desktop/scripts/package-windows.ts"], ["desktop"]],
  [[".github/workflows/windows-wsl.yml"], []],
  [["clients/desktop/native/Cargo.lock"], ["desktop"]],
  [["clients/desktop/scripts/package-app.ts"], ["desktop"]],
  [["clients/desktop/scripts/package-dmg.ts"], ["desktop"]],
  [["clients/desktop/scripts/dmg-layout.py"], ["desktop"]],
  [["clients/desktop/scripts/dmg-artwork.swift"], ["desktop"]],
  [["clients/desktop/packaging/dmg-requirements.txt"], ["desktop"]],
  [["clients/desktop/tests/tooling/package-dmg.test.ts"], ["desktop"]],
  [["clients/desktop/tests/tooling/verify-dmg.py"], ["desktop"]],
  [["clients/desktop/tests/tooling/verify-dmg.swift"], ["desktop"]],
  [["clients/desktop/phux-protocol"], ALL],
  [
    ["clients/desktop/native/src/main.rs", "crates/phux-mcp/src/lib.rs"],
    ["phux", "desktop"],
  ],
  [["just/unknown.just"], ALL],
  [["scripts/fixtures/release-manifest/stable.json"], ["phux", "desktop"]],
  [[".github/workflows/ci.yml"], ALL],
  [[".github/workflows/release.yml"], []],
  [["commitlint.config.mjs"], []],
  [[".github/cockpit-history-tip"], []],
  [[".github/ISSUE_TEMPLATE/bug.yml"], []],
  [[".github/ISSUE_TEMPLATE/config.yml"], []],
  [["commitlint.config.mjs", "clients/desktop/native/src/main.rs"], ["desktop"]],
  [["scripts/doctor.sh"], ["native"]],
  [["just/gates.just"], ["phux"]],
  [["justfile"], []],
  [[".beads/issues.jsonl"], []],
  [[".beads/config.yaml"], []],
  [[".beads/issues.jsonl", "clients/desktop/native/src/main.rs"], ["desktop"]],
  [["scripts/check-docs.sh"], []],
  [["brand.config.json", "brand.lock.json"], []],
  [["scripts/brand.py", "scripts/update-brand.py", "scripts/test_brand_update.py"], []],
  [["brand.lock.json", "clients/desktop/packaging/AppIcon.svg"], ["desktop"]],
  [["brand.lock.json", "clients/desktop/packaging/AppIcon.png"], ["desktop"]],
  [
    [
      "clients/desktop/packaging/brand-profile.json",
      "clients/desktop/packaging/brand-profiles.json",
      "clients/desktop/scripts/brand.ts",
    ],
    ["desktop"],
  ],
  [[".github/workflows/ffi-xcframework.yml"], []],
  [[".github/actions/setup-rust-lane/action.yml"], ALL],
  [[".cargo/config.toml"], ["phux", "web", "native", "desktop"]],
  [["tools/src/desktop/source.ts"], ["desktop"]],
  [["tools/src/lib/git.ts"], ["web", "desktop"]],
  [["tools/src/routing/routes.ts"], []],
  [["docs/development.md", "scripts/doctor.sh"], ["native"]],
  [["docs/development.md", "clients/phux-web/src/lib.rs"], ["web"]],
])("%p routes to %p", (paths, wanted) => {
  expect(lanes(paths)).toEqual(SURFACES.filter((surface) => wanted.includes(surface)));
});

test("brace alternatives stay within one path segment", () => {
  expect(routes().filter(([pattern]) => /\{[^}]*\//.test(pattern))).toEqual([]);
});

test("every workspace crate's library runs the root lane, not native setup, and its rdeps", () => {
  for (const { dir, name } of workspaceMembers()) {
    const result = classifier.classify([`${dir}/src/lib.rs`]);
    expect([dir, result.needed.has("phux"), result.needed.has("native")]).toEqual([
      dir,
      true,
      false,
    ]);
    expect(result.testFilterset).toBe(`rdeps(=${name})`);
  }
});

test("the unit plan narrows to the integration test a diff touches", () => {
  const plan = (paths: string[]) => {
    const { unitMode, unitTargets, e2eNeeded } = classifier.classify(paths);
    return [unitMode, unitTargets, e2eNeeded];
  };
  expect(plan(["crates/phux-server/tests/hub_relay_federation.rs"])).toEqual([
    "narrow",
    "phux-server:hub_relay_federation",
    false,
  ]);
  // A gated target names its required features.
  expect(plan(["crates/phux-server/tests/stress/stress_churn.rs"])[1]).toBe(
    "phux-server:stress:stress",
  );
  expect(plan(["crates/phux-client-ffi/tests/connected_flood.rs"])[1]).toBe(
    "phux-client-ffi:connected_flood:measure",
  );
  expect(plan(["crates/phux-server/src/lib.rs"])).toEqual(["rdeps", "", true]);
  expect(plan(["Cargo.toml"])).toEqual(["workspace", "", true]);
  expect(plan(["docs/development.md"])).toEqual(["skip", "", false]);
  expect(plan([])).toEqual(["workspace", "", true]);
});

test("docs_only holds only for documentation no build reads", () => {
  expectField("docsOnly", [
    [["docs/development.md", "clients/desktop/README.md"], true],
    [["docs/install.md", "README.md", "docs/README.md"], true],
    [["docs/development.md", "docs/spec/wire-schema.json"], false],
    [["docs/reference/cli.md"], false],
    [["docs/agents.md"], false],
    [["docs/consumers/agents.md"], true],
    [["CHANGELOG.md"], false],
    [[".agents/skills/using-phux/SKILL.md"], false],
    [["distros/crew/lead.md"], false],
    [["distros/crew/skills/crew-pulse/SKILL.md"], false],
    [["integrations/muse/SKILL.md"], false],
    [[".github/workflows/ci.yml"], false],
    [[], false],
  ]);
});

test("docs a crate reads leave the docs-only path", () => {
  // A quoted docs/ path in Rust source is a fixture or an embedded input.
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
    expect([probe, result.docsOnly, result.needed.has("phux")]).toEqual([probe, false, true]);
  }
});

test("the rdeps filterset is exact for crate diffs and fails safe otherwise", () => {
  expectField("testFilterset", [
    [[], ""],
    [["docs/development.md"], ""],
    [["Cargo.toml"], ""],
    [[".agents/skills/using-phux/SKILL.md"], ""],
    [["new-product/source.xyz"], ""],
    [["crates/phux-tui/src/lib.rs", "docs/development.md"], "rdeps(=phux-tui)"],
    [
      ["crates/phux-protocol/src/lib.rs", "crates/phux-tui/src/lib.rs"],
      "rdeps(=phux-protocol) + rdeps(=phux-tui)",
    ],
    [["crates/phux-protocol/src/lib.rs", "Cargo.lock"], ""],
  ]);
});

test("desktop file inputs outside its tree run the desktop lane", () => {
  // include_str!/include_bytes! reach past the Cargo graph.
  const literal = /include_(?:str|bytes)!\(\s*"([^"]+)"/g;
  const inputs = new Set<string>();
  for (const directory of ["src", "tests", "examples", "benches"]) {
    const base = join(ROOT, "clients/desktop/native", directory);
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
    expect([path, lanes([path]).includes("desktop")]).toEqual([path, true]);
});

test("content republishes on docs, installers and product skills only", () => {
  expectField("contentNeeded", [
    [[], true],
    [["docs/SETUP.md"], true],
    [["scripts/install.sh"], true],
    [["scripts/install-desktop.sh"], true],
    [[".agents/skills/using-phux/SKILL.md"], true],
    [["scripts/skills-package/package.json"], true],
    [["crates/phux/src/main.rs"], false],
    [["README.md"], false],
  ]);
});

test("outputs name every classification field once", () => {
  const outputs = Object.keys(toOutputs(classifier.classify([])));
  expect(outputs).toContain("mobile_ffi_needed");
  expect(new Set(outputs).size).toBe(outputs.length);
});

test("UniFFI qualification skips only known unrelated crate inputs", () => {
  expectField("uniffiNeeded", [
    [[], true],
    [["crates/phux-client-ffi/src/lib.rs"], true],
    [["crates/phux-client-ffi/tests/connected.rs"], true],
    [["crates/phux-core/src/lib.rs"], true],
    [["crates/phux-server/src/lib.rs"], true],
    [["crates/phux-testkit/src/lib.rs"], true],
    [["crates/quictun/src/lib.rs"], true],
    [["crates/phux-mcp/src/lib.rs"], false],
    [["crates/phux-tui/src/lib.rs", "docs/development.md"], false],
    [["crates/phux-tui/src/lib.rs", "crates/phux-core/src/lib.rs"], true],
    [["crates/phux-mcp/src/lib.rs", "Cargo.lock"], true],
    [["crates/phux-mcp/src/lib.rs", "new-product/source.xyz"], true],
    [["crates/new-crate/src/lib.rs"], true],
    [["Cargo.toml"], true],
    [[".github/workflows/ci.yml"], true],
    [["docs/development.md"], false],
  ]);
});

test("the mobile FFI artifacts rebuild on their crate closure only", () => {
  expectField("mobileFfiNeeded", [
    [[], true],
    [["crates/phux-client-ffi/src/lib.rs"], true],
    // Reached through phux-client-ffi's dependencies, not listed anywhere.
    [["crates/phux-core/src/lib.rs"], true],
    [["crates/phux-config-core/src/lib.rs"], true],
    [["crates/phux-config/src/lib.rs"], true],
    [["crates/quictun/src/lib.rs"], true],
    [["scripts/build-mobile-ffi-android.sh"], true],
    [[".github/workflows/ffi-xcframework.yml"], true],
    [["crates/phux-tui/src/lib.rs"], false],
    [["clients/desktop/native/src/main.rs"], false],
    [["new-product/source.xyz"], false],
  ]);
});
