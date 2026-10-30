#!/usr/bin/env python3
"""Stub agent for the verification harness.

PHUX_AGENT_VERIFY_STUB marks this file. Invoked under an agent basename
(`claude`, `codex`, `opencode`, `pi`) it emits the same public signals that
agent's screen rules and phux integration use:

- plain: the title or footer the detection manifest treats as working
- integration: identity-only `phux agent set`, then `agent session open` and
  `agent emit` (`prompt` then `stop`), which is how the Claude, pi, and
  OpenCode integrations report a turn

It does not talk to a model or a network. Marks go to PHUX_AGENT_VERIFY_MARK.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

PHUX_AGENT_VERIFY_STUB = "1"

SCREEN = {
    "claude": ("title", "\033]0;⠂ claude\033\\"),
    "codex": ("title", "\033]0;⠋ tmp\033\\"),
    "pi": ("line", " ⠋ Working...\n"),
    "opencode": ("line", "esc interrupt\n"),
}


def now_ms() -> int:
    return int(time.time() * 1000)


def mark(event: str, **fields: object) -> None:
    path = os.environ.get("PHUX_AGENT_VERIFY_MARK")
    if not path:
        return
    record = {"t_ms": now_ms(), "event": event, **fields}
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, sort_keys=True) + "\n")
        handle.flush()
        os.fsync(handle.fileno())


def emit_screen(agent: str) -> None:
    kind, payload = SCREEN.get(agent, ("line", "working\n"))
    sys.stdout.write(payload)
    sys.stdout.flush()
    mark("screen", agent=agent, signal=kind)


def phux(*args: str) -> subprocess.CompletedProcess[str]:
    binary = os.environ.get("PHUX_AGENT_VERIFY_PHUX") or "phux"
    return subprocess.run(
        [binary, *args],
        check=False,
        text=True,
        capture_output=True,
    )


def emit_stream(agent: str) -> None:
    terminal = os.environ.get("PHUX_TERMINAL_ID", "")
    if not terminal:
        mark("error", reason="PHUX_TERMINAL_ID unset")
        return
    target = f"@{terminal}"
    deadline = time.time() + 8
    opened = False
    while time.time() < deadline and not opened:
        identity = phux("agent", "set", target, "--name", agent, "--kind", agent)
        opened_proc = phux(
            "agent",
            "session",
            "open",
            target,
            "--provider",
            agent,
            "--native-id",
            "verify",
            "--json",
        )
        mark(
            "session-open",
            exit_code=opened_proc.returncode,
            stderr=(opened_proc.stderr or "")[-400:],
            identity_exit=identity.returncode,
        )
        opened = opened_proc.returncode == 0
        if not opened:
            time.sleep(0.3)
    if not opened:
        mark("error", reason="session open failed")
        return
    mark("state", state="working")
    prompt = phux(
        "agent",
        "emit",
        target,
        "--type",
        "prompt",
        "--data",
        '{"chars":4}',
        "--json",
    )
    mark("emit", state="working", emit_exit=prompt.returncode, stderr=(prompt.stderr or "")[-400:])
    time.sleep(1.5)
    mark("state", state="done")
    stop = phux("agent", "emit", target, "--type", "stop", "--json")
    mark("emit", state="done", emit_exit=stop.returncode, stderr=(stop.stderr or "")[-400:])


def main() -> int:
    agent = Path(sys.argv[0]).name
    if agent.endswith(".py"):
        agent = os.environ.get("PHUX_AGENT_VERIFY_AGENT", "claude")
    mode = os.environ.get("PHUX_AGENT_VERIFY_MODE", "plain")
    mark("start", agent=agent, mode=mode, argv0=sys.argv[0])
    # Let the harness subscribe `phux watch` to this pane before the first signal.
    time.sleep(2)
    if mode == "integration":
        emit_stream(agent)
        emit_screen(agent)
    else:
        emit_screen(agent)
        mark("state", state="working")
    # The detector's startup grace is 3s and the attach client paints after
    # that. Stay up long enough for both, then let the harness reap us.
    time.sleep(12)
    mark("exit", agent=agent)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:  # noqa: BLE001 — the mark file is the log
        mark("error", reason=type(exc).__name__)
        raise
