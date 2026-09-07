#!/usr/bin/python3
"""Explicit test-only helper: no Phux invocation and no socket/service creation."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

home = Path(__file__).parent
signal.signal(signal.SIGTERM, signal.SIG_IGN)
if (home / "spawn-probe").exists():
    probe = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
    (home / "probe-pid").write_text(str(probe.pid))
with (home / "calls").open("a") as calls:
    calls.write("\n".join(sys.argv[1:]) + "\n")
(home / "pid.tmp").write_text(str(os.getpid()))
(home / "pid.tmp").replace(home / "pid")
while not (home / "release").exists():
    time.sleep(0.01)
code = (home / "release").read_text()
(home / "exit-code").write_text(code)
sys.exit(int(code))
