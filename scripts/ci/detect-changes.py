#!/usr/bin/env python3
"""Resolve an Actions event to changed paths, then invoke the shared classifier.

Caller checks out the current event commit with fetch-depth: 0. No API token or
fetch is required. Missing/invalid history, empty diffs, schedule and dispatch
request every surface. A release-please version-field diff does not: those
files are metadata, and compiling them is the 18-minute macOS job. Paths are
NUL-delimited until passed to the classifier.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import sys

CLASSIFIER = Path(__file__).with_name("classify-changes.sh")
sys.path.insert(0, str(Path(__file__).resolve().parent))
import release_metadata  # noqa: E402


def git(*args):
    return subprocess.check_output(["git", *args], stderr=subprocess.PIPE)


def commit(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-fA-F]{40,64}", value):
        raise ValueError("missing or malformed event commit")
    git("cat-file", "-e", value + "^{commit}")
    return value


def event_range(event_name, event):
    if event_name == "push":
        return commit(event.get("before")), commit(event.get("after"))
    if event_name == "pull_request":
        pr = event["pull_request"]
        base, head = commit(pr["base"]["sha"]), commit(pr["head"]["sha"])
        return git("merge-base", base, head).decode().strip(), head
    if event_name == "merge_group":
        group = event["merge_group"]
        return commit(group["base_sha"]), commit(group["head_sha"])
    raise ValueError("event requests full validation")


def diff_paths(base, head):
    # --no-renames retains both old and new paths across ownership boundaries.
    names = git("diff", "--no-renames", "--name-only", "-z", base, head, "--")
    paths = [path for path in names.decode("utf-8").rstrip("\0").split("\0") if path]
    if any("\n" in path or "\r" in path for path in paths):
        raise ValueError("path cannot be represented by the classifier CLI")
    return paths


def show(rev, path):
    try:
        return git("show", f"{rev}:{path}").decode("utf-8")
    except (subprocess.CalledProcessError, UnicodeDecodeError):
        return None


def detect():
    """Return paths, \"skip\" for release metadata, or None for full validation."""
    try:
        event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
        if not isinstance(event, dict):
            raise ValueError("event payload must be an object")
        base, head = event_range(os.environ.get("GITHUB_EVENT_NAME", ""), event)
        paths = diff_paths(base, head)
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f"Full validation: {error}", file=sys.stderr)
        return None
    if release_metadata.only(paths, lambda path: (show(base, path), show(head, path))):
        print("Release metadata only; compile lanes skipped", file=sys.stderr)
        return "skip"
    return paths


if __name__ == "__main__":
    detected = detect()
    if detected == "skip":
        print("\n".join(release_metadata.skip_outputs()))
    else:
        subprocess.run(
            ["bash", str(CLASSIFIER)],
            input="\n".join(detected or []),
            text=True,
            check=True,
        )
