#!/usr/bin/env python3
"""Dispatch agent-integration-release.yml and wait for it.

npm trusted publishing matches the entry workflow filename. Calling that
workflow with workflow_call makes the token say publish.yml, and npm
answers 404. A workflow_dispatch keeps the filename npm trusts.
"""

import json
import os
import subprocess
import sys
import time


WORKFLOW = "agent-integration-release.yml"


def run_ids(repo):
    raw = subprocess.check_output(
        [
            "gh", "run", "list", "--repo", repo, "--workflow", WORKFLOW,
            "--limit", "30", "--json", "databaseId,event",
        ],
        text=True,
    )
    return json.loads(raw)


def dispatch(repo, tag, list_runs=run_ids, sleep=time.sleep, clock=time.time):
    before = {run["databaseId"] for run in list_runs(repo)}
    subprocess.check_call(
        [
            "gh", "workflow", "run", WORKFLOW, "--repo", repo, "--ref", "main",
            "-f", f"tag={tag}", "-f", "dry_run=false",
        ]
    )
    deadline = clock() + 180
    while clock() < deadline:
        sleep(5)
        for run in list_runs(repo):
            if run["databaseId"] not in before and run["event"] == "workflow_dispatch":
                return run["databaseId"]
    raise RuntimeError(f"no {WORKFLOW} run appeared for {tag}")


def watch(repo, run_id):
    return subprocess.run(
        ["gh", "run", "watch", str(run_id), "--repo", repo, "--exit-status"],
    ).returncode


def main():
    repo = os.environ["GITHUB_REPOSITORY"]
    tags = json.loads(os.environ["TAGS"])
    failed = []
    for tag in tags:
        run_id = dispatch(repo, tag)
        print(f"watching {tag} in run {run_id}")
        if watch(repo, run_id) != 0:
            failed.append(f"{tag} (run {run_id})")
    if failed:
        raise SystemExit("integration publish failed: " + ", ".join(failed))


if __name__ == "__main__":
    main()
