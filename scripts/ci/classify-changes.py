#!/usr/bin/env python3
"""One CI routing contract. Patterns use fnmatch (stars include slashes).

Root crate changes conservatively cover the complete bundled Cockpit coordinator
and FFI closure. Browser workspaces are separate; only their shared Rust inputs
route both. Native means the *clean setup assurance* lane, not all native Rust.
Workflow orchestration is compile-free; Zig pins live in .config independently.

`test_filterset` is the PR unit-test execution set (phux-14r7): rdeps of each
changed workspace crate after a `--workspace` build. Empty means run
everything — empty diffs, docs-only leftovers, or any path that is not a
workspace crate (Cargo.toml, justfile, skills, scripts).
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
    (("skills/*",), RUST),
    (("clients/cockpit/*",), {"cockpit"}),
    (("scripts/ci/cockpit_artifacts.py", "scripts/ci/test_cockpit_artifacts.py"), {"cockpit"}),
    (("clients/phux-web/*", "clients/phux-vt-web/*", "scripts/ci/web-browser.py",
      "scripts/ci/test_web_browser.py"), {"web"}),
    (("clients/phux-vt-web/vendor/*", "scripts/build-vt-wasm.sh",
      "scripts/test-vt-wasm.mjs"), {"web", "web_engine"}),
    (("integrations/*", ".claude-plugin/*",
      "scripts/check-agent-integration-versions.mjs", ".release-please-manifest.json"), {"integrations"}),
    (("crates/*",), RUST),
    # Browser Rust consumers plus the live demo-server example's dependency
    # closure (including its Cargo dev dependencies). The fixture checks this
    # against manifests so a new local dependency cannot silently lose coverage.
    (("crates/phux-protocol/*", "crates/phux-client-core/*", "crates/phux-perf/*",
      "crates/phux-client-ffi/*", "crates/phux-config/*", "crates/phux-core/*",
      "crates/phux-dial/*", "crates/phux-plugin/*", "crates/phux-relay/*",
      "crates/phux-server/*", "crates/phux-server-testkit/*",
      "crates/portable-pty-adopt/*"), {"web"}),
    (("crates/*/Cargo.toml", "crates/*/Cargo.lock", "crates/*/build.rs",
      "crates/*/*.ld", "crates/*/*.lds", "crates/*/*.c", "crates/*/*.h"), {"native"}),
    (("Cargo.toml", "rust-toolchain.toml", ".cargo/*",
      "scripts/setup-rust.sh", "scripts/doctor.sh", "scripts/test-dev-setup.sh"), SHARED),
    (("Cargo.lock",), SHARED),
    (("scripts/native-smoke.sh",), RUST | {"native"}),
    ((".config/zig-toolchain.json", "scripts/install-zig.sh",
      "scripts/lib/dev-toolchain.sh"), ZIG),
    (("flake.nix", "flake.lock"), RUST | {"web"}),
    (("justfile", "release-please-config.json"), ALL),
)
WORKFLOWS = (
    ".github/workflows/*.yml", ".github/workflows/*.yaml",
    ".github/actions/*", ".github/actionlint.yaml", ".github/dependabot.yml",
    "scripts/ci/classify-changes*", "scripts/ci/check-classify-changes.sh",
    "scripts/ci/detect-changes.py",
    "scripts/ci/validation_receipt.py", "scripts/ci/test_validation_receipt.py",
    "scripts/ci/wait_validation.py", "scripts/ci/test_wait_validation.py",
    "scripts/ci/extract_changelog_section.py", "scripts/ci/test_extract_changelog_section.py",
    "scripts/check-release-orchestration.mjs",
)
ROOT = Path(__file__).resolve().parents[2]
# nextest package matchers treat an unprefixed name as a glob; `=` is exact.
CRATE_NAME = re.compile(r"^[A-Za-z0-9_-]+$")


def matches(path, patterns):
    return any(fnmatchcase(path, pattern) for pattern in patterns)


def is_doc(path):
    # Skills are executable product inputs, including shipped agent prompts.
    if matches(path, ("skills/*", "integrations/*/skills/*")):
        return False
    return matches(path, ("docs/*", "docs/adr/*", "*.md"))


def surfaces_for(path):
    if is_doc(path) or matches(path, WORKFLOWS):
        return set()
    selected = set()
    for patterns, surfaces in ROUTES:
        if matches(path, patterns):
            selected.update(surfaces)
    return selected or ALL.copy()


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
    selected = set()
    for path in files:
        selected.update(surfaces_for(path))
    if not files:
        selected = ALL.copy()
    outputs = {surface + "_needed": surface in selected for surface in SURFACES}
    outputs["docs_only"] = bool(files) and all(is_doc(path) for path in files)
    outputs["workflow_only"] = bool(files) and all(matches(path, WORKFLOWS) for path in files)
    outputs["test_filterset"] = test_filterset(files)
    return outputs


def emit(outputs):
    for key, value in outputs.items():
        if isinstance(value, bool):
            print(f"{key}={str(value).lower()}")
        else:
            print(f"{key}={value}")


if __name__ == "__main__":
    emit(classify(sys.stdin.read().splitlines()))
