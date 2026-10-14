#!/usr/bin/env python3
"""Collapse duplicate mbx target bytes onto the object store.

Workspace restore reflinks when the filesystem can, and copies otherwise.
GitHub-hosted ext4 copies, so the test lane holds the object store twice:
once under ``actions/cas`` and again in the managed target. The post step
then byte-copies the store into the export directory. Run 38505694112 had
a 36.5 GiB store, a 37.3 GiB target, and 30 GiB free, and that copy
returned ENOSPC after the tests had passed.

``hardlink(1)`` keeps one inode for identical contents. Cargo's exported
scheduler state records each target file's mode and mtime, and hardlink
keeps whichever inode it chooses, so this restores that metadata afterwards.
Two target files that need different metadata cannot share an inode; the
second one is copied back apart.
"""

from __future__ import annotations

import os
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path


def iter_files(root: Path):
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames[:] = [
            name
            for name in dirnames
            if not os.path.islink(os.path.join(dirpath, name))
        ]
        for name in filenames:
            path = Path(dirpath) / name
            if path.is_symlink():
                continue
            yield path


def snapshot(root: Path) -> dict[Path, os.stat_result]:
    found: dict[Path, os.stat_result] = {}
    for path in iter_files(root):
        try:
            found[path] = path.stat()
        except FileNotFoundError:
            continue
    return found


def split_copy(path: Path) -> None:
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=".phux-split-", dir=path.parent
    )
    os.close(descriptor)
    temporary = Path(temporary_name)
    try:
        shutil.copyfile(path, temporary)
        os.replace(temporary, path)
    except Exception:
        temporary.unlink(missing_ok=True)
        raise


def restore(saved: dict[Path, os.stat_result]) -> None:
    """Put each target file's mode and mtime back after hardlink."""
    applied: dict[tuple[int, int], tuple[int, int]] = {}
    for path, old in saved.items():
        try:
            current = path.stat()
        except FileNotFoundError:
            continue
        want = (stat.S_IMODE(old.st_mode), old.st_mtime_ns)
        key = (current.st_dev, current.st_ino)
        previous = applied.get(key)
        if previous is not None and previous != want:
            try:
                split_copy(path)
            except OSError as error:
                print(
                    f"mbx-export-headroom: could not split {path}: {error}",
                    file=sys.stderr,
                )
                continue
            current = path.stat()
            key = (current.st_dev, current.st_ino)
        os.chmod(path, want[0])
        os.utime(path, ns=(old.st_atime_ns, old.st_mtime_ns))
        applied[key] = want


def free_bytes(path: Path) -> int:
    usage = os.statvfs(path)
    return usage.f_bavail * usage.f_frsize


def collapse(cache_dir: Path) -> None:
    targets = cache_dir / "targets"
    cas = cache_dir / "actions" / "cas" / "v1"
    if not targets.is_dir() or not cas.is_dir():
        print("mbx-export-headroom: no managed target to collapse")
        return
    saved = snapshot(targets)
    if not saved:
        print("mbx-export-headroom: managed target is empty")
        return
    hardlink = shutil.which("hardlink")
    if hardlink is None:
        print(
            "mbx-export-headroom: hardlink is not installed; leaving target copies",
            file=sys.stderr,
        )
        return
    before = free_bytes(cache_dir)
    command = [hardlink, "-c", "-q"]
    # Darwin's util-linux build has no reflink option and uses hardlinks.
    if sys.platform != "darwin":
        command.append("--reflink=never")
    command.extend((str(targets), str(cas)))
    completed = subprocess.run(
        command,
        check=False,
        text=True,
        capture_output=True,
    )
    if completed.returncode != 0:
        if completed.stderr:
            print(completed.stderr, file=sys.stderr, end="")
        print(
            "mbx-export-headroom: hardlink failed; leaving remaining target copies",
            file=sys.stderr,
        )
    restore(saved)
    freed = free_bytes(cache_dir) - before
    print(
        "mbx-export-headroom: collapsed duplicate target files onto the "
        f"object store ({len(saved)} files, {max(freed, 0)} bytes freed)"
    )


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(f"usage: {argv[0]} CACHE_DIR", file=sys.stderr)
        return 2
    collapse(Path(argv[1]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
