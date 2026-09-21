#!/usr/bin/python3
"""Explicit test-only helper: no Phux invocation and no socket/service creation."""
import os
import json
from pathlib import Path
import signal
import subprocess
import sys
import time

home = Path(__file__).parent
signal.signal(signal.SIGTERM, signal.SIG_IGN)
# Announce this helper before spawning a probe. `ready()` and argv checks
# must not wait on a second interpreter under CI load (phux-7v35).
with (home / "calls").open("a") as calls:
    calls.write("\n".join(sys.argv[1:]) + "\n")
(home / "pid.tmp").write_text(str(os.getpid()))
(home / "pid.tmp").replace(home / "pid")
if (home / "spawn-probe").exists():
    probe = subprocess.Popen(
        ["/bin/sleep", "60"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    (home / "probe-pid").write_text(str(probe.pid))
while not (home / "release").exists():
    time.sleep(0.01)
code = (home / "release").read_text()
(home / "exit-code").write_text(code)
if int(code) == 0:
    socket = sys.argv[2]
    print(json.dumps({
        "schema_version": 1,
        "running": True,
        "socket": socket,
        "disposition": "daemon_started",
        "cli_version": "test",
        "server_log": str(home / "server.log"),
    }))
sys.exit(int(code))
