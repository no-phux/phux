#!/usr/bin/env python3
"""Record benchmark provenance, never connect to a multiplexer server."""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile


def command_output(argv, environment):
    result = subprocess.run(
        argv, env=environment, capture_output=True, text=True, timeout=15,
        check=False,
    )
    return {"argv": argv, "exit_code": result.returncode,
            "stdout": result.stdout.strip(), "stderr": result.stderr.strip()}


def sha256(path):
    with open(path, "rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def binary_info(path, flag, environment):
    resolved = Path(path).resolve()
    return {"path": str(resolved), "sha256": sha256(resolved),
            "version": command_output([str(resolved), flag], environment)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("out", "repo", "phux", "herdr", "tmux", "lanes"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--config", type=json.loads, required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    lanes = args.lanes.split(",")
    with tempfile.TemporaryDirectory(prefix="mux-metadata-") as home:
        environment = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": home,
                       "XDG_CONFIG_HOME": home, "XDG_STATE_HOME": home,
                       "XDG_DATA_HOME": home, "XDG_CACHE_HOME": home,
                       "PHUX_PROFILE": "muxbench-metadata"}
        binaries = {"tmux": binary_info(args.tmux, "-V", environment)}
        for name in ("phux", "herdr"):
            if any(lane.startswith(name) for lane in lanes):
                binaries[name] = binary_info(getattr(args, name), "--version", environment)
        hardware = {"system": platform.platform(), "machine": platform.machine(),
                    "logical_cpus": os.cpu_count(), "load_average": os.getloadavg()}
        if platform.system() == "Darwin":
            hardware["cpu"] = command_output(["/usr/sbin/sysctl", "-n", "machdep.cpu.brand_string"], environment)
            hardware["memory_bytes"] = command_output(["/usr/sbin/sysctl", "-n", "hw.memsize"], environment)
            hardware["os"] = command_output(["/usr/bin/sw_vers"], environment)
        result = {
            "schema_version": 1,
            "recorded_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "command": args.command[1:] if args.command[:1] == ["--"] else args.command,
            "lanes": lanes, "hardware": hardware, "binaries": binaries,
            "harness_revision": command_output(["git", "-C", args.repo, "rev-parse", "HEAD"], environment),
            "harness_sha256": {name: sha256(Path(args.repo) / "scripts" / "bench" / name)
                               for name in ("mux-compare.sh", "pty-echo.py", "metadata.py")},
            "configuration": {
                **args.config,
                "terminal": "xterm-256color", "locale": "en_US.UTF-8",
                "shell": "/bin/sh", "outer_geometry": [120, 40],
                "big_history_geometry": [188, 40], "big_history_panes": 4,
                "big_history_lines_per_pane": 60000,
                "history_policy": "product defaults; equal input does not imply equal retained history",
                "isolation": "env -i; private HOME/XDG/socket per lane; tmux -f /dev/null",
                "memory_scope": "individual server and attach-client RSS; excludes shells, observer, GUI and GPU",
                "echo_boundary": "outer PTY write to attach-client output byte, not input-to-pixel",
                "percentile_method": "sorted sample at round(p*(n-1)/100); p99 only for n>=1000",
            },
        }
    with open(args.out, "x") as handle:
        json.dump(result, handle, indent=2)
        handle.write("\n")


if __name__ == "__main__":
    main()
