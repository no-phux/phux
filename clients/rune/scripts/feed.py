#!/usr/bin/env python3
"""Write the update feed for a Rune release.

    feed.py VERSION SHA DIST_DIR PROTOCOL_SOURCE > rune.json

Digests come from the sidecars the release signs, sizes from the archives.
The feed is unsigned: clients believe a digest only once it matches the
minisigned sidecar, and name assets themselves.
"""
import json
import os
import re
import sys

ARCHIVES = {
    "aarch64-apple-darwin": "macos-arm64.zip",
    "x86_64-unknown-linux-gnu": "linux-x86_64.tar.gz",
}


def entry(dist, archive):
    path = os.path.join(dist, archive)
    with open(f"{path}.sha256", encoding="utf-8") as sidecar:
        digest, name = sidecar.read().split()
    if name.lstrip("*") != archive:
        sys.exit(f"{archive}.sha256 names {name}")
    return {"sha256": digest, "size": os.path.getsize(path)}


def main():
    version, sha, dist, source = sys.argv[1:]
    with open(source, encoding="utf-8") as lib:
        found = re.search(
            r"PROTOCOL_VERSION: Version = Version \{\s*major: (\d+),\s*minor: (\d+)", lib.read()
        )
    if not found:
        sys.exit(f"no PROTOCOL_VERSION in {source}")
    targets = {
        target: entry(dist, f"phux-rune-{version}-{suffix}")
        for target, suffix in ARCHIVES.items()
    }
    feed = {
        "schema": 1,
        "version": version,
        "sha": sha,
        "protocol": f"{found[1]}.{found[2]}",
        "targets": targets,
    }
    json.dump(feed, sys.stdout, indent=2)
    print()


main()
