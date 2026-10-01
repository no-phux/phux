#!/usr/bin/env python3
"""Byte-level key echo latency probe for terminal multiplexers.

Runs the command under test on a real pty of a fixed size, proves that the
shell answers a readiness command, then measures one typed byte: write to the
master, then select() until that byte comes back out of the master. The
measurement ends at client output bytes, not a displayed GPU frame. Its floor
is the PTY and process under test rather than a screen-capture polling interval.

Stdlib only. Usage:

  pty-echo.py [options] -- CMD [ARGS...]

Options of note:
  --interfere-at N --interfere "CMD ARGS"  spawn a second client on its own pty
      at iteration N and keep draining it, to measure the echo spike a
      concurrent attach inflicts on the pane already being typed into.
"""

import argparse
import errno
import fcntl
import json
import os
import pty
import re
import select
import shlex
import signal
import struct
import subprocess
import sys
import termios
import time

QUIET_DRAIN_S = 0.02
DRAIN_CAP_S = 0.4

# A multiplexer paints one SGR-wrapped character per cell, so the prompt never
# appears as contiguous bytes on the wire. Strip the escape grammar and match
# against the visible text instead; hold back a trailing partial sequence so a
# read boundary never splits one.
ANSI_RE = re.compile(
    rb"\x1b\[[0-9;?<>=]*[ -/]*[@-~]"
    rb"|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)"
    rb"|\x1b[PX^_][^\x1b]*\x1b\\"
    rb"|\x1b[@-Z\\-_]"
)


def visible(raw, carry=b""):
    """Return (visible text, leftover raw) for a byte chunk."""
    raw = carry + raw
    cut = raw.rfind(b"\x1b")
    tail = b""
    if cut != -1 and not ANSI_RE.match(raw, cut):
        raw, tail = raw[:cut], raw[cut:]
    return ANSI_RE.sub(b"", raw), tail


def set_winsize(fd, rows, cols):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def spawn_pty(argv, cols, rows):
    """Fork argv onto its own controlling pty and return (pid, master_fd)."""
    master, slave = pty.openpty()
    set_winsize(slave, rows, cols)
    pid = os.fork()
    if pid == 0:
        os.setsid()
        try:
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        except OSError:
            pass
        os.dup2(slave, 0)
        os.dup2(slave, 1)
        os.dup2(slave, 2)
        os.close(master)
        if slave > 2:
            os.close(slave)
        try:
            os.execvp(argv[0], argv)
        finally:
            os._exit(127)
    os.close(slave)
    return pid, master


def read_ready(fds, timeout):
    try:
        return select.select(fds, [], [], timeout)[0]
    except (OSError, select.error):
        return []


def pump(fd, extra_fds, timeout):
    """Return whatever arrived on fd within timeout, draining extra_fds too."""
    out = b""
    ready = read_ready([fd] + extra_fds, timeout)
    for r in ready:
        try:
            chunk = os.read(r, 65536)
        except OSError as exc:
            if exc.errno in (errno.EIO, errno.EBADF):
                chunk = b""
            else:
                raise
        if r == fd:
            out += chunk
    return out


def wait_for(fd, needle, extra_fds, timeout_s):
    """Read until needle appears in the visible text; return (found, text)."""
    deadline = time.monotonic() + timeout_s
    text = b""
    carry = b""
    while time.monotonic() < deadline:
        chunk, carry = visible(pump(fd, extra_fds, 0.02), carry)
        text += chunk
        if needle in text:
            return True, text
    return False, text


def drain(fd, extra_fds):
    """Read until the stream goes quiet, so no stale byte is mistaken for echo."""
    stop = time.monotonic() + DRAIN_CAP_S
    while time.monotonic() < stop:
        if not pump(fd, extra_fds, QUIET_DRAIN_S):
            return


def settle(fd, extra_fds, token, timeout=2.5, attach_timeout=30.0):
    """Clear overlays and prove readiness; prompt text is not a readiness signal."""
    needle = ("READY_" + token).encode()
    deadline = time.monotonic() + attach_timeout
    for _ in range(8):
        if time.monotonic() >= deadline:
            break
        os.write(fd, b"\x1b")
        time.sleep(min(0.1, max(0, deadline - time.monotonic())))
        if time.monotonic() >= deadline:
            break
        os.write(fd, b"\x15")
        os.write(fd, b'echo R""EADY_' + token.encode() + b"\r")
        found, _ = wait_for(fd, needle, extra_fds, min(timeout, max(0, deadline - time.monotonic())))
        if found:
            os.write(fd, b"\x15")
            drain(fd, extra_fds)
            return True
        if time.monotonic() >= deadline:
            break
        os.write(fd, b"\r")
        time.sleep(min(0.2, max(0, deadline - time.monotonic())))
    return False


def percentile(values, p):
    if not values:
        return None
    ordered = sorted(values)
    return ordered[(p * (len(ordered) - 1) + 50) // 100]


def kill_quietly(pid):
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.kill(pid, sig)
        except OSError:
            return
        for _ in range(30):
            try:
                if os.waitpid(pid, os.WNOHANG)[0]:
                    return
            except OSError:
                return
            time.sleep(0.05)


def measure(args, fd, extra_fds):
    """One typed byte per iteration; returns per-iteration microseconds."""
    samples = []
    spikes = []
    timeouts = []
    interfere_pid = None
    for i in range(args.iters):
        if args.interfere and i == args.interfere_at:
            interfere_pid, ifd = spawn_pty(args.interfere, args.cols, args.rows)
            extra_fds.append(ifd)
        drain(fd, extra_fds)
        letter = bytes([97 + i % 26])
        carry = b""
        start = time.monotonic_ns()
        os.write(fd, letter)
        seen = False
        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline:
            chunk, carry = visible(pump(fd, extra_fds, 0.002), carry)
            if letter in chunk:
                seen = True
                break
        elapsed = (time.monotonic_ns() - start) // 1000
        if seen:
            samples.append(elapsed)
            if interfere_pid is not None and i >= args.interfere_at:
                spikes.append(elapsed)
        else:
            timeouts.append(i)
        os.write(fd, b"\x15")
    if interfere_pid is not None:
        kill_quietly(interfere_pid)
    return samples, spikes, timeouts


def capture_telemetry(command):
    """Retain a report or explicit collection failure; never discard echo samples."""
    try:
        completed = subprocess.run(command, capture_output=True, text=True, timeout=10)
        if completed.returncode:
            return {"error": "telemetry command failed", "returncode": completed.returncode,
                    "stderr": completed.stderr}
        return json.loads(completed.stdout)
    except (OSError, subprocess.TimeoutExpired, json.JSONDecodeError) as exc:
        return {"error": str(exc)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cols", type=int, default=120)
    ap.add_argument("--rows", type=int, default=40)
    ap.add_argument("--iters", type=int, default=60)
    ap.add_argument("--label", default="probe")
    ap.add_argument("--json", dest="json_out")
    ap.add_argument("--attach-timeout", type=float, default=30.0)
    ap.add_argument("--settle-timeout", type=float, default=2.5)
    ap.add_argument("--telemetry-command",
                    help="JSON-report command to run immediately before and after the measured phase")
    ap.add_argument("--interfere-at", type=int, default=-1)
    ap.add_argument("--interfere", help="second client command line, run on its own pty")
    ap.add_argument("cmd", nargs=argparse.REMAINDER)
    args = ap.parse_args()

    argv = args.cmd
    if argv and argv[0] == "--":
        argv = argv[1:]
    if not argv:
        print("no command given", file=sys.stderr)
        return 2
    if args.iters <= 0 or args.cols <= 0 or args.rows <= 0:
        ap.error("iters, cols and rows must be positive")
    args.interfere = shlex.split(args.interfere) if args.interfere else None
    args.telemetry_command = shlex.split(args.telemetry_command) if args.telemetry_command else None

    token = "P%d" % os.getpid()
    pid, fd = spawn_pty(argv, args.cols, args.rows)
    extra_fds = []
    result = {
        "label": args.label, "iters": args.iters, "ok": False,
        "command": argv, "boundary": "outer PTY write to attach-client output byte",
        "cols": args.cols, "rows": args.rows,
    }
    try:
        ready_started = time.monotonic()
        ready = settle(fd, extra_fds, token, args.settle_timeout, args.attach_timeout)
        result["readiness_elapsed_ms"] = (time.monotonic() - ready_started) * 1000
        if not ready:
            result["error"] = "pane never answered a probe command"
        else:
            if args.telemetry_command:
                result["telemetry_before"] = capture_telemetry(args.telemetry_command)
            samples, spikes, timeouts = measure(args, fd, extra_fds)
            result.update(
                ok=len(samples) == args.iters,
                completed=len(samples),
                timeouts=len(timeouts),
                timeout_iterations=timeouts,
                samples_us=samples,
                p50_us=percentile(samples, 50),
                p90_us=percentile(samples, 90),
                p99_us=percentile(samples, 99) if len(samples) >= 1000 else None,
                max_us=max(samples) if samples else None,
            )
            if args.telemetry_command:
                result["telemetry_after"] = capture_telemetry(args.telemetry_command)
            if spikes:
                result.update(
                    interference_max_us=max(spikes),
                    interference_p50_us=percentile(spikes, 50),
                    interference_samples_us=spikes,
                )
    finally:
        kill_quietly(pid)
        for extra in extra_fds:
            try:
                os.close(extra)
            except OSError:
                pass
        try:
            os.close(fd)
        except OSError:
            pass

    text = json.dumps(result)
    if args.json_out:
        with open(args.json_out, "w") as handle:
            handle.write(text + "\n")
    print(text)
    return 0 if result.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
