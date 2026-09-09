#!/usr/bin/env python3
"""Install a verified immutable Zentinel source build into a private tool cache."""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def install(scratch_parent=None):
    pin = json.loads(Path(__file__).with_suffix(".json").read_text())
    version = subprocess.check_output(["zig", "version"], text=True).strip()
    if version != pin["zig_version"]:
        raise ValueError(f"Zig {pin['zig_version']} required; found {version}")
    cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    destination = cache / "phux/mutation/zentinel" / pin["revision"]
    binary = destination / "zentinel"
    stamp = destination / "binary.sha256"
    if binary.is_file() and stamp.is_file() and digest(binary) == stamp.read_text().strip():
        return binary
    destination.parent.mkdir(parents=True, exist_ok=True)
    if scratch_parent is None:
        scratch_parent = destination.parent
    with tempfile.TemporaryDirectory(prefix="install-", dir=scratch_parent) as work:
        work = Path(work)
        archive = work / "source.tar.gz"
        url = f"https://codeload.github.com/oly-wan-kenobi/zentinel/tar.gz/{pin['revision']}"
        with urllib.request.urlopen(url, timeout=60) as response:
            archive.write_bytes(response.read())
        if digest(archive) != pin["archive_sha256"]:
            raise ValueError("Zentinel source archive checksum mismatch")
        with tarfile.open(archive) as source:
            source.extractall(work, filter="data")
        root = work / f"zentinel-{pin['revision']}"
        env = dict(os.environ, ZIG_GLOBAL_CACHE_DIR=str(work / "global-cache"))
        for command in (["zig", "build", "test", "-j2", "--summary", "all"], ["zig", "build", "-j2"]):
            subprocess.run(command, cwd=root, env=env, check=True, stdout=sys.stderr,
                           timeout=300)
        destination.mkdir(parents=True, exist_ok=True)
        staged_binary = work / "zentinel"
        shutil.copy2(root / "zig-out/bin/zentinel", staged_binary)
        # A concurrently starting runner never sees a partially written binary.
        # Cache and TMPDIR may be on different filesystems, so stage beside it.
        with tempfile.NamedTemporaryFile(dir=destination, delete=False) as publish:
            publish_path = Path(publish.name)
        shutil.copy2(staged_binary, publish_path)
        os.replace(publish_path, binary)
        stamp.write_text(digest(binary) + "\n")
    return binary


if __name__ == "__main__":
    try:
        print(install(sys.argv[1] if len(sys.argv) > 1 else None))
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"zig-tool: {error}", file=sys.stderr)
        sys.exit(2)
