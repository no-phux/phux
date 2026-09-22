#!/usr/bin/env python3
"""One CI routing contract. Patterns use fnmatch (stars include slashes).

Library inputs in the bundled coordinator or FFI closure still route to
Cockpit. `tests/`, `benches/` and `examples/` do not: they are not binary
inputs, so they do not rebuild Cockpit, the browser, or native setup. Crates
outside both closures (the MCP binary, the server testkit) do not either.
Browser workspaces stay separate. Native means the *clean setup assurance*
lane, not all native Rust. Workflow orchestration is compile-free.

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

SURFACES = ("phux", "cockpit", "web", "web_engine", "integrations", "native")
ALL = set(SURFACES)
RUST = {"phux", "cockpit"}
SHARED = RUST | {"web", "native"}
ZIG = SHARED | {"web_engine"}

# Additive routes: a crate manifest matches its product and the native input
# row. Keep these in one table rather than duplicating path filters in YAML.
ROUTES = (
    ((".agents/skills/using-phux/*", ".agents/skills/using-phux-mcp/*"), RUST),
    (("clients/cockpit/*",), {"cockpit"}),
    (("scripts/ci/cockpit_artifacts.py", "scripts/ci/test_cockpit_artifacts.py"), {"cockpit"}),
    (("clients/phux-web/*", "clients/phux-vt-web/*", "scripts/ci/web-browser.py",
      "scripts/ci/test_web_browser.py"), {"web"}),
    (("clients/phux-vt-web/vendor/*", "scripts/build-vt-wasm.sh",
      "scripts/test-vt-wasm.mjs"), {"web", "web_engine"}),
    (("integrations/*", ".claude-plugin/*",
      "scripts/check-agent-integration-versions.mjs",
      "scripts/ci/agent-integrations.sh", ".release-please-manifest.json"), {"integrations"}),
    (("crates/*",), RUST),
    # Browser Rust consumers plus the live demo-server example's dependency
    # closure (including its Cargo dev dependencies). The fixture checks this
    # against manifests so a new local dependency cannot silently lose coverage.
    (("crates/phux-protocol/*", "crates/phux-client-core/*", "crates/phux-perf/*",
      "crates/phux-client-ffi/*", "crates/phux-config/*", "crates/phux-core/*",
      "crates/phux-client-runtime/*",
      "crates/phux-dial/*", "crates/phux-plugin/*", "crates/phux-relay/*",
      "crates/phux-server/*", "crates/phux-server-testkit/*",
      "crates/phux-agent-rules/*",
      "crates/portable-pty-adopt/*"), {"web"}),
    (("crates/*/Cargo.toml", "crates/*/Cargo.lock", "crates/*/build.rs",
      "crates/*/*.ld", "crates/*/*.lds", "crates/*/*.c", "crates/*/*.h"), {"native"}),
    (("Cargo.toml", "rust-toolchain.toml", ".cargo/*"), SHARED),
    (("Cargo.lock",), SHARED),
    # Setup helpers and just/setup.just are the native-setup assurance lane.
    # They do not rebuild Cockpit, the browser, or the root Rust graph.
    (("just/setup.just", "scripts/setup-rust.sh", "scripts/doctor.sh",
      "scripts/test-dev-setup.sh", "scripts/native-smoke.sh"), {"native"}),
    ((".config/zig-toolchain.json",), ZIG),
    # install-zig.sh is how cockpit-ci, native-setup, and web-engine get Zig;
    # the Nix phux lanes use flake.nix's compiler instead.
    (("scripts/install-zig.sh", "scripts/lib/dev-toolchain.sh"),
     {"cockpit", "native", "web_engine"}),
    (("flake.nix", "flake.lock"), RUST | {"web"}),
    (("just/gates.just", "just/test.just", "just/build.just"), {"phux"}),
    (("just/cockpit.just",), {"cockpit"}),
    # Root justfile is imports + `default`. Product recipes live in just/*.just.
    # Perf/release/mutation and release-please config are compile-free.
    (("justfile", "just/perf.just", "just/release.just", "just/mutation.just",
      "release-please-config.json", "scripts/check-*"), set()),
    # Product-skills publish mirror and maintainer-only beads skill. The
    # compiled using-phux* skills stay on the RUST route above.
    (("scripts/export-product-skills.sh", "scripts/product-skills",
      "scripts/skills-package/*", ".agents/skills/beads/*"), set()),
    # The xcframework builder runs only from ffi-xcframework.yml (release and
    # dispatch) and `just ffi-xcframework`; no product lane consumes it, and
    # its inputs (the crate, Cargo, the Zig pin) route on their own.
    (("scripts/build-ffi-xcframework.sh", "scripts/build-mobile-ffi-android.sh",
      "scripts/ci/setup-android-ndk.sh"), set()),
)
WORKFLOWS = (
    ".github/workflows/*.yml", ".github/workflows/*.yaml",
    ".github/actions/*", ".github/actionlint.yaml", ".github/dependabot.yml",
    "scripts/ci/classify-changes*", "scripts/ci/check-classify-changes.sh",
    "scripts/ci/detect-changes.py",
    "scripts/ci/validation_receipt.py", "scripts/ci/test_validation_receipt.py",
    "scripts/ci/publish_plan.py", "scripts/ci/test_publish_plan.py",
    "scripts/ci/dispatch_integration_publishes.py",
    "scripts/ci/test_dispatch_integration_publishes.py",
    "scripts/ci/extract_changelog_section.py", "scripts/ci/test_extract_changelog_section.py",
    "scripts/ci/setup-linux-release-userspace.sh", "scripts/ci/test_runner_policy.py",
    "scripts/check-release-orchestration.mjs",
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
    return matches(path, ("docs/*", "docs/adr/*", "*.md"))


def surfaces_for(path):
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
DROP_FOR_NON_LIBRARY = frozenset({"cockpit", "web", "web_engine", "native"})
SHARED_BINARY_INPUTS = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/*")
ZIG_APP = (
    "clients/cockpit/build.zig",
    "clients/cockpit/build.zig.zon",
    "clients/cockpit/src/*",
    "clients/cockpit/*.zig",
    "clients/cockpit/scripts/build-shipping-app.sh",
    "clients/cockpit/scripts/zig-build.sh",
    "clients/cockpit/scripts/build-phux-cli.sh",
    "clients/cockpit/scripts/build-phux-artifacts.sh",
    "clients/cockpit/scripts/native-cargo-target.sh",
    ".config/zig-toolchain.json",
    "scripts/install-zig.sh",
)
FFI_ROOT = "phux-client-ffi"
CLI_ROOT = "phux"


def package_manifests(root=ROOT):
    found = {}
    for manifest in (root / "crates").glob("*/Cargo.toml"):
        data = tomllib.loads(manifest.read_text())
        found[data["package"]["name"]] = data
    return found


def normal_deps(manifest):
    deps = set()
    for table in (manifest, *manifest.get("target", {}).values()):
        deps.update(table.get("dependencies", {}))
    return deps


def normal_closure(roots, manifests):
    pending = list(roots)
    seen = set()
    while pending:
        name = pending.pop()
        if name in seen or name not in manifests:
            continue
        seen.add(name)
        pending.extend(normal_deps(manifests[name]) & manifests.keys())
    return seen


def closure_directories(root_package, packages=None, manifests=None):
    """Crate directories whose normal-dependency closure includes root_package."""
    packages = workspace_packages() if packages is None else packages
    manifests = package_manifests() if manifests is None else manifests
    names = normal_closure((root_package,), manifests)
    return {prefix.split("/", 1)[1] for name, prefix in packages if name in names}


def non_library_kind(path):
    parts = path.split("/")
    if len(parts) < 4 or parts[0] != "crates" or parts[2] not in NON_LIBRARY:
        return None
    return parts[2]


def crate_directory(path):
    parts = path.split("/")
    if len(parts) < 2 or parts[0] != "crates":
        return None
    return parts[1]


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


def narrow_targets(paths, packages, root=ROOT):
    targets = []
    for path in paths:
        if non_library_kind(path) != "tests":
            return None
        parsed = integration_target(path, root)
        name = package_named(parsed[0], packages) if parsed else None
        if name is None:
            return None
        targets.append(f"{name}:{parsed[1]}")
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


def touches_closure(path, packages, directories):
    if matches(path, SHARED_BINARY_INPUTS):
        return True
    directory = crate_directory(path)
    return directory in directories and non_library_kind(path) is None


def artifact_flags(files, packages, manifests):
    if not files:
        return {"ffi_needed": True, "cli_needed": True, "zig_needed": True, "shipping_needed": True}
    ffi_dirs = closure_directories(FFI_ROOT, packages, manifests)
    cli_dirs = closure_directories(CLI_ROOT, packages, manifests)
    flags = {"ffi_needed": False, "cli_needed": False, "zig_needed": False}
    for path in files:
        if not path or is_doc(path):
            continue
        if matches(path, ZIG_APP):
            flags["zig_needed"] = True
        if touches_closure(path, packages, ffi_dirs):
            flags["ffi_needed"] = True
        if touches_closure(path, packages, cli_dirs):
            flags["cli_needed"] = True
    flags["shipping_needed"] = flags["zig_needed"] or flags["ffi_needed"]
    return flags


def route_path(path, packages, cli_dirs, ffi_dirs):
    selected = surfaces_for(path)
    if non_library_kind(path):
        return (selected - DROP_FOR_NON_LIBRARY) | {"phux"}
    directory = crate_directory(path)
    if directory and directory not in cli_dirs and directory not in ffi_dirs:
        selected.discard("cockpit")
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


def classify(files):
    files = [path for path in files if path]
    packages = workspace_packages()
    manifests = package_manifests()
    cli_dirs = closure_directories(CLI_ROOT, packages, manifests)
    ffi_dirs = closure_directories(FFI_ROOT, packages, manifests)
    selected = set()
    for path in files:
        selected.update(route_path(path, packages, cli_dirs, ffi_dirs))
    if not files:
        selected = ALL.copy()
    outputs = {surface + "_needed": surface in selected for surface in SURFACES}
    outputs["docs_only"] = bool(files) and all(is_doc(path) for path in files)
    outputs["workflow_only"] = bool(files) and all(matches(path, WORKFLOWS) for path in files)
    outputs["test_filterset"] = test_filterset(files, packages)
    outputs.update(unit_plan(files, packages))
    outputs.update(artifact_flags(files, packages, manifests))
    return outputs


def emit(outputs):
    for key, value in outputs.items():
        if isinstance(value, bool):
            print(f"{key}={str(value).lower()}")
        else:
            print(f"{key}={value}")


if __name__ == "__main__":
    emit(classify(sys.stdin.read().splitlines()))
