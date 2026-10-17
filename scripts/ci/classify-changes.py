#!/usr/bin/env python3
"""One CI routing contract. Patterns use fnmatch (stars include slashes).

`tests/`, `benches/` and `examples/` are not binary inputs, so they do not
rebuild the browser or native setup.
Browser workspaces stay separate. Native means the *clean setup assurance*
lane, not all native Rust. Desktop is the GPUI host crate under
clients/desktop/native: its own Cargo workspace and lock, built against the
root crates it names by path. Workflow orchestration is compile-free.

`test_filterset` is still the PR rdeps expression for a library change
(phux-14r7). Empty means the full pool. `unit_mode=narrow` is a different
path: one integration-test binary, not a workspace link followed by a filter.
Pushes to main ignore `unit_mode` and keep the full pool.
"""

from fnmatch import fnmatchcase
from pathlib import Path
import re
import sys
import tomllib

SURFACES = ("phux", "web", "web_engine", "integrations", "native", "desktop")
ALL = set(SURFACES)
RUST = {"phux"}
SHARED = RUST | {"web", "native"}
ZIG = SHARED | {"web_engine", "desktop"}

# Documentation that code or a guard reads as an input. Each row leaves the
# docs-only fast path and routes like its reader: Rust tests byte-compare the
# spec, generated reference, agent guide and recorded casts; the full workflow
# gate reads the root changelog. classify-changes-test.py fails
# when a crate starts reading a doc that is missing here. The site sources
# live in no-phux/website.
DOC_INPUTS = (
    (("docs/spec/*", "docs/reference/*", "docs/assets/*",
      "docs/consumers/agents.md"), {"phux"}),
    (("CHANGELOG.md",), set()),
)

# Additive routes: a crate manifest matches its product and the native input
# row. Keep these in one table rather than duplicating path filters in YAML.
ROUTES = DOC_INPUTS + (
    ((".agents/skills/using-phux/*", ".agents/skills/using-phux-mcp/*"), RUST),
    (("clients/phux-web/*", "clients/phux-vt-web/*", "scripts/ci/web-browser.py",
      "scripts/ci/test_web_browser.py", "scripts/ci/web_agent_fixture.py"), {"web"}),
    (("clients/phux-vt-web/vendor/*", "scripts/build-vt-wasm.sh",
      "scripts/prepare-vt-wasm.mjs", "scripts/smoke-vt-wasm.mjs",
      "scripts/test-vt-wasm.mjs"), {"web", "web_engine"}),
    (("integrations/*", ".claude-plugin/*", ".grok-plugin/*",
      "scripts/check-agent-integration-versions.mjs",
      "scripts/ci/agent-integrations.sh", ".release-please-manifest.json"), {"integrations"}),
    (("crates/*",), RUST),
    # The golden release manifests (ADR-0175) are include_str! inputs of the
    # root and desktop updaters' tests; the installers that also read them
    # route on their own. classify-changes-test.py checks every desktop file
    # input outside clients/desktop against this table.
    (("scripts/fixtures/release-manifest/*",), RUST | {"desktop"}),
    # Browser Rust consumers plus the live demo server and local-owner CLI
    # fixture dependency closures (including Cargo dev dependencies). The test checks this
    # against manifests so a new local dependency cannot silently lose coverage.
    (("crates/phux-protocol/*", "crates/phux-client-core/*", "crates/phux-perf/*",
      "crates/phux-client-ffi/*", "crates/phux-config/*", "crates/phux-core/*",
      "crates/phux-client-runtime/*",
      "crates/phux-dial/*", "crates/phux-plugin/*", "crates/phux-relay/*",
      "crates/phux-server/*", "crates/phux-server-testkit/*",
      "crates/phux-agent-rules/*", "crates/phux-agent-host/*", "crates/phux/*", "crates/phux-client/*",
      "crates/phux-mission/*", "crates/phux-work/*",
      "crates/phux-tui/*", "crates/phux-record/*", "crates/phux-crash/*",
      "crates/portable-pty-adopt/*"), {"web"}),
    # The desktop host's path-dependency closure, checked against the
    # manifests by the test. Every other clients/desktop path is unrouted, so
    # it requests every surface, desktop included.
    (("crates/phux-protocol/*", "crates/phux-client-core/*", "crates/phux-perf/*",
      "crates/phux-client-ffi/*", "crates/phux-config/*", "crates/phux-core/*",
      "crates/phux-client-runtime/*", "crates/phux-dial/*"), {"desktop"}),
    (("crates/*/Cargo.toml", "crates/*/Cargo.lock", "crates/*/build.rs",
      "crates/*/*.ld", "crates/*/*.lds", "crates/*/*.c", "crates/*/*.h"), {"native"}),
    # Path crates inherit `workspace = true` keys from the root manifest, and
    # the desktop build reads the root toolchain and Cargo config. Its lock is
    # its own, so the root Cargo.lock stays off the desktop lane.
    (("Cargo.toml", "rust-toolchain.toml", ".cargo/*"), SHARED | {"desktop"}),
    (("Cargo.lock",), SHARED),
    # Setup helpers and just/setup.just are the native-setup assurance lane.
    # They do not rebuild the browser or the root Rust graph.
    (("just/setup.just", "scripts/setup-rust.sh", "scripts/doctor.sh",
      "scripts/test-dev-setup.sh", "scripts/native-smoke.sh"), {"native"}),
    # desktop-test.yml provisions its runner with the same contributor setup.
    (("scripts/setup-rust.sh",), {"desktop"}),
    ((".config/zig-toolchain.json",), ZIG),
    # install-zig.sh is how native-setup, web-engine and desktop get
    # libghostty's Zig; the Nix phux lanes use flake.nix's compiler instead.
    (("scripts/install-zig.sh", "scripts/lib/dev-toolchain.sh"),
     {"native", "web_engine", "desktop"}),
    (("flake.nix", "flake.lock"), RUST | {"web"}),
    (("just/gates.just", "just/test.just", "just/build.just"), {"phux"}),
    # Root justfile is imports + `default`. Product recipes live in just/*.just.
    # Perf/release/mutation and release-please config are compile-free.
    (("justfile", "just/perf.just", "just/release.just", "just/mutation.just",
      "release-please-config.json", "scripts/check-*",
      "scripts/bench/perf-nightly.sh"), set()),
    # Product-skills publish mirror. The compiled using-phux* skills stay on
    # the RUST route above.
    (("scripts/export-product-skills.sh", "scripts/product-skills",
      "scripts/skills-package/*"), set()),
    # The xcframework builder runs only from ffi-xcframework.yml (release and
    # dispatch) and `just ffi-xcframework`; no product lane consumes it, and
    # its inputs (the crate, Cargo, the Zig pin) route on their own.
    (("scripts/build-ffi-xcframework.sh", "scripts/build-mobile-ffi-android.sh",
      "scripts/ci/setup-android-ndk.sh"), set()),
)
# A lane's own workflow runs its lane, so an edit to it is proven on the PR
# that makes it. Checked before the compile-free WORKFLOWS short-circuit.
SELF_ROUTES = (
    ((".github/workflows/desktop-test.yml",), {"desktop"}),
)
WORKFLOWS = (
    ".github/workflows/*.yml", ".github/workflows/*.yaml",
    ".github/actions/*", ".github/actionlint.yaml", ".github/dependabot.yml",
    "scripts/ci/classify-changes*", "scripts/ci/check-classify-changes.sh",
    "scripts/ci/detect-changes.py",
    "scripts/ci/release_metadata.py", "scripts/ci/test_release_metadata.py",
    "scripts/ci/validation_receipt.py", "scripts/ci/test_validation_receipt.py",
    "scripts/ci/publish_plan.py", "scripts/ci/test_publish_plan.py",
    "scripts/ci/release_manifest.py", "scripts/ci/test_release_manifest.py",
    "scripts/ci/dispatch_integration_publishes.py",
    "scripts/ci/test_dispatch_integration_publishes.py",
    "scripts/ci/extract_changelog_section.py", "scripts/ci/test_extract_changelog_section.py",
    "scripts/ci/setup-linux-release-userspace.sh", "scripts/ci/test_runner_policy.py",
    "scripts/ci/mbx-export-failed-lane.sh",
    "scripts/ci/test_release_drift.py",
    "scripts/sync-path-lock-versions.py", "scripts/ci/test_sync_path_lock_versions.py",
)
ROOT = Path(__file__).resolve().parents[2]
# nextest package matchers treat an unprefixed name as a glob; `=` is exact.
CRATE_NAME = re.compile(r"^[A-Za-z0-9_-]+$")


def matches(path, patterns):
    return any(fnmatchcase(path, pattern) for pattern in patterns)


def is_doc(path):
    # Skills are executable product inputs, including shipped agent prompts.
    if matches(path, (".agents/skills/using-phux/*",
                      ".agents/skills/using-phux-mcp/*",
                      "integrations/*/skills/*")):
        return False
    if any(matches(path, patterns) for patterns, _ in DOC_INPUTS):
        return False
    return matches(path, ("docs/*", "docs/adr/*", "*.md"))


def surfaces_for(path):
    for patterns, surfaces in SELF_ROUTES:
        if matches(path, patterns):
            return set(surfaces)
    if is_doc(path) or matches(path, WORKFLOWS):
        return set()
    selected = set()
    matched = False
    for patterns, surfaces in ROUTES:
        if matches(path, patterns):
            selected.update(surfaces)
            matched = True
    # An explicit empty route is cheap (workflow-gate only). Unknown paths
    # still fail closed into every surface.
    if matched:
        return selected
    return ALL.copy()


def workspace_packages(root=ROOT):
    """Workspace members from crate manifests — the cargo-metadata set."""
    packages = []
    for manifest in (root / "crates").glob("*/Cargo.toml"):
        name = tomllib.loads(manifest.read_text())["package"]["name"]
        if not CRATE_NAME.fullmatch(name):
            continue
        packages.append((name, str(manifest.parent.relative_to(root))))
    packages.sort(key=lambda item: -len(item[1]))
    return packages


def crate_for(path, packages):
    for name, prefix in packages:
        if path == prefix or path.startswith(prefix + "/"):
            return name
    return None


NON_LIBRARY = frozenset({"tests", "benches", "examples"})
DROP_FOR_NON_LIBRARY = frozenset({"web", "web_engine", "native", "desktop"})


def non_library_kind(path):
    parts = path.split("/")
    if len(parts) < 4 or parts[0] != "crates" or parts[2] not in NON_LIBRARY:
        return None
    return parts[2]


def part_is_e2e(part):
    stem = part[:-3] if part.endswith(".rs") else part
    return stem == "e2e" or stem.endswith("_e2e") or stem.startswith("e2e_")


def is_e2e_path(path):
    return any(part_is_e2e(part) for part in path.split("/"))


def integration_target(path, root=ROOT):
    """`(directory, binary)` for a tests/ path. `*` means every integration test."""
    parts = path.split("/")
    if non_library_kind(path) != "tests":
        return None
    rest = parts[3:]
    if len(rest) == 1 and rest[0].endswith(".rs"):
        return parts[1], rest[0][:-3]
    binary = rest[0][:-3] if rest[0].endswith(".rs") else rest[0]
    if (root / "crates" / parts[1] / "tests" / binary / "main.rs").is_file():
        return parts[1], binary
    return parts[1], "*"


def package_named(directory, packages):
    prefix = f"crates/{directory}"
    for name, path in packages:
        if path == prefix:
            return name
    return None


def plan(mode, targets, e2e):
    return {"unit_mode": mode, "unit_targets": targets, "e2e_needed": e2e}


def gating_features(directory, binary, root=ROOT):
    """Non-default `required-features` of one `[[test]]` target, `+`-joined.

    A gated target is skipped unless its features are on, so the narrow lane
    names them to keep building the target the diff touched.
    """
    manifest = tomllib.loads((root / "crates" / directory / "Cargo.toml").read_text())
    defaults = set(manifest.get("features", {}).get("default", []))
    for target in manifest.get("test", []):
        if target.get("name") == binary:
            return "+".join(f for f in target.get("required-features", []) if f not in defaults)
    return ""


def narrow_targets(paths, packages, root=ROOT):
    targets = []
    for path in paths:
        if non_library_kind(path) != "tests":
            return None
        parsed = integration_target(path, root)
        name = package_named(parsed[0], packages) if parsed else None
        if name is None:
            return None
        features = gating_features(*parsed, root) if parsed[1] != "*" else ""
        targets.append(f"{name}:{parsed[1]}" + (f":{features}" if features else ""))
    return targets


def library_unit_plan(files, packages):
    mode = "workspace" if test_filterset(files, packages) == "" else "rdeps"
    return plan(mode, "", True)


def narrow_unit_plan(productive, packages, root):
    if all(non_library_kind(path) != "tests" for path in productive):
        return plan("skip", "", False)
    targets = narrow_targets(productive, packages, root)
    if targets is None:
        return plan("workspace", "", True)
    e2e = any(is_e2e_path(path) for path in productive)
    return plan("narrow", ",".join(sorted(set(targets))), e2e)


def unit_plan(files, packages, root=ROOT):
    """workspace/rdeps keep today's pool. narrow builds one integration test."""
    if not files:
        return plan("workspace", "", True)
    productive = [path for path in files if not is_doc(path)]
    if not productive:
        return plan("skip", "", False)
    if any(non_library_kind(path) is None for path in productive):
        return library_unit_plan(files, packages)
    return narrow_unit_plan(productive, packages, root)


def route_path(path):
    selected = surfaces_for(path)
    if non_library_kind(path):
        return (selected - DROP_FOR_NON_LIBRARY) | {"phux"}
    return selected


def test_filterset(files, packages=None):
    """nextest `-E` expression, or empty to run the full unit pool."""
    if packages is None:
        packages = workspace_packages()
    crates = set()
    for path in files:
        if not path or is_doc(path):
            continue
        crate = crate_for(path, packages)
        if crate is None:
            return ""
        crates.add(crate)
    if not crates:
        return ""
    return " + ".join(f"rdeps(={name})" for name in sorted(crates))


# Instruction-count job. Stars include slashes. An empty diff fails closed.
# Docs and unrelated crates (the TUI, for example) stay off this lane.
PERF_ICOUNT_PATHS = (
    "crates/phux-server/*",
    "crates/phux-protocol/*",
    "crates/phux-client-core/*",
    "crates/phux-core/*",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/*",
    "flake.nix",
    "flake.lock",
    "benchmarks/icount-baseline.json",
    "scripts/ci/perf-icount.sh",
    "scripts/ci/perf_icount.py",
    "scripts/ci/test_perf_icount.py",
    "just/perf.just",
    ".github/workflows/ci.yml",
)


def perf_icount_needed(files):
    if not files:
        return True
    return any(matches(path, PERF_ICOUNT_PATHS) for path in files)


def classify(files):
    files = [path for path in files if path]
    packages = workspace_packages()
    selected = set()
    for path in files:
        selected.update(route_path(path))
    if not files:
        selected = ALL.copy()
    outputs = {surface + "_needed": surface in selected for surface in SURFACES}
    outputs["docs_only"] = bool(files) and all(is_doc(path) for path in files)
    outputs["workflow_only"] = bool(files) and all(matches(path, WORKFLOWS) for path in files)
    outputs["perf_icount_needed"] = perf_icount_needed(files)
    outputs["test_filterset"] = test_filterset(files, packages)
    outputs.update(unit_plan(files, packages))
    return outputs


def emit(outputs):
    for key, value in outputs.items():
        if isinstance(value, bool):
            print(f"{key}={str(value).lower()}")
        else:
            print(f"{key}={value}")


if __name__ == "__main__":
    emit(classify(sys.stdin.read().splitlines()))
