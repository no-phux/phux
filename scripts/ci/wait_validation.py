#!/usr/bin/env python3
"""Wait for exact-SHA main validation before automated release artifact fan-out."""

import argparse
import os
import time
from urllib.parse import urlencode

from validation_receipt import api


def validation_state(runs, sha, workflow):
    matching = [run for run in runs
                if run.get("head_sha") == sha and run.get("head_branch") == "main"
                and run.get("event") == "push" and run.get("path") == workflow]
    if not matching:
        return "pending"
    latest = max(matching, key=lambda run: (run["id"], run.get("run_attempt", 1)))
    if latest["status"] != "completed":
        return "pending"
    return latest["conclusion"]


def wait(repository, sha, workflow, timeout, fetch=api, clock=time.monotonic, sleep=time.sleep):
    deadline = clock() + timeout
    query = urlencode({"head_sha": sha, "event": "push", "per_page": 100})
    while clock() < deadline:
        result = fetch(f"repos/{repository}/actions/workflows/{workflow.rsplit('/', 1)[-1]}/runs?{query}")
        state = validation_state(result["workflow_runs"], sha, workflow)
        if state == "success":
            print(f"{workflow} passed for {sha}")
            return
        if state != "pending":
            raise RuntimeError(f"{workflow} finished {state} for {sha}; release remains draft")
        sleep(15)
    raise TimeoutError(f"No successful {workflow} for {sha} within {timeout}s; release remains draft")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("workflow")
    parser.add_argument("sha", help="Resolved release tag commit, not the triggering push")
    parser.add_argument("--timeout", type=int, default=2400)
    args = parser.parse_args()
    wait(os.environ["GITHUB_REPOSITORY"], args.sha, args.workflow, args.timeout)


if __name__ == "__main__":
    main()
