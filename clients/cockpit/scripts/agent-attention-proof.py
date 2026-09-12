#!/usr/bin/env python3
"""Interactive AgentSession fixture, launched INSIDE an isolated Phux terminal.

Only real stdin consumption advances the producer beyond its ask. The private
JSONL receipt contains resource identity and stamped metadata, not typed input.
Use the matching checkout's phux CLI; the native acceptance driver supplies
terminal input through Cockpit and inspects the resulting producer evidence.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


def command(args, *words):
    return subprocess.check_output(
        [str(args.phux), "--socket", args.socket, *words], text=True, timeout=20
    )


def receipt(output, value):
    output.write(json.dumps(value, sort_keys=True) + "\n")
    output.flush()


def emit(args, resource, kind, data):
    return json.loads(command(args, "agent", "emit", resource, "--type", kind,
                              "--data", json.dumps(data), "--json"))


def run(args, output):
    local_id = os.environ.get("PHUX_TERMINAL_ID", "")
    if not local_id.isdecimal() or not sys.stdin.isatty():
        raise RuntimeError("Run this fixture inside a Phux PTY with PHUX_TERMINAL_ID")
    parent = "@" + local_id
    opened = json.loads(command(args, "agent", "session", "open", parent,
                                "--provider", "cockpit-proof", "--native-id", args.run_id, "--json"))
    if opened["parent"] != parent:
        raise RuntimeError("Coordinator returned a different parent")
    resource = opened["resource"]
    receipt(output, {"phase": "opened", **opened})
    initial = emit(args, resource, "ask", {"question": "Preparing terminal intervention proof"})
    receipt(output, {"phase": "blocked-initial", **initial})
    print("Type inspect to publish a second blocked reason:", flush=True)
    if sys.stdin.readline().strip() != "inspect":
        raise RuntimeError("Expected inspect; proof agent retained for inspection")
    ask = emit(args, resource, "ask", {"question": "Enter a decimal proof value in this terminal"})
    receipt(output, {"phase": "blocked", **ask})
    print("Enter a decimal proof value in this terminal:", flush=True)
    answer = sys.stdin.readline().strip()
    if not answer.isascii() or not answer.isdecimal() or len(answer) > 12:
        raise RuntimeError("Expected a decimal proof value of at most twelve digits")
    # The result is neither a shell-command echo nor the entered answer.
    result = str(int(answer) + 451)
    emit(args, resource, "prompt", {"reason": "Terminal input consumed"})
    done = emit(args, resource, "stop", {"reason": "Terminal intervention completed"})
    print(result, flush=True)
    receipt(output, {"phase": "done", "computed_result": result, **done})
    print("Type close to retire this proof agent:", flush=True)
    if sys.stdin.readline().strip() != "close":
        raise RuntimeError("Expected close; proof agent retained for inspection")
    command(args, "agent", "session", "close", resource)
    receipt(output, {"phase": "closed", "resource": resource, "parent": parent})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--phux", type=Path, required=True)
    parser.add_argument("--socket", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    args = parser.parse_args()
    descriptor = os.open(args.receipt, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as output:
        run(args, output)


if __name__ == "__main__":
    main()
