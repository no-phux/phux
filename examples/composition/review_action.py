#!/usr/bin/env python3
"""An application-owned review projection for the exact phux action target."""

import argparse
import json
import os
from pathlib import Path
import sys

from worktree_launcher import Fault, Phux, execution_fence, git, read, require_checkout


def verify_target(phux, record, selected):
    receipt = record["receipt"]
    # A CLI-supplied target is data, not authorization, and UI selections can
    # become stale after capture. Both must match current server observations.
    expected = {"authority": record["authority"], "session": record["request"]["session"],
                "resource": receipt["resource"], **receipt["execution"]}
    if selected.get("satellite") or not matches(selected, expected):
        raise Fault("wrong_target", "review requires the exact local launcher execution")
    current = phux.call("resource.show", expected["resource"])
    observed = {**current, **(execution_fence(current) or {})}
    if current["schema_version"] != 2 or not matches(observed, expected):
        raise Fault("stale", "this is no longer the selected execution in the saved session")
    return expected


def matches(target, expected):
    return all(target.get(key) == value for key, value in expected.items())


def review(args):
    context = json.loads(os.environ["PHUX_ACTION_CONTEXT"])
    if context["schema_version"] != 1 or context["executor"] != "invoking_machine":
        raise Fault("unsupported", "unsupported action context")
    record = read(args.journal)
    expected = verify_target(Phux(args), record, context["target"])
    checkout, cwd = record["git"], record["request"]["cwd"]
    require_checkout(checkout, cwd)
    return {
        "target": expected,
        "invocation_source": context["source"],
        "selection_observed": context["observed"],
        "branch": checkout["branch"],
        "tracked_diffstat": git(cwd, "diff", "--no-ext-diff", "--no-textconv", "--stat",
                               checkout["base"], "--"),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", type=Path, required=True)
    parser.add_argument("--phux", default="phux")
    parser.add_argument("--legacy-cli", action="store_true")
    parser.add_argument("journal", type=Path, help="trusted app-owned launcher journal")
    args = parser.parse_args()
    try:
        print(json.dumps(review(args)))
    except (Fault, OSError, ValueError, KeyError, TypeError) as error:
        problem = error.document if isinstance(error, Fault) else {"message": str(error)}
        print(json.dumps({"error": problem}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
