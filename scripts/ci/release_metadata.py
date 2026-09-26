#!/usr/bin/env python3
"""Release-please version bumps are not compile inputs.

A root release PR rewrites version fields, the changelog, and the hosted
PHUX_VERSION pin. Those bytes do not change Rust, Zig, or the coordinator.
Treating them as shared binary inputs rebuilds FFI, the Zig test graph, and
the CLI (PR 853: 18 minutes, cancelled still linking). Fail closed: any other
hunk, path, or unreadable side keeps the normal classifier.
"""

import json
import re
import tomllib

SEMVER = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+")
SKILL_VERSION = re.compile(
    r'^(\s*)version:\s*"([0-9]+\.[0-9]+\.[0-9]+)"\s*#\s*x-release-please-version\s*$'
)
DOCKER_VERSION = re.compile(
    r'^(ARG PHUX_VERSION=[0-9]+\.[0-9]+\.[0-9]+'
    r'|RUN test "\$PHUX_VERSION" = "[0-9]+\.[0-9]+\.[0-9]+")$'
)

EXACT = {
    "Cargo.toml": "workspace_manifest",
    "Cargo.lock": "workspace_lock",
    "clients/phux-web/Cargo.lock": "workspace_lock",
    ".release-please-manifest.json": "manifest",
    "docs/site/worker/Dockerfile": "dockerfile",
    ".agents/skills/using-phux/SKILL.md": "skill",
    ".agents/skills/using-phux-mcp/SKILL.md": "skill",
}


def kind_of(path):
    if path == "CHANGELOG.md" or path.endswith("/CHANGELOG.md"):
        return "changelog"
    return EXACT.get(path)


def changelog_only(_before, _after):
    return True


def workspace_manifest_only(before, after):
    try:
        old = tomllib.loads(before)
        new = tomllib.loads(after)
        old_version = old["workspace"]["package"].pop("version")
        new_version = new["workspace"]["package"].pop("version")
    except (KeyError, tomllib.TOMLDecodeError, TypeError):
        return False
    if not isinstance(new_version, str) or SEMVER.fullmatch(new_version) is None:
        return False
    if not isinstance(old_version, str) or SEMVER.fullmatch(old_version) is None:
        return False
    return old == new


def _split_lock(text):
    try:
        packages = tomllib.loads(text).get("package", [])
    except tomllib.TOMLDecodeError:
        return None
    path, registry = {}, []
    for package in packages:
        if not isinstance(package, dict) or "name" not in package:
            return None
        if "source" in package:
            registry.append(package)
            continue
        name = package["name"]
        if name in path:
            return None
        path[name] = package
    return path, registry


def workspace_lock_only(before, after):
    old = _split_lock(before)
    new = _split_lock(after)
    if old is None or new is None:
        return False
    old_path, old_registry = old
    new_path, new_registry = new
    if old_registry != new_registry or set(old_path) != set(new_path):
        return False
    for name, previous in old_path.items():
        current = new_path[name]
        previous_rest = {key: value for key, value in previous.items() if key != "version"}
        current_rest = {key: value for key, value in current.items() if key != "version"}
        if previous_rest != current_rest:
            return False
        version = current.get("version")
        if not isinstance(version, str) or SEMVER.fullmatch(version) is None:
            return False
    return True


def manifest_only(before, after):
    try:
        old = json.loads(before)
        new = json.loads(after)
    except json.JSONDecodeError:
        return False
    if not isinstance(old, dict) or not isinstance(new, dict) or set(old) != set(new):
        return False
    for value in (*old.values(), *new.values()):
        if not isinstance(value, str) or SEMVER.fullmatch(value) is None:
            return False
    return True


def _version_lines_only(before, after, pattern):
    old_lines = before.splitlines()
    new_lines = after.splitlines()
    if len(old_lines) != len(new_lines):
        return False
    for old, new in zip(old_lines, new_lines):
        if old == new:
            continue
        if pattern.fullmatch(old) is None or pattern.fullmatch(new) is None:
            return False
    return True


def skill_only(before, after):
    return _version_lines_only(before, after, SKILL_VERSION)


def dockerfile_only(before, after):
    return _version_lines_only(before, after, DOCKER_VERSION)


CHECKS = {
    "changelog": changelog_only,
    "workspace_manifest": workspace_manifest_only,
    "workspace_lock": workspace_lock_only,
    "manifest": manifest_only,
    "dockerfile": dockerfile_only,
    "skill": skill_only,
}


def only(paths, read_pair):
    """True when every path is release metadata and each pair is version-only."""
    if not paths:
        return False
    for path in paths:
        kind = kind_of(path)
        if kind is None:
            return False
        pair = read_pair(path)
        if not pair or pair[0] is None or pair[1] is None:
            return False
        if not CHECKS[kind](pair[0], pair[1]):
            return False
    return True


def skip_outputs():
    """Classifier-shaped outputs that request no compile lane."""
    surfaces = (
        "phux", "cockpit", "web", "web_engine", "integrations", "native",
        "ffi", "cli", "zig", "shipping",
    )
    lines = [f"{name}_needed=false" for name in surfaces]
    lines.extend([
        "docs_only=false",
        "workflow_only=false",
        "release_metadata_only=true",
        "test_filterset=",
        "unit_mode=skip",
        "unit_targets=",
        "e2e_needed=false",
    ])
    return lines
