#!/usr/bin/env python3
"""Fetch and verify the matched GPUIX/Solid source without changing existing trees."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


DESKTOP = Path(__file__).resolve().parent.parent
SOURCE = DESKTOP / "toolchain" / "gpuix"


def git(directory: Path, *arguments: str) -> str:
    return subprocess.check_output(
        ["git", "-C", str(directory), *arguments], text=True
    ).strip()


def verify_clean(source: Path) -> None:
    changes = git(source, "status", "--porcelain", "--untracked-files=normal")
    # The user's index may hide edits with assume-unchanged/skip-worktree.
    changes = changes or patch_tree_changes(source, [])
    if changes:
        raise SystemExit(f"Modified source in {source}; preserve it and investigate:\n{changes}")


def pinned_patches(pin: dict) -> list[Path]:
    patches = []
    for relative, expected in pin.get("patches", {}).items():
        patch = DESKTOP / "toolchain" / relative
        if hashlib.sha256(patch.read_bytes()).hexdigest() != expected:
            raise SystemExit(f"Source patch checksum mismatch: {relative}")
        patches.append(patch)
    return patches


def patch_tree_changes(source: Path, patches: list[Path]) -> str:
    # A temporary index describes the reviewed patch tree without changing the
    # user's index. New patch files must be compared too, not merely tracked diffs.
    with tempfile.TemporaryDirectory(prefix="phux-source-index-") as scratch:
        environment = dict(os.environ, GIT_INDEX_FILE=str(Path(scratch) / "index"))
        command = ["git", "-C", str(source)]
        subprocess.run([*command, "read-tree", "HEAD"], env=environment, check=True)
        for patch in patches:
            subprocess.run([*command, "apply", "--cached", str(patch)], env=environment, check=True)
        changed = subprocess.check_output(
            [*command, "diff", "--name-only"], env=environment, text=True
        ).strip()
        untracked = subprocess.check_output(
            [*command, "ls-files", "--others", "--exclude-standard"], env=environment, text=True
        ).strip()
        return "\n".join(filter(None, [changed, untracked]))


def verify_destination(source: Path, relative: str) -> None:
    destination = source / relative
    if os.path.lexists(destination):
        raise SystemExit(f"Source patch would replace existing path: {relative}")
    for ancestor in destination.parents:
        if ancestor == source:
            return
        if ancestor.is_symlink() or (ancestor.exists() and not ancestor.is_dir()):
            raise SystemExit(f"Source patch destination has obstructing parent: {ancestor}")


def verify_created_paths(source: Path, patches: list[Path], prefix: int) -> None:
    """Refuse even ignored worktree collisions before applying any suffix."""
    with tempfile.TemporaryDirectory(prefix="phux-source-index-") as scratch:
        environment = dict(os.environ, GIT_INDEX_FILE=str(Path(scratch) / "index"))
        command = ["git", "-C", str(source)]
        subprocess.run([*command, "read-tree", "HEAD"], env=environment, check=True)
        for patch in patches[:prefix]:
            subprocess.run([*command, "apply", "--cached", str(patch)], env=environment, check=True)
        baseline = subprocess.check_output([*command, "write-tree"], env=environment, text=True).strip()
        for patch in patches[prefix:]:
            subprocess.run([*command, "apply", "--cached", str(patch)], env=environment, check=True)
            created = subprocess.check_output(
                [*command, "diff", "--cached", "--no-renames", "--name-only", "--diff-filter=A", "-z", baseline],
                env=environment, text=True,
            )
            for relative in filter(None, created.split("\0")):
                verify_destination(source, relative)


def prepare_patches(source: Path, patches: list[Path]) -> None:
    if not patches:
        verify_clean(source)
        return
    # Reject staged work even when its worktree happens to match the reviewed patch.
    if git(source, "diff", "--cached", "--name-only"):
        raise SystemExit(f"Staged source in {source}; preserve it and investigate.")
    # An exact earlier prefix can advance when a new reviewed patch is added.
    # Partial or independently edited prefixes are never repaired in place.
    for count in range(len(patches), -1, -1):
        if patch_tree_changes(source, patches[:count]):
            continue
        verify_created_paths(source, patches, count)
        for patch in patches[count:]:
            git(source, "apply", str(patch))
        changes = patch_tree_changes(source, patches)
        if changes:
            raise SystemExit(f"Patched source verification failed in {source}:\n{changes}")
        return
    raise SystemExit(f"Modified source beyond pinned patches in {source}; preserve it and investigate.")


def verify_source(source: Path, pin: dict) -> None:
    if git(source, "rev-parse", "HEAD") != pin["revision"]:
        raise SystemExit(f"Unexpected GPUIX revision in {source}; preserve it and investigate.")
    if git(source / "zed", "rev-parse", "HEAD") != pin["zedRevision"]:
        raise SystemExit("Zed revision does not match the GPUIX source pin.")
    for relative, expected in pin["sha256"].items():
        actual = hashlib.sha256((source / relative).read_bytes()).hexdigest()
        if actual != expected:
            raise SystemExit(f"Source lockfile checksum mismatch: {relative}")
    for package in ("native", "solid"):
        manifest = json.loads((source / "packages" / package / "package.json").read_text())
        if manifest["version"] != pin["version"]:
            raise SystemExit(f"Mismatched @gpuix/{package} version")
    verify_clean(source / "zed")
    prepare_patches(source, pinned_patches(pin))


def main() -> None:
    pin = json.loads((DESKTOP / "toolchain" / "source.json").read_text())
    if not SOURCE.exists():
        subprocess.run(
            ["git", "clone", "--no-checkout", pin["repository"], str(SOURCE)],
            check=True,
        )
        git(SOURCE, "checkout", "--detach", pin["revision"])
        git(SOURCE, "submodule", "update", "--init", "--depth", "1", "zed")
    verify_source(SOURCE, pin)
    subprocess.run(
        ["bun", "install", "--frozen-lockfile", "--ignore-scripts"],
        cwd=SOURCE,
        check=True,
    )
    print(f"Verified GPUIX {pin['version']} at {pin['revision']}")


if __name__ == "__main__":
    main()
