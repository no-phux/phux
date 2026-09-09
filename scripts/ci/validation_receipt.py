#!/usr/bin/env python3
"""Reuse successful validation only for an identical Git tree and workflow.

Receipts contain metadata, never executable artifacts. API/network failure is a
cache miss. A main push without matching PR/main evidence runs normal validation.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import zipfile
from io import BytesIO


def git_tree():
    return subprocess.check_output(
        ["git", "rev-parse", "HEAD^{tree}"], text=True
    ).strip()


def identity(workflow):
    tree = git_tree()
    return {"schema": 1, "tree": tree, "workflow": workflow,
            "coverage": os.environ.get("VALIDATION_COVERAGE", "")}


def artifact_name(receipt):
    workflow = Path(receipt["workflow"]).stem
    return f"validation-{workflow}-{receipt['tree']}"


def api(path, binary=False):
    output = subprocess.check_output(["gh", "api", path], timeout=30)
    return output if binary else json.loads(output)


def trusted_run(run, repository, workflow):
    return (
        run.get("conclusion") == "success"
        and run.get("event") in {"pull_request", "push"}
        and run.get("path") == workflow
        and (run.get("head_repository") or {}).get("full_name") == repository
        and (run.get("repository") or {}).get("full_name") == repository
    )


def read_receipt(archive):
    with zipfile.ZipFile(BytesIO(archive)) as files:
        if files.namelist() != ["receipt.json"]:
            return None
        entry = files.getinfo("receipt.json")
        if entry.file_size > 4096:
            return None
        return json.loads(files.read(entry))


def matches(artifact, expected, repository, fetch=api):
    if artifact.get("expired") or artifact.get("size_in_bytes", 0) > 65536:
        return False
    run_id = artifact["workflow_run"]["id"]
    run = fetch(f"repos/{repository}/actions/runs/{run_id}")
    if not trusted_run(run, repository, expected["workflow"]):
        return False
    # Artifact JSON is not provenance. Bind it to the source run's immutable
    # commit through GitHub's Git database, including the workflow implementation
    # in that tree. For PRs this deliberately requires the head tree to equal
    # the tested merge tree; a stale PR base is a safe cache miss.
    commit = fetch(f"repos/{repository}/git/commits/{run['head_sha']}")
    if commit["tree"]["sha"] != expected["tree"]:
        return False
    archive = fetch(
        f"repos/{repository}/actions/artifacts/{artifact['id']}/zip", binary=True
    )
    return read_receipt(archive) == expected


def find_validation(expected, repository, fetch=api):
    name = artifact_name(expected)
    result = fetch(f"repos/{repository}/actions/artifacts?name={name}&per_page=100")
    for artifact in result["artifacts"]:
        if matches(artifact, expected, repository, fetch):
            return artifact["workflow_run"]["id"]
    return None


def lookup(expected, repository):
    try:
        return find_validation(expected, repository)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError, zipfile.BadZipFile) as error:
        print(f"Validation receipt unavailable; running checks: {error}")
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["check", "write"])
    parser.add_argument("workflow", help="Repository-relative workflow path")
    args = parser.parse_args()
    receipt = identity(args.workflow)
    output = {"name": artifact_name(receipt), "validated": "false"}
    if args.mode == "write":
        destination = Path("target/validation-receipt")
        destination.mkdir(parents=True, exist_ok=True)
        (destination / "receipt.json").write_text(json.dumps(receipt) + "\n")
    elif os.environ.get("GITHUB_EVENT_NAME") == "push":
        run = lookup(receipt, os.environ["GITHUB_REPOSITORY"])
        if run is not None:
            print(f"Identical tree already validated by workflow run {run}")
            output["validated"] = "true"
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf8") as stream:
        for key, value in output.items():
            stream.write(f"{key}={value}\n")


if __name__ == "__main__":
    main()
