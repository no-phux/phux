#!/usr/bin/env python3
"""The release rules of a Rune tag, in one tested place.

    feed.py build VERSION SHA DIST_DIR PROTOCOL_SOURCE > rune.json
    feed.py newest < tags
    feed.py preflight TAG CARGO_TOML RELEASE_NOTES
    feed.py release-flags VERSION

`build` writes the update feed. Digests come from the sidecars the release
signs, sizes from the archives. The feed is unsigned: the app believes a digest
only once it matches the minisigned sidecar, and names assets itself. install.sh
does the same with minisign on PATH, and otherwise trusts this digest.

`newest` prints the highest of the tags on stdin, in the order clients use
(src/updates.rs): 0.1.0 sorts after its alphas, which `sort -V` gets backwards.
A rune-v tag that order can't place is a failure, not something to skip.

`preflight` refuses a tag before anything builds: `release-flags` prints the
`gh release create` arguments a version implies, one per line.
"""
import json
import os
import re
import sys

ARCHIVES = {
    "aarch64-apple-darwin": "macos-arm64.zip",
    "x86_64-unknown-linux-gnu": "linux-x86_64.tar.gz",
}
NUMBER = r"(0|[1-9][0-9]*)"
VERSION = re.compile(rf"{NUMBER}\.{NUMBER}\.{NUMBER}(?:-alpha\.{NUMBER})?")
TAG_PREFIX = "rune-v"


def version_key(version):
    """A sortable key for what clients accept, or None for anything else."""
    found = VERSION.fullmatch(version)
    if not found:
        return None
    major, minor, patch, alpha = found.groups()
    stage = (0, int(alpha)) if alpha is not None else (1, 0)
    return (int(major), int(minor), int(patch), *stage)


def newest(tags):
    """The highest rune-v tag; other products' tags are not ours to judge."""
    ours = [tag for tag in tags if tag.startswith(TAG_PREFIX)]
    suspect = [tag for tag in ours if version_key(tag[len(TAG_PREFIX) :]) is None]
    if suspect:
        raise ValueError(f"clients can't order {', '.join(suspect)}")
    if not ours:
        raise ValueError("no rune-v tag")
    return max(ours, key=lambda tag: version_key(tag[len(TAG_PREFIX) :]))


def cargo_version(manifest):
    found = re.search(r'^version = "(.*)"$', manifest, re.MULTILINE)
    return found[1] if found else None


def preflight(tag, manifest, notes):
    """Every reason `tag` must not build; empty when it may."""
    if not tag.startswith(TAG_PREFIX):
        return [f"{tag} is not a rune-v tag"]
    version = tag[len(TAG_PREFIX) :]
    problems = []
    if version_key(version) is None:
        problems.append(f"clients refuse {version}: use X.Y.Z or X.Y.Z-alpha.N")
    if (declared := cargo_version(manifest)) != version:
        problems.append(f"Cargo.toml is {declared}, the tag is {version}")
    header = f"**Phux Rune {version}**"
    if not notes.startswith(header):
        problems.append(f"RELEASE_NOTES.md must open with {header}")
    return problems


def release_flags(version):
    if version_key(version) is None:
        raise ValueError(f"clients refuse {version}")
    if "-" in version:
        return ["--title", f"Phux Rune {version} (alpha)", "--prerelease"]
    return ["--title", f"Phux Rune {version}"]


def entry(dist, archive):
    path = os.path.join(dist, archive)
    with open(f"{path}.sha256", encoding="utf-8") as sidecar:
        digest, name = sidecar.read().split()
    if name.lstrip("*") != archive:
        sys.exit(f"{archive}.sha256 names {name}")
    return {"sha256": digest, "size": os.path.getsize(path)}


def build(version, sha, dist, source):
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
    return {
        "schema": 1,
        "version": version,
        "sha": sha,
        "protocol": f"{found[1]}.{found[2]}",
        "targets": targets,
    }


def read(path):
    with open(path, encoding="utf-8") as file:
        return file.read()


def main(command, *args):
    if command == "build":
        json.dump(build(*args), sys.stdout, indent=2)
        print()
    elif command == "newest":
        print(newest(sys.stdin.read().split()))
    elif command == "preflight":
        tag, manifest, notes = args
        problems = preflight(tag, read(manifest), read(notes))
        if problems:
            sys.exit("\n".join(problems))
    elif command == "release-flags":
        print("\n".join(release_flags(*args)))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    try:
        main(*sys.argv[1:])
    except (TypeError, ValueError) as error:
        sys.exit(str(error))
