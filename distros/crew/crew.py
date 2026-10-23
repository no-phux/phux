#!/usr/bin/env python3
"""Crew's small home/launcher adapter. phux owns missions, panes and mail."""

import argparse
import contextlib
import fcntl
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent
AGENTS = {name: name for name in ("opencode", "claude", "codex", "pi", "omp", "grok")}


def home_path(value=None):
    if value:
        return Path(value).expanduser().resolve()
    if os.environ.get("PHUX_CREW_HOME"):
        return Path(os.environ["PHUX_CREW_HOME"]).expanduser().resolve()
    state = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state"))
    return (state / "phux/crew").resolve()


def phux_command(*args):
    # PHUX is an executable path, not a shell command. Keep the caller's profile
    # and socket environment: development must never fall back to production.
    return [os.environ.get("PHUX", "phux"), *args]


def agent_binary(name):
    if name not in AGENTS:
        raise ValueError(f"unknown agent {name!r}; choose one of {', '.join(AGENTS)}")
    return AGENTS[name]


def bootstrap(home):
    guide = json.dumps(str(ROOT / "lead.md"))
    return (
        "# phux Crew home\n\n"
        f"Read the Crew lead contract at {guide} before doing work.\n"
        "Read POLICY.md for the operator's additional rules.\n"
        "This is a coordinator home, not a project checkout. Never create a git repo here.\n"
        f"Crew bundle: {json.dumps(str(ROOT))}\n"
        f"Crew home: {json.dumps(str(home))}\n"
        "Use phux's native mission store, mail and worktrees; do not build a second scheduler.\n"
    )


def create_once(path, text):
    try:
        with path.open("x", encoding="utf-8") as stream:
            stream.write(text)
    except FileExistsError:
        if path.read_text(encoding="utf-8") != text:
            raise ValueError(f"refusing to replace {path}; choose another --home") from None


def prepare_home(home):
    home.mkdir(parents=True, exist_ok=True, mode=0o700)
    # These are tiny pointers, not copies of the distro. Bundle updates reach
    # the next lead session, while POLICY and native mission state stay put.
    create_once(home / "AGENTS.md", bootstrap(home))
    create_once(home / "CLAUDE.md", "@AGENTS.md\n")
    policy = home / "POLICY.md"
    try:
        with policy.open("x", encoding="utf-8") as stream:
            stream.write("# Operator policy\n\nAdd your project, model, budget and publication preferences here.\n")
    except FileExistsError:
        pass  # Operator-owned. Never compare, copy out, or overwrite it.


@contextlib.contextmanager
def home_lease(home):
    home.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (home / ".lead.lock").open("a", encoding="utf-8") as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError(f"a lead already owns {home}; use another --home for another crew") from None
        try:
            yield stream.fileno()
        finally:
            fcntl.flock(stream, fcntl.LOCK_UN)


def run_agent(argv, home, lease_fd):
    env = dict(os.environ, PHUX_CREW_HOME=str(home), PHUX_CREW_BUNDLE=str(ROOT))
    # Retain the lease in the direct child too: a launcher crash must not
    # immediately admit another lead while its agent still owns the home.
    child = subprocess.Popen(argv, cwd=home, env=env, pass_fds=(lease_fd,))
    previous = {}

    def forward(signum, _frame):
        # The terminal already sends Ctrl-C to the entire foreground group,
        # including this child. Forwarding again turns one cancel into two.
        if signum == signal.SIGINT and foreground_terminal():
            return
        # Only the exact process this launcher started. Never signal by name,
        # port or process group, which may also contain the person's terminal.
        if child.poll() is None:
            try:
                child.send_signal(signum)
            except ProcessLookupError:
                pass

    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        previous[signum] = signal.signal(signum, forward)
    try:
        code = child.wait()
        return code if code >= 0 else 128 - code
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)


def foreground_terminal():
    try:
        return os.tcgetpgrp(sys.stdin.fileno()) == os.getpgrp()
    except (OSError, ValueError):
        return False


def start(args):
    home = home_path(args.home)
    extra = args.extra[1:] if args.extra[:1] == ["--"] else args.extra
    argv = [agent_binary(args.agent), *extra]
    if args.print_only:
        print(json.dumps({"schema_version": 1, "home": str(home), "agent": args.agent, "argv": argv}))
        return 0
    if not shutil.which(argv[0]):
        raise ValueError(f"{argv[0]} is not on PATH; install/sign in to it, or choose --agent")
    if any((directory / ".git").exists() for directory in (home, *home.parents)):
        raise ValueError(f"Crew home {home} is inside a git checkout; choose a coordinator home outside projects")
    with home_lease(home) as lease_fd:
        prepare_home(home)
        print(f"Crew / {args.agent}\nHome: {home}\nPolicy: {home / 'POLICY.md'}", flush=True)
        return run_agent(argv, home, lease_fd)


def doctor(args):
    required = [phux_command()[0], "git", agent_binary(args.agent)]
    missing = [tool for tool in required if not shutil.which(tool)]
    for tool in required:
        print(f"{'missing' if tool in missing else 'ok':7} {tool}")
    print(f"home    {home_path(args.home)}")
    print("Install the matching phux harness integration for lifecycle and mail delivery.")
    print("No tools installed, credentials read, or provider requests made.")
    return int(bool(missing))


def status_document():
    sections = {}
    for label, argv in (("missions", ("mission", "list", "--json")),
                        ("agents", ("agent", "list", "--json"))):
        try:
            result = subprocess.run(phux_command(*argv), capture_output=True, text=True, timeout=10, check=False)
            if result.returncode:
                sections[label] = {"available": False, "error": result.stderr.strip() or f"exit {result.returncode}"}
            else:
                sections[label] = {"available": True, "data": parse_status(label, result.stdout)}
        except (OSError, subprocess.TimeoutExpired, ValueError) as error:
            sections[label] = {"available": False, "error": str(error)}
    return {"schema_version": 1, **sections}


def parse_status(label, text):
    data = json.loads(text)
    key = "tasks" if label == "missions" else "agents"
    if not isinstance(data, dict) or data.get("schema_version") != 1:
        raise ValueError(f"unsupported {label} document")
    rows = data.get(key)
    if not isinstance(rows, list) or not all(isinstance(row, dict) for row in rows):
        raise ValueError(f"invalid {label} rows")
    return data


def print_status(document, json_output=False):
    if json_output:
        print(json.dumps(document))
        return
    print("CREW  /  one lead · a visible fleet\n")
    for label in ("missions", "agents"):
        section = document[label]
        print(label.upper())
        if section["available"]:
            # Native versioned documents stay intact; don't invent a second
            # task state machine or mistake an idle agent for accepted work.
            print_rows(label, section["data"])
        else:
            print(f"unavailable: {section['error']!a}")
    print("\nC-a q: next asking agent · C-a A: agents · Ctrl-C: close board (crew keeps running)")


def print_rows(label, data):
    rows = data.get("tasks" if label == "missions" else "agents", [])
    if not rows:
        print("  No active " + label + ".")
    for row in rows:
        if label == "missions":
            text = f"{str(row.get('state', '?')):16} {str(row.get('alias', '?')):16} {row.get('title', '')}"
        else:
            identity = row.get("agent")
            kind = identity.get("id", "?") if isinstance(identity, dict) else "?"
            text = f"{str(row.get('state', '?')):16} {str(row.get('terminal', '?')):16} {kind} {row.get('address') or ''}"
        # A title, name or error is data, never trusted terminal escape code.
        print("  " + json.dumps(text, ensure_ascii=True)[1:-1])


def status(args):
    document = status_document()
    print_status(document, args.json)
    return int(not all(document[key]["available"] for key in ("missions", "agents")))


def board(args):
    try:
        while True:
            if sys.stdout.isatty():
                print("\033[2J\033[H", end="", flush=True)
            print_status(status_document())
            time.sleep(args.interval)
    except KeyboardInterrupt:
        return 0


def parser():
    cli = argparse.ArgumentParser(description="Crew — one lead, a visible fleet, native phux state.")
    commands = cli.add_subparsers(dest="command", required=True)
    for name, handler in (("start", start), ("doctor", doctor)):
        command = commands.add_parser(name)
        command.add_argument("--home", help="isolated lead home (or PHUX_CREW_HOME)")
        command.add_argument("--agent", choices=AGENTS, default=os.environ.get("PHUX_CREW_AGENT", "opencode"))
        command.set_defaults(handler=handler)
    launch = commands.choices["start"]
    launch.add_argument("--print", dest="print_only", action="store_true", help="print launch JSON without writing or starting anything")
    launch.add_argument("extra", nargs=argparse.REMAINDER, help="agent arguments after --")
    pulse = commands.add_parser("status")
    pulse.add_argument("--json", action="store_true")
    pulse.set_defaults(handler=status)
    live = commands.add_parser("board")
    live.add_argument("--interval", type=positive_seconds, default=5)
    live.set_defaults(handler=board)
    return cli


def positive_seconds(value):
    seconds = int(value)
    if seconds < 1 or seconds > 3600:
        raise argparse.ArgumentTypeError("interval must be between 1 and 3600 seconds")
    return seconds


def main():
    args = parser().parse_args()
    try:
        return args.handler(args)
    except (OSError, ValueError) as error:
        print(f"crew: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
