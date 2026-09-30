#!/usr/bin/env python3
"""Observe native cmux only on a disposable GitHub-hosted macOS runner."""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import signal
import statistics
import subprocess
import time
import uuid


def command(argv, *, environment=None, timeout=15):
    result = subprocess.run(argv, env=environment, capture_output=True, text=True,
                            timeout=timeout, check=False)
    return {"argv": [str(item) for item in argv], "exit_code": result.returncode,
            "stdout": result.stdout.strip(), "stderr": result.stderr.strip()}


def process_rows():
    rows = []
    for line in command(["/bin/ps", "-axo", "pid=,ppid=,rss=,comm="])["stdout"].splitlines():
        pid, parent, rss, executable = line.strip().split(None, 3)
        rows.append({"pid": int(pid), "parent": int(parent), "rss_kib": int(rss),
                     "executable": executable})
    return rows


def owned_memory(pid, app):
    rows = process_rows()
    owned = {pid} | {row["pid"] for row in rows if row["executable"].startswith(str(app))}
    while True:
        children = {row["pid"] for row in rows if row["parent"] in owned}
        expanded = owned | children
        if expanded == owned:
            break
        owned = expanded
    selected = [row for row in rows if row["pid"] in owned]
    return {"processes": selected, "sum_rss_kib": sum(row["rss_kib"] for row in selected),
            "scope": "app, bundled helper roots, and their current descendants; excludes shared WindowServer, GPU allocations and unattributed reparented helpers; RSS may double-count shared pages"}


def echo_command(cli, environment, timeout=10):
    marker = "CMUX_BENCH_" + uuid.uuid4().hex
    shell = "printf '%s%s\\n' 'CMUX_BENCH_' '" + marker.removeprefix("CMUX_BENCH_") + "'\n"
    started = time.perf_counter_ns()
    sent = command([cli, "send", shell], environment=environment)
    if sent["exit_code"] != 0:
        return {"ok": False, "error": "send failed", "send": sent}
    deadline = time.monotonic() + timeout
    reads = 0
    while time.monotonic() < deadline:
        read = command([cli, "read-screen"], environment=environment)
        reads += 1
        if read["exit_code"] == 0 and marker in read["stdout"]:
            return {"ok": True, "elapsed_us": (time.perf_counter_ns() - started) // 1000,
                    "read_calls": reads}
        time.sleep(0.01)
    return {"ok": False, "error": "completion marker not observed", "last_read": read}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted" or platform.system() != "Darwin":
        parser.error("requires a disposable GitHub-hosted macOS runner; never launch on the user's desktop")
    app = args.app.resolve()
    executable = app / "Contents/MacOS/cmux"
    cli = app / "Contents/Resources/bin/cmux"
    if any(Path(row["executable"]).name in {"cmux", "cmuxd"} for row in process_rows()):
        parser.error("a cmux process already exists; refusing to reuse it")
    args.out.mkdir(parents=True, exist_ok=False)
    environment = {key: os.environ[key] for key in ("HOME", "USER", "LOGNAME", "TMPDIR") if key in os.environ}
    environment.update(PATH="/usr/bin:/bin:/usr/sbin:/sbin", LANG="en_US.UTF-8",
                       CMUX_ALLOW_SOCKET_OVERRIDE="1", CMUX_SOCKET_PATH="/tmp/phux-cmux-bench.sock",
                       CMUXD_UNIX_PATH="/tmp/phux-cmuxd-bench.sock")
    with (app / "Contents/Info.plist").open("rb") as source:
        info = plistlib.load(source)
    result = {
        "recorded_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "runner": {key: os.environ.get(key) for key in ("RUNNER_OS", "RUNNER_ARCH", "RUNNER_ENVIRONMENT", "ImageOS", "ImageVersion", "GITHUB_RUN_ID", "GITHUB_SHA")},
        "hardware": {"platform": platform.platform(), "cpu": command(["/usr/sbin/sysctl", "-n", "machdep.cpu.brand_string"]),
                     "memory": command(["/usr/sbin/sysctl", "-n", "hw.memsize"]), "load_start": os.getloadavg()},
        "release": {"version": info.get("CFBundleShortVersionString"), "build": info.get("CFBundleVersion"),
                    "binary_sha256": hashlib.file_digest(executable.open("rb"), "sha256").hexdigest(),
                    "cli": command([cli, "--version"], environment=environment)},
        "configuration": {"state": "fresh disposable runner account; default native window and terminal", "samples_requested": 20,
                          "startup_boundary": "LaunchServices open invocation to first observed completed terminal command; includes CLI startup and polling",
                          "echo_boundary": "CLI send invocation to completion marker returned by CLI read-screen; includes both CLI launches and polling; NOT PTY-byte echo or input-to-pixel"},
        "samples": [], "ok": False,
    }
    result["display"] = command(["/usr/sbin/system_profiler", "SPDisplaysDataType", "-json"], timeout=30)
    result["initial_screenshot"] = command(["/usr/sbin/screencapture", "-x", args.out / "before.png"])
    result["signature"] = command(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(app)])
    try:
        started = time.perf_counter_ns()
        launch = ["/usr/bin/open", "-n", str(app)]
        for key in ("CMUX_ALLOW_SOCKET_OVERRIDE", "CMUX_SOCKET_PATH", "CMUXD_UNIX_PATH"):
            launch.extend(["--env", f"{key}={environment[key]}"])
        result["launch"] = command(launch, environment=environment)
        if result["launch"]["exit_code"] != 0:
            raise RuntimeError("LaunchServices refused the application")
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            workspaces = command([cli, "list-workspaces", "--json"], environment=environment, timeout=5)
            if workspaces["exit_code"] == 0 and workspaces["stdout"] not in ("", "[]"):
                break
            time.sleep(0.1)
        else:
            raise RuntimeError(f"workspace readiness failed: {workspaces}")
        result["workspaces"] = workspaces
        first = echo_command(cli, environment)
        result["first_command"] = first
        if not first["ok"]:
            raise RuntimeError("initial terminal did not complete the readiness command")
        result["launch_to_first_command_us"] = (time.perf_counter_ns() - started) // 1000
        for _ in range(20):
            result["samples"].append(echo_command(cli, environment))
        values = [sample["elapsed_us"] for sample in result["samples"] if sample["ok"]]
        result["completed"] = len(values)
        result["failures"] = 20 - len(values)
        result["median_us"] = statistics.median(values) if values else None
        result["max_us"] = max(values) if values else None
        pid = next((row["pid"] for row in process_rows() if row["executable"] == str(executable)), None)
        if pid is None:
            raise RuntimeError("application process unavailable for memory observation")
        result["memory"] = owned_memory(pid, app)
        result["screenshot"] = command(["/usr/sbin/screencapture", "-x", args.out / "screen.png"])
        result["ok"] = len(values) == 20
    except (RuntimeError, subprocess.TimeoutExpired, OSError) as error:
        result["error"] = str(error)
    finally:
        for row in process_rows():
            if row["executable"].startswith(str(app) + "/"):
                try:
                    os.kill(row["pid"], signal.SIGTERM)
                except ProcessLookupError:
                    pass
        reports = Path.home() / "Library/Logs/DiagnosticReports"
        for report in list(reports.glob("cmux*.ips")) + list(reports.glob("cmux*.crash")):
            (args.out / report.name).write_bytes(report.read_bytes())
        (args.out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
