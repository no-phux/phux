#!/usr/bin/env python3
"""A local, out-of-process Git worktree policy composed with phux's public API."""

import argparse
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import uuid


class Fault(Exception):
    def __init__(self, code, message, outcome="unknown", key=None):
        self.document = {"code": code, "message": message, "outcome": outcome}
        self.key = key
        super().__init__(message)


OPS = {"terminal.create", "terminal.operation", "resource.show"}


class Phux:
    def __init__(self, args):
        self.command = [args.phux, "--socket", str(args.socket.absolute())]
        self.legacy = args.legacy_cli

    def json(self, *args, request=None):
        try:
            result = subprocess.run(
                [*self.command, *args], input=request or "", text=True,
                capture_output=True, timeout=20, check=False,
            )
            value = json.loads(result.stderr if result.returncode else result.stdout)
        except (OSError, ValueError, subprocess.TimeoutExpired) as error:
            raise Fault("unreadable_reply", str(error)) from error
        if result.returncode:
            error = value.get("error", {}) if isinstance(value, dict) else {}
            raise Fault(error.get("code", "unreadable_reply"),
                        error.get("message", "phux returned an unrecognized error"),
                        error.get("outcome", "unknown"), error.get("idempotency_key"))
        return value

    def authority(self):
        catalog = self.json(*(("terminal", "describe") if self.legacy else ("api", "list")))
        supported = {op["name"] for op in catalog["operations"] if op.get("supported")}
        if catalog["source"] != "connected_server" or not OPS <= supported:
            raise Fault("unsupported", "the connected server lacks required public operations")
        authority = durable_key(catalog["authority"])
        if not self.legacy:
            self.require_method(authority, "terminal.create")
        return authority

    def require_method(self, authority, name):
        method = self.json("api", "describe", name)
        if method["authority"] != authority or method["operation"]["name"] != name:
            raise Fault("authority_mismatch", "catalog and operation description disagree")

    def call(self, method, request):
        if not self.legacy:
            return self.json("api", "call", method, "--request-json", "-",
                             request=json.dumps(request))
        # Explicit compatibility mode for independently running against pre-api CLI.
        if method == "terminal.create":
            return self.json("terminal", "create", "--request-json", json.dumps(request))
        if method == "terminal.operation":
            return self.json("terminal", "operation", "--session", request["session"],
                             "--idempotency-key", request["idempotency_key"])
        return self.json("resource", "show", "--json", "^" + request)


def durable_key(value):
    if not isinstance(value, str) or not re.fullmatch("[a-z2-7]{26}", value):
        raise Fault("invalid_key", "expected a full lowercase durable key")
    return value


def git(cwd, *args):
    try:
        return subprocess.run(
            ["git", "-C", str(cwd), *args], stdin=subprocess.DEVNULL, capture_output=True,
            text=True, check=True, timeout=60,
        ).stdout.rstrip("\n")
    except subprocess.CalledProcessError as error:
        raise Fault("git_failed", error.stderr.strip()) from error


def save(path, value):
    # Durable and atomic before crossing from Git policy into a phux mutation.
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as output:
        temporary = Path(output.name)
        try:
            json.dump(value, output, indent=2)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
            os.replace(temporary, path)
            directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            temporary.unlink(missing_ok=True)


def read(path):
    with path.open() as source:
        value = json.load(source)
    if value["schema_version"] != 1:
        raise Fault("unsupported_journal", "unrecognized launcher journal revision")
    return value


@contextmanager
def journal_lock(path):
    # Lock a stable sidecar since the journal itself is atomically replaced.
    with path.with_name(path.name + ".lock").open("a") as file:
        fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield


def placement(authority, session, anchor_path):
    if anchor_path is None:
        return {"type": "new_group"}
    anchor = read(anchor_path)
    receipt = anchor.get("receipt")
    if (anchor["authority"], anchor["request"]["session"]) != (authority, session):
        raise Fault("wrong_anchor", "anchor journal is for a different authority or session")
    if not receipt or not receipt["execution"]:
        raise Fault("no_anchor", "anchor journal lacks a saved placement and execution")
    return {
        "type": "beside", "entry": receipt["placement"]["entry"], "resource": receipt["resource"],
        **receipt["execution"], "axis": "horizontal", "ratio": 0.5,
    }


def fresh(path, authority, checkout, request):
    if path.exists():
        raise Fault("journal_exists", "use recover with this journal; launch never resends")
    record = {"schema_version": 1, "authority": authority, "git": checkout, "request": request}
    save(path, record)
    return record


def same_authority(phux, record):
    if phux.authority() != record["authority"]:
        raise Fault("authority_mismatch", "connect to the authority saved in this journal")


def execution_fence(document):
    observed = document["execution"]
    if observed is None:
        return None
    return {field: observed[field] for field in ("activation", "instance")}


def remember(path, record, reply):
    request = record["request"]
    if reply["schema_version"] != 1 or (
        reply["authority"], reply["session"], reply["idempotency_key"]
    ) != (record["authority"], request["session"], request["idempotency_key"]):
        raise Fault("receipt_mismatch", "cannot associate receipt with the journal")
    created = reply["placement"]
    receipt = {
        "resource": durable_key(reply["resource"]),
        "placement": {"arrangement": durable_key(created["arrangement"]), "entry": created["entry"]},
        "execution": execution_fence(reply),
    }
    previous = record.get("receipt")
    if previous and (previous["resource"], previous["placement"]) != (
        receipt["resource"], receipt["placement"]
    ):
        raise Fault("receipt_mismatch", "receipt now names a different resource or placement")
    if not previous or (previous["execution"] is None and receipt["execution"] is not None):
        record["receipt"] = receipt
        record.pop("error", None)
        save(path, record)


def report(phux, record):
    receipt = record["receipt"]
    try:
        current = phux.call("resource.show", receipt["resource"])
    except Fault as error:
        raise Fault(error.document["code"],
                    "intent committed, but resource observation failed: " + str(error),
                    "committed") from error
    if not isinstance(current, dict) or (
        current.get("schema_version"), current.get("authority"), current.get("resource")
    ) != (2, record["authority"], receipt["resource"]):
        raise Fault("resource_mismatch", "cannot associate observation with the saved resource",
                    "committed")
    return {
        "status": "intent_committed",
        "resource": receipt["resource"],
        "placement": receipt["placement"],
        "current_session": current["session"],
        "current_execution": current["execution"],
        "original_execution_matches": (
            current["session"] == record["request"]["session"]
            and receipt["execution"] is not None
            and execution_fence(current) == receipt["execution"]
        ),
    }


def place(phux, path, record):
    try:
        reply = phux.call("terminal.create", record["request"])
        remember(path, record, reply)
    except Fault as error:
        # Only a correlated typed refusal will ever qualify for a new intent.
        record["error"] = dict(error.document)
        if error.key != record["request"]["idempotency_key"]:
            record["error"]["outcome"] = "unknown"
        save(path, record)
        raise
    return report(phux, record)


def launch(phux, path, args):
    if path.exists():
        raise Fault("journal_exists", "use recover with this journal; launch never resends")
    if not args.argv:
        raise Fault("missing_argv", "supply a program after --")
    authority = phux.authority()
    session = durable_key(args.session)
    if args.worktree.exists() or args.worktree.is_symlink():
        raise Fault("worktree_exists", "choose an unused worktree path; no force or reuse")
    repo, worktree = args.repo.resolve(strict=True), args.worktree.resolve()
    common = git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir")
    base = git(repo, "rev-parse", "--verify", "--end-of-options", args.base + "^{commit}")
    if git(repo, "check-ref-format", "--branch", args.branch) != args.branch:
        raise Fault("invalid_branch", "use a literal new branch name")
    request = {
        "session": session, "idempotency_key": uuid.uuid4().hex, "argv": args.argv,
        "cwd": str(worktree), "placement": placement(authority, session, args.beside),
    }
    checkout = {"repo": str(repo), "common": common, "branch": args.branch, "base": base}
    record = fresh(path, authority, checkout, request)
    git(repo, "worktree", "add", "-b", args.branch, "--", str(worktree), base)
    return place(phux, path, record)


def recover(phux, path):
    record = read(path)
    same_authority(phux, record)
    if record.get("receipt"):
        # A saved durable resource key remains useful after a receipt is pruned.
        return report(phux, record)
    request = record["request"]
    reply = phux.call("terminal.operation", {
        "session": request["session"], "idempotency_key": request["idempotency_key"],
    })
    remember(path, record, reply)
    return report(phux, record)


def require_checkout(checkout, cwd):
    common = git(cwd, "rev-parse", "--path-format=absolute", "--git-common-dir")
    branch = git(cwd, "symbolic-ref", "HEAD")
    if (common, branch) != (checkout["common"], "refs/heads/" + checkout["branch"]):
        raise Fault("worktree_changed", "checkout no longer matches the launcher journal")


def replan(phux, path):
    record = read(path)
    same_authority(phux, record)
    old = record["request"]
    error = record.get("error", {})
    if (error.get("code"), error.get("outcome")) != ("stale", "refused"):
        raise Fault("replan_refused", "requires a definite stale placement refusal")
    checkout = record["git"]
    require_checkout(checkout, old["cwd"])
    anchor = old["placement"]
    observed = phux.call("resource.show", anchor["resource"])
    if (observed["authority"], observed["resource"], observed["session"]) != (
        record["authority"], anchor["resource"], old["session"]
    ):
        raise Fault("wrong_anchor", "the anchor no longer belongs to this authority and session")
    current = execution_fence(observed)
    if not current or current == {field: anchor[field] for field in current}:
        raise Fault("no_new_anchor_execution", "no replacement execution was observed")
    if observed["execution"]["state"] not in ("starting", "running", "stopping"):
        raise Fault("no_live_anchor", "the replacement anchor is not live")
    # Keep the entry identity. phux atomically revalidates it against the refreshed
    # execution; a move/replacement of the entry is not repaired speculatively.
    request = dict(old, placement={**anchor, **current}, idempotency_key=uuid.uuid4().hex)
    next_path = path.with_name(path.name + ".replan")
    with journal_lock(next_path):
        new = fresh(next_path, record["authority"], checkout, request)
        return place(phux, next_path, new)


def parse():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", type=Path, required=True, help="explicit local authority route")
    parser.add_argument("--phux", default="phux", help="path to a phux executable")
    parser.add_argument("--legacy-cli", action="store_true", help="opt into pre-api typed commands")
    actions = parser.add_subparsers(dest="action", required=True)
    start = actions.add_parser("launch", help="create a new Git worktree and place a terminal")
    start.add_argument("journal", type=Path)
    start.add_argument("--repo", type=Path, required=True)
    start.add_argument("--worktree", type=Path, required=True)
    start.add_argument("--base", required=True, help="existing local Git revision; never fetches")
    start.add_argument("--branch", required=True, help="new branch; never forces an existing branch")
    start.add_argument("--session", required=True, help="full durable phux session key")
    start.add_argument("--beside", type=Path, help="launcher journal of the selected anchor")
    actions.add_parser("recover", help="read without replaying").add_argument("journal", type=Path)
    actions.add_parser("replan-stale", help="refresh a definitely refused anchor once").add_argument(
        "journal", type=Path
    )
    # Split explicitly so option-like program arguments never reach our parser.
    argv = sys.argv[1:]
    boundary = argv.index("--") if "--" in argv else len(argv)
    args = parser.parse_args(argv[:boundary])
    args.argv = argv[boundary + 1:]
    if args.action != "launch" and args.argv:
        parser.error("only launch accepts a program after --")
    return args


def main():
    args = parse()
    path = args.journal.absolute()
    phux = Phux(args)
    try:
        with journal_lock(path):
            if args.action == "launch":
                report_value = launch(phux, path, args)
            else:
                handler = {"recover": recover, "replan-stale": replan}[args.action]
                report_value = handler(phux, path)
    except (OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"error": Fault("launcher_error", str(error)).document}), file=sys.stderr)
        return 1
    except Fault as error:
        print(json.dumps({"error": error.document}), file=sys.stderr)
        return 1
    print(json.dumps(report_value))
    return 0


if __name__ == "__main__":
    sys.exit(main())
