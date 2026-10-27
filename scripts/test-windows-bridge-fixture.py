#!/usr/bin/env python3
"""Owned, deliberately unread Unix socket for native Windows/WSL qualification."""

import array
import fcntl
import json
import os
from pathlib import Path
import signal
import socket
import struct
import sys
import termios
import time


def process_identity(pid):
    """PID plus start tick protects cleanup and observation against PID reuse."""
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
        return {"start": fields[19], "alive": fields[0] != "Z"}
    except FileNotFoundError:
        return {"start": None, "alive": False}


def publish(root, status):
    staged = root / "status.new"
    staged.write_text(json.dumps(status))
    staged.replace(root / "status.json")


def terminate(_signum, _frame):
    raise SystemExit(0)


def serve(root):
    signal.signal(signal.SIGTERM, terminate)
    status = {"serverPid": os.getpid(), "serverStart": process_identity(os.getpid())["start"]}
    with socket.socket(socket.AF_UNIX) as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
        listener.bind(str(root / "server.sock"))
        listener.listen(1)
        publish(root, status)
        peer, _ = listener.accept()
        with peer:
            pid, uid, _ = struct.unpack("3i", peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            if uid != os.getuid() or Path(f"/proc/{pid}/exe").resolve() != root / "phux":
                raise RuntimeError("Only this fixture's same-source bridge may connect")
            identity = process_identity(pid)
            status.update(bridgePid=pid, bridgeStart=identity["start"], accepted=True)
            while True:
                # Kernel queue observation is NOT a read: keep the peer blocked.
                pending = array.array("i", [0])
                fcntl.ioctl(peer, termios.FIONREAD, pending, True)
                current = process_identity(pid)
                status.update(
                    queuedBytes=pending[0],
                    bridgeAlive=current["alive"] and current["start"] == identity["start"],
                )
                publish(root, status)
                time.sleep(0.1)


def cleanup(root):
    status_file = root / "status.json"
    if not status_file.exists():
        return
    status = json.loads(status_file.read_text())
    for role in ("bridge", "server"):
        pid = status.get(role + "Pid")
        if pid is None:
            continue
        identity = process_identity(pid)
        if identity["alive"] and identity["start"] == status[role + "Start"]:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass


if __name__ == "__main__":
    mode, directory = sys.argv[1:]
    root = Path(directory)
    if mode == "serve":
        serve(root)
    elif mode == "cleanup":
        cleanup(root)
    else:
        raise ValueError(f"Unknown fixture mode: {mode}")
