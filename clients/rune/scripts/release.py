#!/usr/bin/env python3
"""Cut the next Rune release with no human: rune-auto-release.yml runs this.

    release.py          one step of the train, from a main checkout
    RUNE_RELEASE_TAG_ONLY=1 release.py   only ever the tag step

Each run does at most one step. If Cargo.toml names a version with no tag
yet, it tags the commit that set it once that commit's push ci is green, and
rune-client.yml takes it from there. Otherwise, if feat/fix/perf commits
touched clients/rune since the last tag, it opens the next patch bump PR
with generated notes and turns on auto-merge. Anything else is a no-op.
"""

import os
import re
import subprocess
import sys
import time
from pathlib import Path

from feed import TAG_PREFIX, VERSION, cargo_version

MANIFEST = "clients/rune/Cargo.toml"
LOCK = "clients/rune/Cargo.lock"
NOTES = "clients/rune/RELEASE_NOTES.md"
BRANCH = "rune-release/next"
# ponytail: only commits under clients/rune count, not the shared crates
# Rune builds from; route through tools/src/routing if that misses releases.
PATHS = ["clients/rune"]
STUCK_SECONDS = 6 * 3600
SHIPPABLE = re.compile(r"^(feat|fix|perf)(\([^)]*\))?!?: (.+)$")


def next_version(version):
    found = VERSION.fullmatch(version)
    if not found:
        raise ValueError(f"clients can't read {version}")
    major, minor, patch, alpha = found.groups()
    if alpha is not None:
        return f"{major}.{minor}.{patch}-alpha.{int(alpha) + 1}"
    return f"{major}.{minor}.{int(patch) + 1}"


def shippable(subjects):
    """What users would notice, as note lines: feat, fix and perf only."""
    out = []
    for subject in subjects:
        if found := SHIPPABLE.match(subject):
            text = found[3]
            out.append(text[:1].upper() + text[1:])
    return out


def notes(version, previous, changes):
    lines = "\n".join(f"- {change}" for change in changes)
    count = f"{len(changes)} change{'s' if len(changes) != 1 else ''}"
    return f"**Phux Rune {version}**: {count} since {previous}.\n\n{lines}\n"


def bump(manifest, lock, version):
    manifest = re.sub(r'^version = ".*"$', f'version = "{version}"', manifest, count=1, flags=re.M)
    lock, n = re.subn(r'(name = "phux-rune"\nversion = )".*"', rf'\1"{version}"', lock, count=1)
    if n != 1:
        raise ValueError("Cargo.lock has no phux-rune entry")
    return manifest, lock


def sh(*args, check=True):
    done = subprocess.run(args, check=check, capture_output=True, text=True)
    return done.stdout.strip() if check else done


def tag_step(version):
    tag = TAG_PREFIX + version
    sha = sh("git", "log", "-1", "--format=%H", "-G", "^version = ", "--", MANIFEST)
    if sh("bun", "tools/main.ts", "ci-green", sha, check=False).returncode == 0:
        sh("git", "tag", tag, sha)
        sh("git", "push", "origin", f"refs/tags/{tag}")
        print(f"tagged {tag} at {sha}")
        return 0
    age = time.time() - int(sh("git", "show", "-s", "--format=%ct", sha))
    if age > STUCK_SECONDS:
        print(f"{tag}: ci is not green on {sha} after {age // 3600:.0f}h; fix main, then rerun")
        return 1
    print(f"{tag}: waiting for ci on {sha}")
    return 0


def bump_step(version):
    previous = TAG_PREFIX + version
    log = sh("git", "log", "--format=%s", f"{previous}..HEAD", "--", *PATHS)
    changes = shippable(log.splitlines())
    if not changes:
        print(f"nothing to ship since {previous}")
        return 0
    if sh("gh", "pr", "list", "--head", BRANCH, "--state", "open", "--json", "number", "-q", "length") != "0":
        print(f"the {BRANCH} PR is open; auto-merge lands it")
        return 0
    new = next_version(version)
    manifest, lock = bump(Path(MANIFEST).read_text(), Path(LOCK).read_text(), new)
    Path(MANIFEST).write_text(manifest)
    Path(LOCK).write_text(lock)
    Path(NOTES).write_text(notes(new, f"alpha.{version.rsplit('.', 1)[1]}" if "alpha" in version else version, changes))
    title = f"chore(rune): release {new}"
    sh("git", "switch", "-C", BRANCH)
    sh("git", "commit", "-qam", title)
    sh("git", "push", "-f", "origin", BRANCH)
    body = "Cut by rune-auto-release.yml. Merging tags it once ci is green.\n\n" + Path(NOTES).read_text()
    sh("gh", "pr", "create", "--base", "main", "--head", BRANCH, "--title", title, "--body", body)
    sh("gh", "pr", "merge", BRANCH, "--auto", "--squash")
    print(f"opened {title}")
    return 0


def main():
    version = cargo_version(Path(MANIFEST).read_text())
    if not sh("git", "tag", "-l", TAG_PREFIX + version):
        return tag_step(version)
    if os.environ.get("RUNE_RELEASE_TAG_ONLY"):
        return 0
    return bump_step(version)


if __name__ == "__main__":
    sys.exit(main())
