#!/usr/bin/env python3
"""Fail when the desktop host lock can no longer satisfy a `--locked` build.

`clients/desktop/native` is its own Cargo workspace with its own lockfile, kept
separate so the GPUI/Zed graph does not enter every other phux consumer. Its
path dependencies are the root phux crates, so when a root crate changes a
registry dependency's semver requirement the host lock must be re-resolved.
Nothing does that automatically, and `just desktop-app` then aborts before it
compiles: cargo refuses to update the host lock because `--locked` was passed.

This is a STATIC check. It reads the root and host lockfiles and the root
crates' manifests only, so it runs on a bare checkout with no GPUIX source, no
network and no Rust (CI runs it that way). For every path crate present in both
locks it checks two things:

- Every non-optional normal or build dependency the crate's Cargo.toml declares
  (for any target; a lockfile is target-independent) is an edge in the host
  lock. Such a dependency is unconditional, so a missing edge means `--locked`
  would have to add it: a root crate gained a dependency the host lock never
  resolved.
- The version selected for each dependency name both locks record stays within
  one semver compatibility boundary (major, or major.minor while major is 0);
  otherwise the host lock no longer satisfies the root crate's requirement.
  Patch drift within a boundary (thiserror 2.0.20 versus 2.0.21) stays valid
  under `--locked` and is not reported.

Dev-dependencies and optional dependencies are not required: the host builds
the root crates as path dependencies with its own feature set, so the root
lock's feature union and dev graph legitimately exceed the host's. A new
dependency behind a feature the host enables is therefore invisible here and
surfaces at `just desktop-lock-refresh` or the `--locked` host build.

Pass --fix to rewrite repairable drift in HOST_LOCK: a missing edge is added,
and a stale edge rewritten to the root's selected version, when the host lock
already carries that package; a path crate version is aligned with the root
workspace. Anything left is not repairable without resolving against the
bootstrapped GPUIX source (`just desktop-source`).

Usage: check-desktop-host-lock.py [--fix] [ROOT_LOCK [HOST_LOCK]]
"""

import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_ROOT_LOCK = ROOT / "Cargo.lock"
DEFAULT_HOST_LOCK = ROOT / "clients" / "desktop" / "native" / "Cargo.lock"

PACKAGE = re.compile(r"(?ms)^\[\[package\]\]\n.*?(?=^\[\[package\]\]|\Z)")
NAME = re.compile(r'(?m)^name = "([^"]+)"$')
VERSION = re.compile(r'(?m)^version = "([^"]+)"$')
SOURCE = re.compile(r"(?m)^source = ")
DEPENDENCIES = re.compile(r"(?ms)^dependencies = \[\n(.*?)^\]")
DEPENDENCY_LINE = re.compile(r'^ "(.+)",$', re.M)


class Package:
    def __init__(self, block: str) -> None:
        self.block = block
        self.name = NAME.search(block).group(1)
        self.version = VERSION.search(block).group(1)
        self.is_path = not SOURCE.search(block)
        match = DEPENDENCIES.search(block)
        self.dependencies = DEPENDENCY_LINE.findall(match.group(1)) if match else []


def parse(text: str) -> list[Package]:
    return [Package(block.group(0)) for block in PACKAGE.finditer(text)]


def versions_by_name(packages: list[Package]) -> dict[str, list[str]]:
    versions: dict[str, list[str]] = {}
    for package in packages:
        versions.setdefault(package.name, []).append(package.version)
    return versions


def compat(version: str) -> str:
    """The semver compatibility boundary of a resolved version."""
    major, _, rest = version.partition("+")[0].partition(".")
    minor = rest.partition(".")[0] if rest else "0"
    return major if major != "0" else f"{major}.{minor}"


def selection(entry: str, versions: dict[str, list[str]]) -> tuple[str, str | None]:
    """A lock dependency entry as (name, version); None when ambiguous."""
    name, separator, version = entry.partition(" ")
    if separator:
        return name, version
    candidates = versions.get(name, [])
    return name, candidates[0] if len(candidates) == 1 else None


def selections(package: Package, versions: dict[str, list[str]]) -> dict[str, tuple[str, str | None]]:
    resolved = {}
    for entry in package.dependencies:
        name, version = selection(entry, versions)
        resolved[name] = (entry, version)
    return resolved


def required_dependencies(root: Path) -> dict[str, set[str]]:
    """Each root workspace crate's unconditional (non-dev, non-optional) packages."""
    inherited = tomllib.loads((root / "Cargo.toml").read_text())["workspace"].get(
        "dependencies", {}
    )
    required = {}
    for manifest in sorted(root.glob("crates/*/Cargo.toml")):
        data = tomllib.loads(manifest.read_text())
        tables = [data, *data.get("target", {}).values()]
        required[data["package"]["name"]] = {
            name
            for table in tables
            for kind in ("dependencies", "build-dependencies")
            for key, spec in table.get(kind, {}).items()
            if (name := required_package(key, spec, inherited)) is not None
        }
    return required


def required_package(key: str, spec, inherited: dict) -> str | None:
    """The package a manifest entry names, or None when it is optional."""
    if not isinstance(spec, dict):
        return key
    if spec.get("optional", False):
        return None
    if spec.get("workspace", False):
        spec = inherited.get(key, {})
    return spec.get("package", key) if isinstance(spec, dict) else key


class Finding:
    def __init__(self, message: str, fix=None) -> None:
        self.message = message
        self.fix = fix


def compare(
    root_text: str, host_text: str, required: dict[str, set[str]] | None = None
) -> list[Finding]:
    root_packages = parse(root_text)
    host_packages = parse(host_text)
    root_versions = versions_by_name(root_packages)
    host_versions = versions_by_name(host_packages)
    host_path = {p.name: p for p in host_packages if p.is_path}
    host_index = {(p.name, p.version) for p in host_packages}

    findings = []
    for crate in (p for p in root_packages if p.is_path):
        host_crate = host_path.get(crate.name)
        if host_crate is None:
            continue
        if crate.version != host_crate.version:
            findings.append(
                Finding(
                    f"path crate {crate.name} is {host_crate.version}; root workspace is {crate.version}",
                    version_fix(crate.name, crate.version),
                )
            )
        root_selected = selections(crate, root_versions)
        host_selected = selections(host_crate, host_versions)
        for dependency in sorted((required or {}).get(crate.name, set()) - set(host_selected)):
            findings.append(
                Finding(
                    f"{crate.name} depends on {dependency}, but the host lock records no "
                    "such edge (cargo --locked would have to add it)",
                    missing_edge_fix(crate.name, dependency, host_versions),
                )
            )
        for dependency in sorted(set(root_selected) & set(host_selected)):
            _, root_version = root_selected[dependency]
            host_entry, host_version = host_selected[dependency]
            if root_version is None or host_version is None:
                continue
            if compat(root_version) == compat(host_version):
                continue
            fixable = (dependency, root_version) in host_index
            findings.append(
                Finding(
                    f"{crate.name} selects {dependency} {host_version}, but the root lock "
                    f"selects {root_version}",
                    edge_fix(crate.name, host_entry, dependency, root_version) if fixable else None,
                )
            )
    return findings


def version_fix(crate: str, version: str):
    def apply(text: str) -> str:
        package = Package(block_for(text, crate))
        rewritten = VERSION.sub(f'version = "{version}"', package.block, count=1)
        return text.replace(package.block, rewritten, 1)

    return apply


def edge_fix(crate: str, host_entry: str, dependency: str, version: str):
    def apply(text: str) -> str:
        package = Package(block_for(text, crate))
        old = f' "{host_entry}",'
        new = f' "{dependency} {version}",'
        return text.replace(package.block, package.block.replace(old, new, 1), 1)

    return apply


def missing_edge_fix(crate: str, dependency: str, versions: dict[str, list[str]]):
    """Add an edge to a package the host lock carries once, spelled as cargo does."""
    # A name the lock carries once is written bare; adding an edge leaves the
    # package set, and so every other entry's spelling, unchanged. A name it
    # lacks or carries at several versions needs real resolution.
    if len(versions.get(dependency, [])) != 1:
        return None

    def apply(text: str) -> str:
        package = Package(block_for(text, crate))
        entries = sorted([*package.dependencies, dependency])
        body = "".join(f' "{entry}",\n' for entry in entries)
        match = DEPENDENCIES.search(package.block)
        if match is None:
            raise SystemExit(f"{crate} has no dependency list in the host lock")
        rewritten = package.block.replace(match.group(1), body, 1)
        return text.replace(package.block, rewritten, 1)

    return apply


def block_for(text: str, crate: str) -> str:
    for block in PACKAGE.finditer(text):
        body = block.group(0)
        if not SOURCE.search(body) and NAME.search(body).group(1) == crate:
            return body
    raise SystemExit(f"{crate} not found in host lock")


def main(argv: list[str]) -> int:
    fix = "--fix" in argv
    paths = [Path(arg) for arg in argv if not arg.startswith("-")]
    root_lock = paths[0] if paths else DEFAULT_ROOT_LOCK
    host_lock = paths[1] if len(paths) > 1 else DEFAULT_HOST_LOCK
    if len(paths) > 2:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2

    root_text = root_lock.read_text()
    host_text = host_lock.read_text()
    required = required_dependencies(root_lock.resolve().parent)

    if fix:
        changed = 0
        for finding in (f for f in compare(root_text, host_text, required) if f.fix is not None):
            host_text = finding.fix(host_text)
            changed += 1
        if changed:
            host_lock.write_text(host_text)
            print(f"fixed {changed} entr{'y' if changed == 1 else 'ies'} in {host_lock}")

    findings = compare(root_text, host_text, required)
    if not findings:
        print(f"{host_lock} is in step with {root_lock}")
        return 0

    for finding in findings:
        print(f"{host_lock}: {finding.message}", file=sys.stderr)
        if finding.fix is None:
            print("  refresh the host lock: just desktop-lock-refresh", file=sys.stderr)
        else:
            print("  repair with: just desktop-lock-fix", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
