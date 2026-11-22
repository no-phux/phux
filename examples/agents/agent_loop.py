#!/usr/bin/env python3
"""The read+act+wait loop, as an agent would wire it in code.

A self-contained illustration of driving phux purely via its CLI: every
phux interaction is a subprocess call, and the JSON verbs (`ls --json`,
`snapshot --json`, `run --json`) are parsed straight into dicts. There is
no phux client library here on purpose -- the CLI *is* the agent surface
(ADR-0022). Any language that can spawn a process and parse JSON can drive
phux this way.

Run it:   python3 examples/agents/agent_loop.py

It stands up a throwaway server on a private socket so it never touches
the user's real one-per-user server. A production agent skips that and
just runs `phux <verb>` with no --socket.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def find_phux() -> str:
    """Use an explicit binary or this checkout; never build or fall back to PATH."""
    repo_root = Path(__file__).resolve().parents[2]
    candidate = Path(os.environ.get("PHUX", repo_root / "target/debug/phux"))
    if not os.access(candidate, os.X_OK):
        raise FileNotFoundError(
            "build this checkout first (just build), or set PHUX=/absolute/path/to/phux"
        )
    return str(candidate.resolve())


def private_env(directory: str) -> dict[str, str]:
    """Isolate state/config and discard pane-inherited phux settings."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("PHUX_")}
    for name in ("ENV", "BASH_ENV", "ZDOTDIR", "PYTHONSTARTUP"):
        env.pop(name, None)
    home = Path(directory) / "home"
    home.mkdir()
    runtime = Path(directory) / "run"
    runtime.mkdir()
    env.update(
        HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"),
        XDG_STATE_HOME=str(home / ".local/state"),
        XDG_DATA_HOME=str(home / ".local/share"), XDG_CACHE_HOME=str(home / ".cache"),
        XDG_RUNTIME_DIR=str(runtime), SHELL="/bin/sh", PHUX_NO_UPDATE_CHECK="1",
    )
    return env


class Phux:
    """A thin CLI wrapper: each method is one `phux <verb>` invocation.

    Insert `--socket` before trailing positional args so it cannot become
    part of a command or keystroke.
    """

    def __init__(self, binary: str, socket: str, session: str, env: dict[str, str]) -> None:
        self.binary = binary
        self.socket = socket
        self.session = session
        self.env = env

    def _run(self, verb: str, *args: str, check: bool = True) -> subprocess.CompletedProcess:
        cmd = [self.binary, verb, "--socket", self.socket, *args]
        return subprocess.run(cmd, env=self.env, capture_output=True, text=True, check=check)

    def ls(self) -> list[dict]:
        """Enumerate sessions (the `ls --json` contract)."""
        out = self._run("ls", "--json").stdout
        return json.loads(out)["sessions"]

    def snapshot(self) -> dict:
        """Read the focused pane as structured data (side-effect-free)."""
        out = self._run("snapshot", self.session, "--json").stdout
        return json.loads(out)

    def send_keys(self, *keys: str) -> None:
        """Send named keys and/or literal strings to the focused pane."""
        self._run("send-keys", self.session, *keys)

    def run(self, command: str, timeout_s: int = 30) -> dict:
        """Run a discrete command; return {command, exit_code, output, ...}.

        `run` mirrors the child's exit code into its own, so check=False
        here: a non-zero command is data, not a Python exception.
        """
        proc = self._run(
            "run", "--json", "--timeout", str(timeout_s), self.session, command,
            check=False,
        )
        if not proc.stdout:
            proc.check_returncode()  # Transport/usage failure is not a command result.
        return json.loads(proc.stdout)

    @staticmethod
    def _wait_result(proc: subprocess.CompletedProcess) -> bool:
        if proc.returncode == 124:
            return False
        proc.check_returncode()
        return True

    def wait_until(self, text: str, timeout_s: int = 10) -> bool:
        """Block until a visible line contains `text`. True if met, False on timeout.

        `--until` matches the echo of the command you typed too, so wait on
        text that appears only in OUTPUT.
        """
        proc = self._run(
            "wait", "--until", text, "--timeout", str(timeout_s), self.session,
            check=False,
        )
        return self._wait_result(proc)

    def wait_idle(self, idle_ms: int = 500, timeout_s: int = 10) -> bool:
        """Block until the screen holds still for `idle_ms`."""
        proc = self._run(
            "wait", "--idle", str(idle_ms), "--timeout", str(timeout_s), self.session,
            check=False,
        )
        return self._wait_result(proc)


def run_example(phux: Phux) -> int:
    print("sessions:", [s["name"] for s in phux.ls()])
    result = phux.run("echo hello; echo world")
    if result["exit_code"] != 0:
        raise RuntimeError(f"example command failed: {result}")
    print(f"run exit={result['exit_code']} output={result['output']!r}")

    # Neither the formatted prompt nor the computed result occurs in the source.
    prog = "sh -c 'printf \"Pick a %s: \" number; read -r n; echo \"result=$((n * n))\"'"
    phux.send_keys(prog, "Enter")
    if not phux.wait_until("Pick a number:"):
        raise TimeoutError("prompt never appeared")

    scr = phux.snapshot()
    prompt = next((line for line in scr["lines"] if "Pick a number:" in line), "")
    print(f"prompt on screen: {prompt.strip()!r}")
    phux.send_keys("7", "Enter")
    if not phux.wait_until("result=49"):
        raise TimeoutError("program did not compute the result")
    print("loop complete: program computed 7*7=49")
    if not phux.wait_idle():
        raise TimeoutError("pane did not settle")
    return 0


def wait_for_server(server: subprocess.Popen, socket: str) -> None:
    for _ in range(200):
        if Path(socket).is_socket():
            return
        if server.poll() is not None:
            break
        time.sleep(0.025)
    raise RuntimeError("private server did not bind; see server log above")


def stop_server(server: subprocess.Popen) -> None:
    server.terminate()
    try:
        server.wait(timeout=5)
    except subprocess.TimeoutExpired:
        server.kill()
        server.wait()


def main() -> int:
    binary = find_phux()
    with tempfile.TemporaryDirectory(prefix="phux-agent-loop.") as tmpdir:
        socket = str(Path(tmpdir) / "phux.sock")
        env = private_env(tmpdir)
        with tempfile.TemporaryFile(mode="w+") as log:
            server = subprocess.Popen(
                [binary, "server", "--session", "demo", "--socket", socket,
                 "--exit-after-idle", "60"],
                env=env, stdout=log, stderr=log,
            )
            try:
                wait_for_server(server, socket)
                return run_example(Phux(binary, socket, "demo", env))
            except Exception:
                log.seek(0)
                print(log.read(), end="", file=sys.stderr)
                raise
            finally:
                stop_server(server)


if __name__ == "__main__":
    sys.exit(main())
