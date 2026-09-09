#!/usr/bin/env python3
"""Resolve an Actions event to changed paths, then invoke the shared classifier.

Caller checks out the current event commit with fetch-depth: 0. No API token or
fetch is required. Missing/invalid history, empty diffs, schedule and dispatch
request every surface. Paths are NUL-delimited until passed to the classifier.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import sys

CLASSIFIER = Path(__file__).with_name("classify-changes.sh")


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


def changed_paths(event_name, event):
    base, head = event_range(event_name, event)
    # --no-renames retains both old and new paths across ownership boundaries.
    names = git("diff", "--no-renames", "--name-only", "-z", base, head, "--")
    paths = names.decode("utf-8").rstrip("\0").split("\0")
    if any("\n" in path or "\r" in path for path in paths):
        raise ValueError("path cannot be represented by the classifier CLI")
    return paths


def detect():
    try:
        event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
        if not isinstance(event, dict):
            raise ValueError("event payload must be an object")
        return changed_paths(os.environ.get("GITHUB_EVENT_NAME", ""), event)
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f"Full validation: {error}", file=sys.stderr)
        return []


if __name__ == "__main__":
    subprocess.run(["bash", str(CLASSIFIER)], input="\n".join(detect()), text=True, check=True)
