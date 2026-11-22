#!/usr/bin/env python3
"""Capture raw PTY bytes from an interactive command.

Usage: pty_capture.py <output-file> <duration-seconds> <cmd> [args...]

Spawns <cmd> attached to a real pty (so it believes it is an interactive
terminal), sets a stable window size, and tees every byte the child writes
to <output-file> for <duration-seconds>. After a short warmup it sends a
single line of input (READ from stdin of this script, if any is piped in
via the PTY_CAPTURE_INPUT env var) to trigger a work turn.

This is a research tool for phux-w7z2.15: it exists to answer whether an
agent CLI emits OSC 9;4, not to be a general recording utility.
"""
import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import time

def set_winsize(fd, rows=40, cols=120):
    winsize = struct.pack("HHHH", rows, cols, 0, 0)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, winsize)

def main():
    out_path = sys.argv[1]
    duration = float(sys.argv[2])
    cmd = sys.argv[3:]
    input_line = os.environ.get("PTY_CAPTURE_INPUT")
    input_delay = float(os.environ.get("PTY_CAPTURE_INPUT_DELAY", "3"))
    input_line2 = os.environ.get("PTY_CAPTURE_INPUT2")
    input_delay2 = float(os.environ.get("PTY_CAPTURE_INPUT_DELAY2", "0"))

    pid, master_fd = pty.fork()
    if pid == 0:
        # Child
        env = os.environ.copy()
        env["TERM"] = "xterm-256color"
        os.execvpe(cmd[0], cmd, env)
        os._exit(1)

    set_winsize(master_fd)
    start = time.time()
    sent_input = input_line is None
    sent_input2 = input_line2 is None
    with open(out_path, "wb") as out:
        while time.time() - start < duration:
            if not sent_input and (time.time() - start) >= input_delay:
                os.write(master_fd, input_line.encode() + b"\r")
                sent_input = True
            if not sent_input2 and sent_input and (time.time() - start) >= input_delay2:
                os.write(master_fd, input_line2.encode() + b"\r")
                sent_input2 = True
            # select() with a short timeout, NOT a blocking read: a quiet
            # CLI (e.g. codex sitting at a trust prompt with no periodic
            # redraw) would otherwise starve the staged-input timer above,
            # since a blocking read only returns to the loop top when the
            # child produces output.
            ready, _, _ = select.select([master_fd], [], [], 0.2)
            if not ready:
                continue
            try:
                r = os.read(master_fd, 65536)
            except OSError:
                break
            if not r:
                break
            out.write(r)
            out.flush()
        # done
    try:
        os.kill(pid, signal.SIGTERM)
        time.sleep(0.3)
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    try:
        os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        pass

if __name__ == "__main__":
    main()
