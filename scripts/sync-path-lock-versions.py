#!/usr/bin/env python3
"""Align a standalone Cargo.lock's phux path crates with the root lock.

The desktop host (clients/desktop/native) is its own Cargo workspace whose
path dependencies include the GPUIX checkout, which is bootstrapped rather than
committed. `cargo update --workspace` therefore cannot run there on a bare
release checkout, yet a release bump still changes the versions its lock
records for the root path crates, and `--locked` builds then refuse to start.

A path package carries no `source` line, and its version is exactly what the
root workspace declares. This rewrites the version of every sourceless package
in each target lock whose name is also a sourceless package in the root lock,
leaving every other byte untouched.

Usage: sync-path-lock-versions.py ROOT_LOCK TARGET_LOCK...
"""

import re
import sys
from pathlib import Path

PACKAGE = re.compile(r"(?ms)^\[\[package\]\]\n.*?(?=^\[\[package\]\]|\Z)")
NAME = re.compile(r'(?m)^name = "([^"]+)"$')
VERSION = re.compile(r'(?m)^version = "([^"]+)"$')
SOURCE = re.compile(r"(?m)^source = ")


def path_versions(text: str) -> dict[str, str]:
    versions = {}
    for block in PACKAGE.finditer(text):
        body = block.group(0)
        if SOURCE.search(body):
            continue
        versions[NAME.search(body).group(1)] = VERSION.search(body).group(1)
    return versions


def sync(text: str, versions: dict[str, str]) -> str:
    def rewrite(block: re.Match[str]) -> str:
        body = block.group(0)
        name = NAME.search(body).group(1)
        if SOURCE.search(body) or name not in versions:
            return body
        return VERSION.sub(f'version = "{versions[name]}"', body, count=1)

    return PACKAGE.sub(rewrite, text)


def main(argv: list[str]) -> int:
    if len(argv) < 3:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    versions = path_versions(Path(argv[1]).read_text())
    for target in map(Path, argv[2:]):
        before = target.read_text()
        after = sync(before, versions)
        if after != before:
            target.write_text(after)
            print(f"updated {target}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
