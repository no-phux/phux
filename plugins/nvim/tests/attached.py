#!/usr/bin/env python3
"""Drives the plugin through a real attached client in a pty, on a throwaway
server, so keys and pane sizes take the TUI's path.
Usage: tests/attached.py [path to phux]."""

import json
import os
import pty
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import fcntl
import termios
import time

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(HERE, "target/release/phux-nvim")
PHUX = sys.argv[1] if len(sys.argv) > 1 else shutil.which("phux")

work = tempfile.mkdtemp(prefix="pa.")
sock = os.path.join(work, "s")
env = {k: v for k, v in os.environ.items() if not k.startswith(("PHUX_", "TMUX"))}
env.update(PHUX_SOCKET=sock, XDG_RUNTIME_DIR=work, TERM="xterm-256color")
shim = os.path.join(work, "shim")
os.mkdir(shim)
os.symlink(PHUX, os.path.join(shim, "phux"))
env["PATH"] = shim + ":" + env["PATH"]


def phux(*args):
    return subprocess.run(
        [PHUX, *args], env=env, capture_output=True, text=True, timeout=10
    )


def live():
    doc = json.loads(phux("ls", "--json").stdout)
    return [r["id"] for r in doc["resources"] if r["lifecycle"] == "running"]


server = subprocess.Popen(
    [PHUX, "server", "--socket", sock, "--session", "t"],
    env=env,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
)
for _ in range(100):
    if os.path.exists(sock):
        break
    time.sleep(0.05)

pid, tty = pty.fork()
if pid == 0:
    os.execve(PHUX, [PHUX, "attach", "t"], env)
fcntl.ioctl(tty, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 160, 0, 0))


def pump(seconds):
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([tty], [], [], 0.05)
        if r:
            try:
                data = os.read(tty, 65536)
            except OSError:
                return
            # answer the client's device-attributes query so it starts
            if b"\x1b[c" in data:
                os.write(tty, b"\x1b[?62;22c")


def type_keys(text):
    os.write(tty, text)
    pump(0.4)


def wait(pred, why):
    for _ in range(100):
        pump(0.05)
        if pred():
            print("ok  ", why)
            return True
    print("FAIL", why)
    return False


fails = 0
try:
    pump(1.0)
    open(os.path.join(work, "a.txt"), "w").write("alpha\n")
    first = live()[0]
    type_keys(f"cd '{work}' && '{BIN}' --clean a.txt\r".encode())
    fails += not wait(
        lambda: "alpha" in phux("snapshot", first).stdout, "opens through the client"
    )
    type_keys(b":vsplit\r")
    fails += not wait(lambda: len(live()) == 2, ":vsplit opens a pane")
    new = [p for p in live() if p != first][0]
    size = phux("snapshot", new).stdout.splitlines()[-1]
    print("     new pane:", size)
    fails += not wait(
        lambda: " 7" in phux("snapshot", new).stdout.splitlines()[-1]
        or "x39" in phux("snapshot", new).stdout.splitlines()[-1]
        or "x38" in phux("snapshot", new).stdout.splitlines()[-1],
        "the client sized it",
    )
    fails += not wait(
        lambda: "alpha" in phux("snapshot", new).stdout, "the client's size reaches it"
    )
    type_keys(b"Oclient typed\x1b")
    fails += not wait(
        lambda: "client typed" in phux("snapshot", new).stdout,
        "keys typed in the client reach Neovim's new window",
    )
    type_keys(b"\x17")
    type_keys(b"w")
    type_keys(b"Omoved\x1b")
    fails += not wait(
        lambda: "moved" in phux("snapshot", first).stdout, "Ctrl-W w through the client"
    )
    type_keys(b":qa!\r")
    fails += not wait(lambda: live() == [first], "quitting closes the spawned pane")
    fails += not wait(
        lambda: "alpha" not in phux("snapshot", first).stdout, "and restores the shell"
    )
finally:
    os.close(tty)
    os.kill(pid, 9)
    os.waitpid(pid, 0)
    server.terminate()
    server.wait()
    shutil.rmtree(work, ignore_errors=True)
sys.exit(fails)
