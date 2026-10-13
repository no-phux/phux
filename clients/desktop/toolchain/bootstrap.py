#!/usr/bin/env python3
"""Materialize the pinned, patched GPUI crates the desktop host builds against.

`source.json` pins each crates.io archive by SHA-256 and each reviewed patch by
SHA-256. The result lives in `toolchain/gpui/<crate>` (ignored build state) and
is wired into the host through `[patch.crates-io]` in `native/Cargo.toml`.

The output is a pure function of the pins. An existing tree is verified
against a fresh materialization and never overwritten: any difference fails,
so local edits stay in place for inspection.

Usage:
  bootstrap.py            materialize (or verify) toolchain/gpui
  bootstrap.py --check    verify only; fail when missing
  bootstrap.py --edit     materialize as a git repo: one commit per patch
  bootstrap.py --export   write that repo's commits back as patches/ + pins

Changing the framework: `--edit`, change code under toolchain/gpui, commit
(amend the matching patch commit, or add a new commit for a new patch), then
`--export`. The commit subject names the patch file. `--export` rewrites
`patches/` and `source.json` to exactly the repo's series, so review the diff.
"""

import filecmp
import hashlib
import json
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

TOOLCHAIN = Path(__file__).resolve().parent
OUTPUT = TOOLCHAIN / "gpui"
CACHE = TOOLCHAIN.parent / ".cache" / "crates"


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def fetch(registry: str, name: str, version: str, expected: str) -> bytes:
    cached = CACHE / f"{name}-{version}.crate"
    if cached.exists():
        data = cached.read_bytes()
        if sha256(data) == expected:
            return data
        raise SystemExit(f"cached {cached} does not match its pin; preserve it and investigate")
    url = f"{registry}/{name}/{name}-{version}.crate"
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read()
    if sha256(data) != expected:
        raise SystemExit(f"{url} does not match its pinned sha256")
    CACHE.mkdir(parents=True, exist_ok=True)
    cached.write_bytes(data)
    return data


def unpack(data: bytes, name: str, version: str, into: Path) -> None:
    with tempfile.NamedTemporaryFile(suffix=".crate") as archive:
        archive.write(data)
        archive.flush()
        with tarfile.open(archive.name) as tar:
            tar.extractall(into, filter="data")
    (into / f"{name}-{version}").rename(into / name)


def git(cwd: Path, *arguments: str) -> str:
    return subprocess.run(
        ["git", "-c", "user.name=phux", "-c", "user.email=phux@localhost", *arguments],
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    ).stdout


def unpack_crates(pin: dict, into: Path) -> None:
    for name, crate in pin["crates"].items():
        data = fetch(pin["registry"], name, crate["version"], crate["sha256"])
        unpack(data, name, crate["version"], into)


def pinned_patches(pin: dict) -> list[Path]:
    patches = []
    for relative, expected in pin["patches"].items():
        patch = TOOLCHAIN / relative
        if sha256(patch.read_bytes()) != expected:
            raise SystemExit(f"patch checksum mismatch: {relative}")
        patches.append(patch)
    return patches


def materialize(pin: dict, into: Path, commits: bool = False) -> None:
    unpack_crates(pin, into)
    if commits:
        git(into, "init", "-q")
        git(into, "add", "-A")
        git(into, "commit", "-qm", "base: pinned crates.io sources")
    for patch in pinned_patches(pin):
        git(into, "apply", "--whitespace=nowarn", str(patch))
        if commits:
            git(into, "add", "-A")
            git(into, "commit", "-qm", patch.stem)


def export(pin: dict) -> None:
    if not (OUTPUT / ".git").exists():
        raise SystemExit(f"{OUTPUT} is not an --edit checkout")
    if git(OUTPUT, "status", "--porcelain").strip():
        raise SystemExit(f"commit or discard the changes in {OUTPUT} first")
    commits = git(OUTPUT, "rev-list", "--reverse", "HEAD").split()
    series = {}
    for commit in commits[1:]:
        subject = git(OUTPUT, "log", "-1", "--format=%s", commit).strip()
        relative = f"patches/{subject}.patch"
        diff = git(OUTPUT, "diff", "--no-color", "--no-renames", f"{commit}^", commit)
        (TOOLCHAIN / relative).write_text(diff)
        series[relative] = sha256(diff.encode())
    for stale in (TOOLCHAIN / "patches").glob("*.patch"):
        if f"patches/{stale.name}" not in series:
            stale.unlink()
    pin["patches"] = series
    (TOOLCHAIN / "source.json").write_text(json.dumps(pin, indent=2) + "\n")
    print(f"exported {len(series)} patch(es); review patches/ and source.json")


def same_tree(left: Path, right: Path) -> bool:
    comparison = filecmp.dircmp(left, right, ignore=[".git"])
    if comparison.left_only or comparison.right_only or comparison.funny_files:
        return False
    _, mismatch, errors = filecmp.cmpfiles(left, right, comparison.common_files, shallow=False)
    if mismatch or errors:
        return False
    return all(same_tree(left / sub, right / sub) for sub in comparison.common_dirs)


def main() -> None:
    mode = sys.argv[1:]
    if mode not in ([], ["--check"], ["--edit"], ["--export"]):
        raise SystemExit(__doc__)
    check = mode == ["--check"]
    pin = json.loads((TOOLCHAIN / "source.json").read_text())
    if mode == ["--export"]:
        export(pin)
        return
    with tempfile.TemporaryDirectory(prefix="phux-gpui-") as scratch:
        fresh = Path(scratch) / "gpui"
        fresh.mkdir()
        materialize(pin, fresh, commits=mode == ["--edit"])
        if mode == ["--edit"] and OUTPUT.exists() and not (OUTPUT / ".git").exists():
            if not same_tree(OUTPUT, fresh):
                raise SystemExit(f"{OUTPUT} has unrecorded edits; preserve them and investigate")
            shutil.rmtree(OUTPUT)
        if OUTPUT.exists():
            if not same_tree(OUTPUT, fresh):
                raise SystemExit(
                    f"{OUTPUT} differs from the pinned, patched source; preserve it and "
                    "investigate (move it aside to rebuild)"
                )
            print(f"verified pinned GPUI source at {OUTPUT}")
            return
        if check:
            raise SystemExit(f"{OUTPUT} is missing; run just desktop-source")
        shutil.move(str(fresh), OUTPUT)
    versions = ", ".join(f"{name} {crate['version']}" for name, crate in pin["crates"].items())
    print(f"materialized {versions} with {len(pin['patches'])} patch(es) at {OUTPUT}")


if __name__ == "__main__":
    main()
