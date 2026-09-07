#!/usr/bin/env python3
"""Run detach_live.c's binary against an isolated real PTY server.

Usage: run_detach_live.py /absolute/phux /absolute/detach-live /scratch/root
Both binaries are explicit so validation cannot silently use an installed app.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def main():
    phux, probe, scratch = map(Path, sys.argv[1:])
    phux, probe = phux.resolve(), probe.resolve()
    with tempfile.TemporaryDirectory(prefix="detach-", dir=scratch) as directory:
        env = {key: value for key, value in os.environ.items() if not key.startswith("PHUX_")}
        env.update(HOME=directory, XDG_CONFIG_HOME=directory, XDG_STATE_HOME=directory,
                   XDG_DATA_HOME=directory, XDG_CACHE_HOME=directory, SHELL="/bin/sh")
        # Relative socket keeps Darwin's sockaddr_un path within its fixed bound.
        command = [str(phux), "--socket", "s"]
        with open(Path(directory) / "server.log", "w+") as log:
            server = subprocess.Popen(command + ["server"], cwd=directory, env=env,
                                      stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 10
                while not (Path(directory) / "s").exists():
                    if server.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError("isolated server failed to start")
                    time.sleep(0.02)
                subprocess.run(command + ["new", "--json", "-s", "detach-live", "--", "/bin/sh"],
                               cwd=directory, env=env, check=True, timeout=10)
                subprocess.run([str(probe), "s", "detach-live"], cwd=directory,
                               env=env, check=True, timeout=60)
            finally:
                # Explicit fixture session only; cleanup runs even on RED.
                subprocess.run(command + ["kill", "detach-live"], cwd=directory,
                               env=env, timeout=10, check=False, capture_output=True)
                server.terminate()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()
                log.seek(0)
                print(log.read(), file=sys.stderr)


if __name__ == "__main__":
    main()
