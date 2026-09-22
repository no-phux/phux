#!/usr/bin/env python3
"""Choose which draft releases to ship, and which stale drafts to drop.

publish.yml is the only workflow that flips a draft public. Release Please
has already cut the tag and the notes. This plan runs when that happens and
again when ci.yml finishes, plus on a schedule, so the two events can arrive
in either order and a missed event still converges.

Nothing here builds artifacts. It only decides.
"""

import json
import os
import re
import subprocess
import sys
from urllib.parse import urlencode

from validation_receipt import api


def validation_state(runs, sha, workflow):
    """Conclusion of the newest push run of `workflow` on main for `sha`.

    Anything else — a pull request, a different commit, a run still going —
    is `pending`, so a draft is never shipped on the wrong evidence.
    """
    matching = [
        run for run in runs
        if run.get("head_sha") == sha and run.get("head_branch") == "main"
        and run.get("event") == "push" and run.get("path") == workflow
    ]
    if not matching:
        return "pending"
    latest = max(matching, key=lambda run: (run["id"], run.get("run_attempt", 1)))
    if latest["status"] != "completed":
        return "pending"
    return latest["conclusion"]


TAG_RE = re.compile(
    r"^(?:(?P<component>[a-z0-9]+(?:-[a-z0-9]+)*)-)?"
    r"v(?P<major>0|[1-9]\d*)\.(?P<minor>0|[1-9]\d*)\.(?P<patch>0|[1-9]\d*)$"
)
CI_WORKFLOW = ".github/workflows/ci.yml"
PENDING = {"pending", "queued", "in_progress", "waiting", "requested", "missing"}


def parse_tag(tag):
    match = TAG_RE.fullmatch(tag)
    if match is None:
        return None
    return {
        "component": match.group("component") or "phux",
        "version": tuple(int(match.group(name)) for name in ("major", "minor", "patch")),
    }


def lane(component):
    if component == "phux":
        return "phux"
    if component == "cockpit":
        return "cockpit"
    return "integration"


def _partition(releases):
    """Return (delete_tags, candidates) from release rows that include a parsed tag."""
    by_component = {}
    for release in releases:
        by_component.setdefault(release["component"], []).append(release)

    delete = []
    candidates = []
    for rows in by_component.values():
        published = [row["version"] for row in rows if not row["draft"]]
        latest = max(published) if published else None
        drafts = [row for row in rows if row["draft"]]
        for row in drafts:
            if latest is not None and row["version"] < latest:
                delete.append(row["tag"])
        ahead = [row for row in drafts if latest is None or row["version"] > latest]
        if ahead:
            candidates.append(max(ahead, key=lambda row: row["version"]))
    return delete, candidates


def decide(releases, ci_state, *, event_sha=None, dispatch_tag=None):
    """Return publish tags, draft tags to delete, and whether to wait or fail.

    `releases` rows are `{tag, draft, sha}`. `ci_state(sha)` is a conclusion
    string, or `pending` when the ci run has not finished. Tags that are not
    component versions (the moving `next` prerelease, for one) are ignored.
    """
    parsed = []
    for release in releases:
        identity = parse_tag(release["tag"])
        if identity is None:
            continue
        parsed.append({**release, **identity})

    delete, candidates = _partition(parsed)
    result = {"publish": [], "delete": delete, "wait": False, "fail": None, "blocked": ""}

    if dispatch_tag:
        return _decide_dispatch(parsed, candidates, ci_state, dispatch_tag, result)
    if event_sha:
        return _decide_event(candidates, ci_state, event_sha, result)
    return _decide_reconcile(candidates, ci_state, result)


def _decide_dispatch(parsed, candidates, ci_state, tag, result):
    selected = next((row for row in parsed if row["tag"] == tag), None)
    if selected is None:
        result["fail"] = f"no release named {tag}"
        return result
    if not selected["draft"]:
        return result
    if selected["tag"] not in {row["tag"] for row in candidates}:
        result["fail"] = f"{tag} is older than the published {selected['component']} release"
        return result
    state = ci_state(selected["sha"])
    if state != "success":
        result["fail"] = f"ci.yml is {state} for {selected['sha']}; {tag} stays a draft"
        return result
    result["publish"] = [selected["tag"]]
    return result


def _decide_event(candidates, ci_state, sha, result):
    mine = [row for row in candidates if row["sha"] == sha]
    if not mine:
        return result
    state = ci_state(sha)
    if state in PENDING:
        result["wait"] = True
        return result
    if state != "success":
        tags = ", ".join(row["tag"] for row in mine)
        result["fail"] = f"ci.yml is {state} for {sha}; not publishing {tags}"
        return result
    result["publish"] = [row["tag"] for row in mine]
    return result


def _decide_reconcile(candidates, ci_state, result):
    blocked = []
    for row in candidates:
        state = ci_state(row["sha"])
        if state == "success":
            result["publish"].append(row["tag"])
        elif state in PENDING:
            continue
        else:
            blocked.append(f"{row['tag']} (ci.yml {state} for {row['sha']})")
    if blocked:
        result["blocked"] = "still draft, ci is not green: " + "; ".join(blocked)
    return result


def split_lanes(tags):
    lanes = {"phux": "", "cockpit": "", "integration": []}
    for tag in tags:
        identity = parse_tag(tag)
        if identity is None:
            continue
        destination = lane(identity["component"])
        if destination == "integration":
            lanes["integration"].append(tag)
        else:
            lanes[destination] = tag
    return lanes


def ci_conclusion(repository, sha, fetch=api):
    query = urlencode({"head_sha": sha, "event": "push", "per_page": 20})
    payload = fetch(f"repos/{repository}/actions/workflows/ci.yml/runs?{query}")
    return validation_state(payload.get("workflow_runs", []), sha, CI_WORKFLOW)


def _releases_from_api(repository):
    raw = subprocess.check_output(
        [
            "gh", "api", f"repos/{repository}/releases", "--paginate",
            "--jq", ".[] | {tag:.tag_name,draft:.draft}",
        ],
        text=True,
    )
    rows = []
    for line in raw.splitlines():
        if line.strip():
            rows.append(json.loads(line))
    return rows


def _tag_sha(tag):
    return subprocess.check_output(
        ["git", "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}"],
        text=True,
    ).strip()


def _delete_drafts(repository, tags):
    for tag in tags:
        subprocess.check_call(
            ["gh", "release", "delete", tag, "--repo", repository, "--yes"],
        )
        print(f"deleted superseded draft {tag}")


def _write_outputs(path, lanes, blocked):
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(f"phux_tag={lanes['phux']}\n")
        handle.write(f"cockpit_tag={lanes['cockpit']}\n")
        handle.write(f"integration_tags={json.dumps(lanes['integration'])}\n")
        handle.write(f"blocked={blocked}\n")


def main():
    repository = os.environ["GITHUB_REPOSITORY"]
    event = os.environ.get("EVENT_NAME", "")
    dispatch = os.environ.get("DISPATCH_TAG", "").strip()
    sha = os.environ.get("EVENT_SHA", "").strip()
    event_sha = sha if event == "workflow_run" and sha and not dispatch else None

    rows = []
    for release in _releases_from_api(repository):
        if parse_tag(release["tag"]) is None:
            continue
        rows.append({**release, "sha": _tag_sha(release["tag"])})

    plan = decide(
        rows,
        lambda commit: ci_conclusion(repository, commit),
        event_sha=event_sha,
        dispatch_tag=dispatch or None,
    )
    if plan["delete"]:
        _delete_drafts(repository, plan["delete"])

    if plan["fail"]:
        print(f"::error::{plan['fail']}", file=sys.stderr)
        return 1

    lanes = split_lanes(plan["publish"])
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        _write_outputs(output, lanes, plan["blocked"])
    if plan["wait"]:
        print(f"ci.yml still running for {event_sha}; publish will retry when it finishes")
    elif plan["publish"]:
        print("publishing " + ", ".join(plan["publish"]))
    else:
        print("nothing to publish")
    if plan["blocked"]:
        print(f"::warning::{plan['blocked']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
