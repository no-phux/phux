#!/usr/bin/env python3
"""Cut the next Rune release with no human: rune-auto-release.yml runs this.

    release.py          one step of the train, from a main checkout
    RUNE_RELEASE_TAG_ONLY=1 release.py   tag or reconcile, never open a new bump

Each run does at most one step. If Cargo.toml names a version with no tag
yet, it tags the commit that set it once that commit's push ci is green, and
rune-client.yml takes it from there. Otherwise, if feat/fix/perf commits
touched clients/rune since the last tag, it opens the next patch bump PR
with generated notes and marks it ready. Later runs enable auto-merge only
after pr-ready-ci authorizes its exact head. Anything else is a no-op.

A release SHA that is still not green after six hours fails the run once.
A later run that already finds that line in this workflow's failed logs
exits 0. The workflow's head SHA is main's tip and is not the marker: it
moves while the release SHA stays put.
"""

import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

from feed import TAG_PREFIX, VERSION, cargo_version

MANIFEST = "clients/rune/Cargo.toml"
LOCK = "clients/rune/Cargo.lock"
NOTES = "clients/rune/RELEASE_NOTES.md"
NEWS = "clients/rune/news.json"
# ponytail: the bell shows the newest notes only; older ones live in git.
NEWS_KEPT = 80
BRANCH = "rune-release/next"
# ponytail: only commits under clients/rune count, not the shared crates
# Rune builds from; route through tools/src/routing if that misses releases.
PATHS = ["clients/rune"]
STUCK_SECONDS = 6 * 3600
PRIOR_FAILURES = 20
WORKFLOW_FILE = "rune-auto-release.yml"
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


def news(feed, version, date, changes):
    """The bell's feed (news.json) with this release's changes on top."""
    added = [
        {"date": date, "title": re.sub(r" \(#\d+\)$", "", change), "body": f"In Rune {version}."}
        for change in changes
    ]
    return json.dumps((added + json.loads(feed))[:NEWS_KEPT], indent=2, ensure_ascii=False) + "\n"


def bump(manifest, lock, version):
    manifest = re.sub(r'^version = ".*"$', f'version = "{version}"', manifest, count=1, flags=re.M)
    lock, n = re.subn(r'(name = "phux-rune"\nversion = )".*"', rf'\1"{version}"', lock, count=1)
    if n != 1:
        raise ValueError("Cargo.lock has no phux-rune entry")
    return manifest, lock


def sh(*args, check=True):
    done = subprocess.run(args, check=check, capture_output=True, text=True)
    return done.stdout.strip() if check else done


def stuck_line(tag, sha, age):
    return f"{tag}: ci is not green on {sha} after {age // 3600:.0f}h; fix main, then rerun"


def already_reported(logs, sha):
    """True when a failed-step log already contains the stuck line for `sha`."""
    needle = f"ci is not green on {sha}"
    return any(needle in log for log in logs)


def stuck_exit(age, sha, logs):
    """Exit status once ci is not green. The six-hour comparison is unchanged.

    `logs is None` means the run list or its failed-step logs could not be
    read (including a 403). That still fails, so a permission error cannot
    hide the alert.
    """
    if age <= STUCK_SECONDS:
        return 0
    if logs is None or not already_reported(logs, sha):
        return 1
    return 0


def record_summary(text):
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if not path:
        return
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(text if text.endswith("\n") else text + "\n")


def prior_failure_logs(sha):
    """Failed-step logs of recent rune-auto-release runs, newest first.

    None when that list cannot be read, so the caller still alerts. The
    marker is the stuck line: a run's head SHA is main's tip, which changes
    while the release SHA in the line does not.
    """
    repo = os.environ.get("GITHUB_REPOSITORY", "")
    if not repo or not (os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")):
        return None
    listed = sh(
        "gh",
        "api",
        f"repos/{repo}/actions/workflows/{WORKFLOW_FILE}/runs?status=failure&per_page={PRIOR_FAILURES}",
        "--jq",
        ".workflow_runs[].id",
        check=False,
    )
    if listed.returncode != 0:
        print(listed.stderr.strip() or "could not list rune-auto-release runs")
        return None
    current = os.environ.get("GITHUB_RUN_ID", "")
    logs = []
    needle = f"ci is not green on {sha}"
    unreadable = False
    for run_id in listed.stdout.split():
        if run_id == current:
            continue
        viewed = sh("gh", "run", "view", run_id, "--repo", repo, "--log-failed", check=False)
        if viewed.returncode != 0:
            unreadable = True
            detail = (viewed.stderr or viewed.stdout or "").strip()
            if detail:
                print(detail)
            continue
        logs.append(viewed.stdout)
        if needle in viewed.stdout:
            break
    # A 403 (or any log-read failure) with no stuck line still alerts.
    if unreadable and not already_reported(logs, sha):
        print("could not read rune-auto-release failure logs; reporting this one")
        return None
    return logs


def tag_step(version):
    tag = TAG_PREFIX + version
    sha = sh("git", "log", "-1", "--format=%H", "-G", "^version = ", "--", MANIFEST)
    if sh("bun", "tools/main.ts", "ci-green", sha, check=False).returncode == 0:
        sh("git", "tag", tag, sha)
        sh("git", "push", "origin", f"refs/tags/{tag}")
        print(f"tagged {tag} at {sha}")
        return 0
    age = time.time() - int(sh("git", "show", "-s", "--format=%ct", sha))
    if age <= STUCK_SECONDS:
        print(f"{tag}: waiting for ci on {sha}")
        return 0
    line = stuck_line(tag, sha, age)
    logs = prior_failure_logs(sha)
    if logs is None:
        print("could not read earlier rune-auto-release failures; reporting this one")
    if stuck_exit(age, sha, logs) == 0:
        print(f"{line}; already reported")
        record_summary(f"{line}. Already reported for this SHA; not failing again.")
        return 0
    print(line)
    return 1


def open_bump_pr():
    """Find the same-repository bump; never arm a fork's matching branch."""
    command = (
        "gh", "pr", "list", "--base", "main", "--head", BRANCH, "--state", "open",
        "--json", "number,isDraft,headRefOid,isCrossRepository",
    )
    done = sh(*command, check=False)
    stdout = done.stdout.strip()
    if done.returncode != 0 or not stdout:
        message = f"{' '.join(command)} exited {done.returncode}; stderr: {done.stderr.strip()}"
        if stdout:
            message += f"; stdout: {stdout}"
        elif done.stdout:
            message += f"; stdout was whitespace: {done.stdout!r}"
        else:
            message += "; stdout was empty"
        raise RuntimeError(message)
    try:
        prs = json.loads(done.stdout)
    except json.JSONDecodeError as error:
        raise ValueError(f"gh pr list did not return JSON: {done.stdout}") from error
    if not isinstance(prs, list):
        raise ValueError("gh pr list did not return a JSON array")
    for pr in prs:
        if not isinstance(pr, dict) or type(pr.get("isCrossRepository")) is not bool:
            raise ValueError("gh pr list returned an unsupported repository identity")
        if not pr["isCrossRepository"]:
            return pr
    return None


def arm_bump_pr(pr):
    sh("gh", "pr", "ready", str(pr))
    print(f"the {BRANCH} PR is armed; waiting for genuine post-ready ci")
    return 0


def same_ready_head(pr, head):
    return pr["state"] == "OPEN" and not pr["isDraft"] and pr["headRefOid"] == head


def reconcile_bump_pr(pr):
    """Arm once, then consume the shared read-only guard before auto-merge."""
    number = str(pr["number"])
    if pr["isDraft"]:
        return arm_bump_pr(number)
    head = pr["headRefOid"]
    gate = sh("bun", "tools/main.ts", "pr-ready-ci", number, head, check=False)
    if gate.returncode != 0:
        print((gate.stdout + gate.stderr).strip())
        print(f"the {BRANCH} PR: guard did not authorize auto-merge; inspect pending or failed checks")
        return 0
    current = json.loads(sh("gh", "pr", "view", number, "--json", "state,isDraft,headRefOid"))
    if not same_ready_head(current, head):
        print(f"the {BRANCH} PR changed after the guard; leaving it alone")
        return 0
    sh("gh", "pr", "merge", number, "--auto", "--squash", "--match-head-commit", head)
    print(f"the {BRANCH} PR: exact-head post-ready ci passed; auto-merge enabled")
    return 0


def bump_step(version):
    pr = open_bump_pr()
    if pr is not None:
        return reconcile_bump_pr(pr)
    if os.environ.get("RUNE_RELEASE_TAG_ONLY"):
        return 0
    previous = TAG_PREFIX + version
    log = sh("git", "log", "--format=%s", f"{previous}..HEAD", "--", *PATHS)
    changes = shippable(log.splitlines())
    if not changes:
        print(f"nothing to ship since {previous}")
        return 0
    new = next_version(version)
    manifest, lock = bump(Path(MANIFEST).read_text(), Path(LOCK).read_text(), new)
    Path(MANIFEST).write_text(manifest)
    Path(LOCK).write_text(lock)
    Path(NOTES).write_text(notes(new, f"alpha.{version.rsplit('.', 1)[1]}" if "alpha" in version else version, changes))
    today = datetime.now(timezone.utc).date().isoformat()
    Path(NEWS).write_text(news(Path(NEWS).read_text(), new, today, changes))
    title = f"chore(rune): release {new}"
    sh("git", "switch", "-C", BRANCH)
    sh("git", "commit", "-qam", title)
    sh("git", "push", "-f", "origin", BRANCH)
    body = "Cut by rune-auto-release.yml. Merging tags it once ci is green.\n\n" + Path(NOTES).read_text()
    sh("gh", "pr", "create", "--draft", "--base", "main", "--head", BRANCH, "--title", title, "--body", body)
    print(f"opened {title}")
    return arm_bump_pr(BRANCH)


def main():
    version = cargo_version(Path(MANIFEST).read_text())
    if not sh("git", "tag", "-l", TAG_PREFIX + version):
        return tag_step(version)
    return bump_step(version)


if __name__ == "__main__":
    sys.exit(main())
