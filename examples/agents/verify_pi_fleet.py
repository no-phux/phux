#!/usr/bin/env python3
"""Independently challenge real fleet panes; Pi narration is not evidence."""
import json
from pathlib import Path
import re
import secrets
import subprocess
import sys


def validate_child(child: dict, cwd: str) -> tuple[str, str]:
    kind = child["agent"]["kind"]
    terminal = child["terminal"]
    if kind not in ("claude", "codex"):
        raise ValueError("proof needs one Claude and one Codex child")
    if not re.fullmatch(r"@[0-9]+", terminal) or child["cwd"] != cwd:
        raise ValueError("child must be a local pane in the proof working directory")
    return kind, terminal


def child_panes(document: dict, session: str, cwd: str) -> dict[str, str]:
    children = [agent for agent in document["agents"]
                if agent["session"] == session and agent["agent"]["kind"] in ("claude", "codex")]
    if len(children) != 2:
        raise ValueError("proof needs exactly two child agents in its own session")
    panes = {}
    for child in children:
        kind, terminal = validate_child(child, cwd)
        if kind in panes:
            raise ValueError("proof needs one Claude and one Codex child")
        panes[kind] = terminal
    if len(set(panes.values())) != 2:
        raise ValueError("proof children must be distinct panes")
    return panes


def verify_response(snapshot: dict, terminal: str, token: str) -> None:
    if snapshot["pane"] != int(terminal[1:]):
        raise ValueError("snapshot came from the wrong child pane")
    # Claude prefixes assistant output with a bullet. Do not accept prompts,
    # source, tool arguments, or narration that merely contains the token.
    lines = [line.strip().lstrip("●• ") for line in snapshot["lines"]]
    if token not in lines:
        raise ValueError(f"child {terminal} did not print its computed response")


def cli(binary: str, socket: str, verb: str, *args: str) -> str:
    return subprocess.run([binary, verb, "--socket", socket, *args],
                          capture_output=True, text=True, check=True).stdout


def main(binary: str, socket: str, session: str, cwd: str) -> None:
    inventory = json.loads(cli(binary, socket, "agent", "list", "--json"))
    panes = child_panes(inventory, session, str(Path(cwd).resolve()))
    for kind, terminal in panes.items():
        # This nonce is chosen after Pi exits and never appears in Pi's prompt.
        words = f"{kind.upper()} LIVE FLEET OK {secrets.token_hex(8)}"
        token = words.replace(" ", "_")
        cli(binary, socket, "send-keys", terminal,
            f"Reply only with the underscore token formed from {words}. Do not use tools.")
        cli(binary, socket, "send-keys", terminal, "Enter")
        cli(binary, socket, "wait", "--until", token, "--timeout", "180", terminal)
        snapshot = json.loads(cli(binary, socket, "snapshot", "--json", terminal))
        verify_response(snapshot, terminal, token)
        print(f"{terminal} kind={kind}: fresh computed-response output verified")


if __name__ == "__main__":
    main(*sys.argv[1:])
