#!/usr/bin/env python3
"""Run a short phux session for each real agent CLI, with and without its integration.

Reads agent state only through the public CLI (`agent show`, `resource show`,
`watch`) and through two `phux attach` clients on the same session. Missing
agents are `not_installed`. Codex has no lifecycle integration, so that row
is `integration_unavailable`. Nothing here is invented when a run does not
happen.

  python3 scripts/agent-verify/run.py
"""

from __future__ import annotations

import argparse
import json
import os
import pty
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import termios
import threading
import time
from pathlib import Path
from typing import Any, IO

sys.path.insert(0, str(Path(__file__).resolve().parent))

import lib  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
AGENTS = ("claude", "codex", "opencode", "pi")
PROMPT = "Reply with exactly the word pong."


def now_ms() -> int:
    return int(time.time() * 1000)


def which(name: str) -> Path | None:
    found = shutil.which(name)
    return Path(found) if found else None


def is_stub(path: Path) -> bool:
    try:
        with path.open("rb") as handle:
            blob = handle.read(65536)
    except OSError:
        return False
    return b"PHUX_AGENT_VERIFY_STUB" in blob


def find_phux(explicit: str | None) -> Path:
    if explicit:
        path = Path(explicit)
        if path.is_file() and os.access(path, os.X_OK):
            return path.resolve()
        raise SystemExit(f"phux binary is not executable: {explicit}")
    env = os.environ.get("PHUX")
    if env:
        return find_phux(env)
    found = which("phux")
    if found:
        return found.resolve()
    debug = REPO / "target" / "debug" / "phux"
    if debug.is_file() and os.access(debug, os.X_OK):
        return debug.resolve()
    raise SystemExit("set PHUX or pass --phux; no phux binary on PATH or at target/debug/phux")


class Log:
    def __init__(self, root: Path, homes: list[str]) -> None:
        self.root = root
        self.homes = homes
        self.root.mkdir(parents=True, exist_ok=True)
        self._handle = (root / "commands.log").open("a", encoding="utf-8")

    def write(self, text: str) -> None:
        self._handle.write(lib.redact(text, self.homes))
        self._handle.flush()

    def close(self) -> None:
        if not self._handle.closed:
            self._handle.close()

    def dump(self, name: str, text: str) -> None:
        (self.root / name).write_text(lib.redact(text, self.homes), encoding="utf-8")


class Phux:
    def __init__(self, binary: Path, socket: Path, log: Log) -> None:
        self.binary = binary
        self.socket = socket
        self.log = log

    def run(self, args: list[str], timeout: float = 20) -> subprocess.CompletedProcess[str]:
        cmd = [str(self.binary), args[0], "--socket", str(self.socket), *args[1:]]
        self.log.write("$ " + " ".join(cmd) + "\n")
        try:
            done = subprocess.run(
                cmd,
                check=False,
                text=True,
                capture_output=True,
                timeout=timeout,
                env=os.environ,
            )
        except subprocess.TimeoutExpired as exc:
            self.log.write(f"timeout after {timeout}s\n")
            return subprocess.CompletedProcess(cmd, 124, exc.stdout or "", exc.stderr or "timeout")
        self.log.write(f"exit {done.returncode}\n")
        if done.stdout:
            self.log.write(done.stdout[-4000:] + "\n")
        if done.stderr:
            self.log.write(done.stderr[-4000:] + "\n")
        return done

    def json(self, args: list[str], timeout: float = 20) -> tuple[int, dict[str, Any] | None, str]:
        done = self.run(args, timeout=timeout)
        text = done.stdout.strip()
        if not text:
            return done.returncode, None, done.stderr
        try:
            parsed = json.loads(text)
        except json.JSONDecodeError:
            return done.returncode, None, text
        return done.returncode, parsed if isinstance(parsed, dict) else None, done.stderr


class ByteLog:
    """A background read of a pipe or pty, kept for later search."""

    def __init__(self) -> None:
        self._buf = bytearray()
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self.lines: list[tuple[int, str]] = []

    def append(self, chunk: bytes) -> None:
        with self._lock:
            self._buf.extend(chunk)
            if len(self._buf) > 1_000_000:
                del self._buf[:-500_000]

    def note_line(self, seen_ms: int, line: str) -> None:
        with self._lock:
            self.lines.append((seen_ms, line))

    def lines_since(self, index: int) -> tuple[int, list[tuple[int, str]]]:
        with self._lock:
            return len(self.lines), list(self.lines[index:])

    def text_since(self, mark: int) -> str:
        with self._lock:
            data = bytes(self._buf[mark:])
        return data.decode("utf-8", errors="replace")

    def mark(self) -> int:
        with self._lock:
            return len(self._buf)

    def stop(self) -> None:
        self._stop.set()


def _read_fd(fd: int, sink: ByteLog) -> None:
    while not sink._stop.is_set():
        try:
            ready, _, _ = select.select([fd], [], [], 0.2)
        except (OSError, ValueError):
            return
        if not ready:
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            return
        if not chunk:
            return
        sink.append(chunk)


def _spawn_reader(cmd: list[str], env: dict[str, str], sink: ByteLog, pty_size: bool) -> subprocess.Popen[bytes]:
    if pty_size:
        master, slave = pty.openpty()
        winsize = __import__("struct").pack("HHHH", 40, 120, 0, 0)
        __import__("fcntl").ioctl(slave, termios.TIOCSWINSZ, winsize)
        proc = subprocess.Popen(
            cmd,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env=env,
            start_new_session=True,
            close_fds=True,
        )
        os.close(slave)
        thread = threading.Thread(target=_read_fd, args=(master, sink), daemon=True)
        thread.start()
        proc._phux_master = master  # type: ignore[attr-defined]
        return proc
    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=env,
        start_new_session=True,
    )
    assert proc.stdout is not None
    thread = threading.Thread(target=_read_stream, args=(proc.stdout, sink), daemon=True)
    thread.start()
    return proc


def _read_stream(handle: IO[bytes], sink: ByteLog) -> None:
    while not sink._stop.is_set():
        chunk = handle.readline()
        if not chunk:
            return
        seen = now_ms()
        sink.append(chunk)
        sink.note_line(seen, chunk.decode("utf-8", errors="replace"))


def _kill(proc: subprocess.Popen[Any] | None) -> None:
    if proc is None or proc.poll() is not None:
        return
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except OSError:
        proc.terminate()
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except OSError:
            proc.kill()
        proc.wait(timeout=3)
    master = getattr(proc, "_phux_master", None)
    if isinstance(master, int):
        os.close(master)


def server_env(socket: Path, state: Path, binary: Path) -> dict[str, str]:
    env = os.environ.copy()
    for key in list(env):
        if key.startswith("PHUX_WS_") or key.startswith("PHUX_QUIC") or key.startswith("PHUX_WT_"):
            del env[key]
    env.pop("PHUX_AGENT_STARTUP_GRACE_MS", None)
    env.pop("PHUX_AGENT_IDENTIFY_RECHECK_MS", None)
    env["PHUX_SOCKET"] = str(socket)
    env["PHUX_PROFILE"] = "agent-verify"
    env["PHUX_NO_UPDATE_CHECK"] = "1"
    env["XDG_CONFIG_HOME"] = str(state / "config")
    env["XDG_STATE_HOME"] = str(state / "state")
    env["XDG_RUNTIME_DIR"] = str(state / "run")
    env["TERM"] = env.get("TERM") or "xterm-256color"
    env["LANG"] = env.get("LANG") or "C.UTF-8"
    bindir = str(binary.parent)
    env["PATH"] = bindir + os.pathsep + env.get("PATH", "")
    for path in (state / "config", state / "state", state / "run"):
        path.mkdir(parents=True, exist_ok=True)
    # First attach otherwise paints the onboarding card over the sidebar.
    profile_state = state / "state" / "phux-agent-verify"
    profile_state.mkdir(parents=True, exist_ok=True)
    (profile_state / "onboarding.json").write_text(
        '{"version":1,"stage":"complete"}\n',
        encoding="utf-8",
    )
    return env


def start_server(binary: Path, socket: Path, state: Path, log_path: Path) -> subprocess.Popen[bytes]:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    handle = log_path.open("ab")
    proc = subprocess.Popen(
        [
            str(binary),
            "server",
            "--socket",
            str(socket),
            "--session",
            "agent-verify",
            "--exit-after-idle",
            "180",
        ],
        env=server_env(socket, state, binary),
        stdout=handle,
        stderr=subprocess.STDOUT,
        stdin=subprocess.DEVNULL,
        start_new_session=True,
    )
    deadline = time.time() + 15
    while time.time() < deadline:
        if socket.exists() and proc.poll() is None:
            return proc
        if proc.poll() is not None:
            break
        time.sleep(0.05)
    _kill(proc)
    raise RuntimeError(f"phux server did not bind {socket}; see {log_path}")


def integration_available(agent: str) -> bool:
    if agent == "codex":
        return False
    if agent == "claude":
        return (REPO / "integrations" / "claude").is_dir()
    if agent == "pi":
        return (REPO / "integrations" / "pi").is_dir() and (REPO / ".pi" / "settings.json").is_file()
    if agent == "opencode":
        return (REPO / "integrations" / "opencode-v2" / "index.js").is_file()
    return False


def prepare_cwd(agent: str, integration: bool, stub: bool, socket: Path, work: Path) -> Path:
    if stub or agent not in {"pi", "opencode"}:
        cwd = work / "cwd"
        cwd.mkdir(parents=True, exist_ok=True)
        return cwd
    if agent == "pi":
        return REPO if integration else work / "cwd-pi-off"
    project = work / ("opencode-on" if integration else "opencode-off")
    project.mkdir(parents=True, exist_ok=True)
    if integration:
        config = {
            "$schema": "https://opencode.ai/config.json",
            "plugin": [
                {
                    "package": str(REPO / "integrations" / "opencode-v2"),
                    "options": {"socket": str(socket)},
                }
            ],
        }
        # OpenCode accepts either key; the README uses "plugins".
        config["plugins"] = config.pop("plugin")
        (project / "opencode.json").write_text(json.dumps(config), encoding="utf-8")
    return project


def spawn_argv(
    agent: str,
    binary: Path,
    integration: bool,
    stub: bool,
    mark: Path,
    phux_bin: Path,
    user_home: str,
    user_config: str | None,
) -> list[str]:
    """Environment prefix plus the agent. Restores the operator's home for real CLIs.

    macOS `env` wants `-u` before `NAME=value` and does not treat `--` as
    end of options, so the agent path is the first non-assignment argument.
    """
    argv = ["env", "-u", "XDG_STATE_HOME", "-u", "XDG_RUNTIME_DIR"]
    if user_config:
        argv.append(f"XDG_CONFIG_HOME={user_config}")
    else:
        argv.extend(["-u", "XDG_CONFIG_HOME"])
    argv.extend(
        [
            f"HOME={user_home}",
            f"PHUX_AGENT_VERIFY_PHUX={phux_bin}",
            f"PHUX_AGENT_VERIFY_MARK={mark}",
            f"PHUX_AGENT_VERIFY_MODE={'integration' if integration else 'plain'}",
            f"PHUX_AGENT_REPORTING={'1' if integration else '0'}",
            str(binary),
        ]
    )
    if not stub and agent == "claude" and integration:
        argv.extend(["--plugin-dir", str(REPO / "integrations" / "claude")])
    return argv


def load_marks(path: Path) -> list[dict[str, Any]]:
    if not path.is_file():
        return []
    rows = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            parsed = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict):
            rows.append(parsed)
    return rows


def latency_ms(state: str, seen_ms: int, marks: list[dict[str, Any]], prompt_ms: int | None, spawn_ms: int) -> tuple[int, str]:
    candidates = [
        int(mark["t_ms"])
        for mark in marks
        if mark.get("event") == "state" and mark.get("state") == state and isinstance(mark.get("t_ms"), int)
    ]
    prior = [stamp for stamp in candidates if stamp <= seen_ms]
    if prior:
        return seen_ms - prior[-1], "agent_mark"
    if prompt_ms is not None and state in {"working", "blocked", "done", "idle"}:
        return max(0, seen_ms - prompt_ms), "prompt"
    return max(0, seen_ms - spawn_ms), "spawn"


def run_session(
    phux: Phux,
    env: dict[str, str],
    agent: str,
    integration: bool,
    timeout: float,
    work: Path,
    homes: list[str],
    user_home: str,
    user_config: str | None,
) -> dict[str, Any]:
    label = f"{agent}-{'on' if integration else 'off'}"
    log = Log(work / "logs" / label, homes)
    binary = which(agent)
    row: dict[str, Any] = {
        "agent": agent,
        "integration": "on" if integration else "off",
        "result": "fail",
        "tier": None,
        "agreement": None,
        "time_to_state_ms": {"samples": [], "p50": None, "max": None, "t0_source": None},
        "states": [],
        "divergence": None,
        "log": f"logs/{label}",
    }
    if binary is None:
        row["result"] = "not_installed"
        row["integration"] = "n/a"
        log.write(f"{agent} is not on PATH\n")
        log.close()
        return row
    if integration and not integration_available(agent):
        row["result"] = "integration_unavailable"
        row["note"] = (
            "Codex has screen rules and no lifecycle integration in this checkout."
            if agent == "codex"
            else f"No phux integration for {agent} is present in this checkout."
        )
        log.write(row["note"] + "\n")
        log.close()
        return row

    stub = is_stub(binary)
    mark_path = log.root / "marks.jsonl"
    cwd = prepare_cwd(agent, integration, stub, phux.socket, log.root)
    if not cwd.is_dir():
        cwd.mkdir(parents=True, exist_ok=True)
    argv = spawn_argv(agent, binary, integration, stub, mark_path, phux.binary, user_home, user_config)
    session = f"av-{agent}-{'on' if integration else 'off'}"
    row["session"] = session
    log.write("new " + session + " " + " ".join(argv) + f" cwd={cwd}\n")

    attach_env = env.copy()
    attach_sinks = [ByteLog(), ByteLog()]
    attaches: list[subprocess.Popen[bytes]] = []
    spawn_at = now_ms()
    # The agent is the session's seed pane, so an attach client's layout is
    # that pane rather than a shell spawned beside it.
    code, spawned, err = phux.json(
        ["new", "--json", "-s", session, "-c", str(cwd), "--", *argv],
        timeout=20,
    )
    if code != 0 or not spawned or "terminal_id" not in spawned:
        row["divergence"] = f"new failed: {err or spawned}"
        log.dump("new.json", json.dumps({"exit": code, "stdout": spawned, "stderr": err}))
        _finish_clients(None, ByteLog(), attaches, attach_sinks, log)
        log.close()
        return row
    pane = f"@{spawned['terminal_id']}"
    row["pane"] = pane
    log.write(f"pane {pane}\n")
    watch_sink = ByteLog()
    watch = _spawn_reader(
        [str(phux.binary), "watch", "--json", "--socket", str(phux.socket), pane],
        attach_env,
        watch_sink,
        pty_size=False,
    )
    watch_index = 0
    # Attach after the pane exists so the TUI layout includes it. The stub waits
    # before its first signal, which leaves time to connect.
    attaches = [
        _spawn_reader(
            [str(phux.binary), "attach", "--socket", str(phux.socket), session],
            attach_env,
            sink,
            pty_size=True,
        )
        for sink in attach_sinks
    ]
    attach_marks = [0 for _ in attach_sinks]

    prompt_ms: int | None = None
    if not stub:
        prompt_ms = _prompt_when_ready(phux, pane, timeout / 3)
        log.write(f"prompt_ms {prompt_ms}\n")

    deadline = time.time() + timeout
    observations: list[dict[str, Any]] = []
    seen_watch: list[tuple[int, str]] = []
    try:
        while time.time() < deadline:
            watch_index, fresh = watch_sink.lines_since(watch_index)
            for seen, state in _parse_watch_lines(fresh, pane):
                seen_watch.append((seen, state))
                observations.append(_observe(phux, pane, state, seen, log))
            if seen_watch:
                latest = seen_watch[-1][1]
                confirmed = any(
                    obs.get("watch_state") == latest
                    and lib.unwrap_agent(obs.get("show")).get("state") == latest
                    for obs in observations
                )
                if latest not in (None, "unknown") and not confirmed:
                    observations.append(_observe(phux, pane, latest, now_ms(), log))
            if _ready(observations, attach_sinks, attach_marks, agent):
                break
            time.sleep(0.2)
        final = _observe(phux, pane, None, now_ms(), log)
        observations.append(final)
    finally:
        phux.run(["kill", "--yes", session], timeout=15)
        _finish_clients(watch, watch_sink, attaches, attach_sinks, log)

    marks = load_marks(mark_path)
    log.dump("marks.jsonl", mark_path.read_text(encoding="utf-8") if mark_path.is_file() else "")
    return _summarize(row, observations, seen_watch, marks, prompt_ms, spawn_at, attach_sinks, attach_marks, log)


def _prompt_when_ready(phux: Phux, pane: str, budget: float) -> int:
    deadline = time.time() + max(3, budget)
    while time.time() < deadline:
        code, snap, _err = phux.json(["snapshot", "--json", "--tail", "20", pane], timeout=10)
        lines = (snap or {}).get("lines") if code == 0 else None
        if isinstance(lines, list) and any(str(line).strip() for line in lines):
            break
        time.sleep(0.3)
    phux.run(["paste", pane, PROMPT], timeout=15)
    sent = now_ms()
    phux.run(["send-keys", pane, "Enter"], timeout=15)
    return sent


def _parse_watch_lines(lines: list[tuple[int, str]], pane: str) -> list[tuple[int, str]]:
    """`agent_state` has no server ts_ms. `seen` is when this client read the line."""
    found = []
    for seen, line in lines:
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            parsed = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(parsed, dict) or parsed.get("event") != "agent_state":
            continue
        if parsed.get("terminal") not in (None, pane):
            continue
        state = parsed.get("state")
        if isinstance(state, str):
            found.append((seen, state))
    return found


def _observe(phux: Phux, pane: str, watch_state: str | None, seen: int, log: Log) -> dict[str, Any]:
    _code, show, show_err = phux.json(["agent", "show", "--json", pane], timeout=15)
    _rcode, resource, res_err = phux.json(["resource", "show", "--json", pane], timeout=15)
    ranked = lib.classify(show, resource)
    if show is not None:
        log.dump("agent-show.json", json.dumps(show, indent=2))
    if resource is not None:
        log.dump("resource-show.json", json.dumps(resource, indent=2))
    if show_err:
        log.write(show_err[-500:] + "\n")
    if res_err:
        log.write(res_err[-500:] + "\n")
    return {"seen_ms": seen, "watch_state": watch_state, "show": show, "resource": resource, "rank": ranked}


def _ready(
    observations: list[dict[str, Any]],
    sinks: list[ByteLog],
    marks: list[int],
    agent: str,
) -> bool:
    confirmed = any(
        obs.get("watch_state") == "working"
        and lib.unwrap_agent(obs.get("show")).get("state") == "working"
        and obs["rank"].get("tier")
        for obs in observations
    )
    if not confirmed:
        return False
    tails = [sink.text_since(mark)[-4000:] for sink, mark in zip(sinks, marks)]
    return all("none running" not in tail and agent in tail for tail in tails)


def _finish_clients(
    watch: subprocess.Popen[bytes] | None,
    watch_sink: ByteLog,
    attaches: list[subprocess.Popen[bytes]],
    sinks: list[ByteLog],
    log: Log,
) -> None:
    watch_sink.stop()
    for sink in sinks:
        sink.stop()
    log.dump("watch.jsonl", watch_sink.text_since(0))
    for index, sink in enumerate(sinks, start=1):
        log.dump(f"attach-{index}.raw", sink.text_since(0))
    if watch is not None:
        _kill(watch)
    for proc in attaches:
        _kill(proc)


def _summarize(
    row: dict[str, Any],
    observations: list[dict[str, Any]],
    seen_watch: list[tuple[int, str]],
    marks: list[dict[str, Any]],
    prompt_ms: int | None,
    spawn_ms: int,
    sinks: list[ByteLog],
    attach_marks: list[int],
    log: Log,
) -> dict[str, Any]:
    log.close()
    samples = []
    sources = []
    states = []
    for seen, state in seen_watch:
        if state in ("", "unknown") or state is None:
            continue
        if state not in states:
            states.append(state)
        delay, source = latency_ms(state, seen, marks, prompt_ms, spawn_ms)
        samples.append({"state": state, "ms": delay, "t0_source": source})
        sources.append(source)
    primary = next(
        (
            obs
            for obs in observations
            if obs.get("watch_state") == "working"
            and lib.unwrap_agent(obs.get("show")).get("state") == "working"
            and obs["rank"].get("tier")
        ),
        None,
    )
    if primary is None:
        primary = next((obs for obs in reversed(observations) if obs["rank"].get("tier")), None)
    rank = (primary or {}).get("rank") or {}
    row["tier"] = rank.get("tier")
    row["tier_note"] = rank.get("note")
    row["tier_candidates"] = rank.get("candidates") or []
    row["states"] = states or ([rank["state"]] if rank.get("state") else [])
    numbers = [sample["ms"] for sample in samples]
    row["time_to_state_ms"] = {
        "samples": samples,
        "p50": lib.percentile(numbers, 50),
        "max": max(numbers) if numbers else None,
        "t0_source": sources[0] if sources else None,
    }
    screens = [sink.text_since(mark) for sink, mark in zip(sinks, attach_marks)]
    show_state = lib.unwrap_agent((primary or {}).get("show")).get("state")
    watch_state = (primary or {}).get("watch_state")
    if watch_state is None and seen_watch:
        watch_state = next((state for _seen, state in seen_watch if state not in ("unknown", None)), None)
    divergence: list[str] = []
    if not seen_watch:
        divergence.append("watch client saw no agent_state event")
    elif show_state != watch_state:
        divergence.append(f"agent show state {show_state!r} disagrees with watch {watch_state!r}")
    # The welcome card prints "◐ working" as a legend. An attach client has
    # seen the agent when its latest frame lists the agent name and is not
    # still on the empty Agents panel.
    tails = [screen[-4000:] for screen in screens]
    for index, tail in enumerate(tails, start=1):
        if "none running" in tail or row["agent"] not in tail:
            divergence.append(f"attach-{index} did not list {row['agent']}")
    if len(tails) == 2 and (("none running" in tails[0]) != ("none running" in tails[1])):
        divergence.append("the two attach clients disagree on whether an agent is listed")
    row["divergence"] = "; ".join(divergence) if divergence else None
    row["agreement"] = "yes" if not divergence else "no"
    if row["tier"] in (None, lib.TIER_UNRESOLVED):
        row["result"] = "fail"
        if row["tier"] == lib.TIER_UNRESOLVED and not divergence:
            row["divergence"] = rank.get("note")
    elif divergence:
        row["result"] = "fail"
    elif not row["states"] and not rank.get("state"):
        row["result"] = "fail"
        row["divergence"] = "phux published no lifecycle state before the timeout"
    else:
        row["result"] = "pass"
    return row


def blank_skip(agent: str) -> dict[str, Any]:
    return {
        "agent": agent,
        "integration": "n/a",
        "result": "not_installed",
        "tier": None,
        "agreement": None,
        "time_to_state_ms": {"samples": [], "p50": None, "max": None, "t0_source": None},
        "states": [],
        "divergence": None,
        "log": None,
    }


def execute(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Measure phux agent-state evidence on real CLIs.")
    parser.add_argument("--phux", help="phux binary. Defaults to $PHUX, then PATH, then target/debug/phux.")
    parser.add_argument("--out", type=Path, help="Results directory. Default: ./agent-verify-results/<UTC>/")
    parser.add_argument("--timeout", type=float, default=90, help="Seconds to wait for a state in one run.")
    parser.add_argument("--agents", default=",".join(AGENTS), help="Comma-separated subset of claude,codex,opencode,pi.")
    parser.add_argument("--home", help="Home directory real agents should see. Default: the operator's home.")
    args = parser.parse_args(argv)
    selected = [name.strip() for name in args.agents.split(",") if name.strip()]
    unknown = [name for name in selected if name not in AGENTS]
    if unknown:
        raise SystemExit(f"unknown agent {unknown}; expected one of {', '.join(AGENTS)}")
    binary = find_phux(args.phux)
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    out = args.out or (Path.cwd() / "agent-verify-results" / stamp)
    out.mkdir(parents=True, exist_ok=True)
    work = out
    user_home = args.home or str(Path.home())
    user_config = os.environ.get("XDG_CONFIG_HOME")
    homes = [user_home, str(Path.home())]
    version = subprocess.run(
        [str(binary), "--version"],
        check=False,
        text=True,
        capture_output=True,
    )
    runtime = Path(tempfile.mkdtemp(prefix="phux-agent-verify-server-"))
    socket = runtime / "phux.sock"
    state = runtime
    server: subprocess.Popen[bytes] | None = None
    phux: Phux | None = None
    runs: list[dict[str, Any]] = []
    try:
        server = start_server(binary, socket, state, runtime / "server.log")
        env = server_env(socket, state, binary)
        # CLI calls from this process must hit the private server, not a production one.
        os.environ["PHUX_SOCKET"] = str(socket)
        os.environ["PHUX_PROFILE"] = "agent-verify"
        phux = Phux(binary, socket, Log(work / "logs" / "_driver", homes))
        for agent in selected:
            if which(agent) is None:
                runs.append(blank_skip(agent))
                continue
            for integration in (False, True):
                runs.append(
                    run_session(
                        phux,
                        env,
                        agent,
                        integration,
                        args.timeout,
                        work,
                        homes,
                        user_home,
                        user_config,
                    )
                )
    finally:
        _kill(server)
        if phux is not None:
            phux.log.close()
        server_log = runtime / "server.log"
        if server_log.is_file():
            raw = server_log.read_text(encoding="utf-8", errors="replace")
            (work / "server.log").write_text(lib.redact(raw, homes), encoding="utf-8")
        shutil.rmtree(runtime, ignore_errors=True)
    payload = {
        "schema_version": 1,
        "generated_at": stamp,
        "phux": {"path": str(binary), "version": (version.stdout or version.stderr).strip()},
        "host": {"os": sys.platform, "machine": os.uname().machine},
        "runs": runs,
        "limitations": lib.LIMITATIONS,
    }
    # Redact the document itself before it is written.
    encoded = lib.redact(json.dumps(payload), homes)
    payload = json.loads(encoded)
    lib.write_results(work, payload)
    print(lib.markdown_table(runs), end="")
    print(f"results: {work / 'results.json'}")
    failed = [run for run in runs if run.get("result") == "fail"]
    return 1 if failed else 0


def main() -> None:
    try:
        raise SystemExit(execute())
    except KeyboardInterrupt:
        raise SystemExit(130) from None


if __name__ == "__main__":
    main()
