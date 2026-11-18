#!/usr/bin/env python3
"""Read a roster of exact phux question subjects and relay an explicit human answer."""

import argparse
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys

from worktree_launcher import Fault, durable_key


def target(authority, resource):
    return f"{durable_key(authority)}/^{durable_key(resource)}"


def invoke(args, *command):
    result = subprocess.run(
        [args.phux, "--socket", str(args.socket.absolute()), "agent", *command],
        stdin=subprocess.DEVNULL, text=True, capture_output=True, timeout=20,
    )
    try:
        document = json.loads(result.stdout)
    except ValueError as error:
        raise Fault("unreadable_reply", "no receipted reply from phux; do not resend") from error
    if document["schema_version"] != 1:
        raise Fault("unsupported", "unrecognized question document revision")
    return document


def current(args, exact):
    authority, resource = exact.split("/^")
    document = invoke(args, "questions", target(authority, resource))
    if (document["authority"], document["resource"]) != (authority, resource):
        raise Fault("wrong_subject", "question response did not name the requested subject")
    return document


def actionable(question):
    return (
        question["type"] == "provider"
        and question["response_supported"]
        and not question["response_reserved"]
    )


def answer(args):
    # The caller explicitly identifies the item and response. Never pick from text,
    # suggestions or an item position that might have changed since display.
    reference = json.loads(args.reference.read_text())
    exact = target(reference["authority"], reference["conversation"])
    if not any(q.get("reference") == reference and actionable(q)
               for q in current(args, exact)["questions"]):
        raise Fault("stale", "the displayed question is no longer actionable", "not_sent")
    operation_id = str(secrets.randbelow(2**64 - 1) + 1)
    request = {"operation_id": operation_id, "question": reference,
               "response": json.loads(args.response_json)}
    # Exclusive creation prevents a second transmission if a caller reuses the
    # same receipt path, even after a timeout. Store no answer body.
    fd = os.open(args.journal, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as file:
        json.dump({"operation_id": operation_id, "question": reference}, file)
        file.flush()
        os.fsync(file.fileno())
    directory = os.open(args.journal.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)
    return correlate(invoke(args, "respond-question", "--request-json",
                            json.dumps(request)), operation_id)


def correlate(receipt, operation_id):
    if receipt["operation_id"] != operation_id:
        raise Fault("receipt_mismatch", "could not correlate question receipt")
    return receipt


def recover(args):
    record = json.loads(args.journal.read_text())
    operation_id = record["operation_id"]
    return correlate(invoke(args, "question-receipt", "--question-json",
                            json.dumps(record["question"]), "--operation-id", operation_id),
                     operation_id)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", type=Path, required=True)
    parser.add_argument("--phux", default="phux")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("list").add_argument("subject", nargs="+", metavar="AUTHORITY/^RESOURCE")
    reply = commands.add_parser("answer")
    reply.add_argument("journal", type=Path)
    reply.add_argument("--reference", type=Path, required=True)
    reply.add_argument("--response-json", required=True)
    commands.add_parser("recover").add_argument("journal", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "list":
            result = [current(args, exact) for exact in args.subject]
        else:
            result = {"answer": answer, "recover": recover}[args.command](args)
    except (Fault, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        problem = error.document if isinstance(error, Fault) else {"message": str(error)}
        print(json.dumps({"error": problem}), file=sys.stderr)
        return 1
    print(json.dumps(result))
    return int(isinstance(result, dict) and result["disposition"] in ("unknown", "refused"))


if __name__ == "__main__":
    sys.exit(main())
