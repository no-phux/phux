#!/usr/bin/env python3
"""Bounded cargo-mutants adapter; all mutations run in cargo-mutants scratch copies."""

import argparse
import json
import math
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

VERSION = "27.0.0"
ROOT = Path(__file__).resolve().parents[2]
HELP = """
Install with the repository's Rust 1.98 toolchain (no global overwrite):
  cargo install --locked cargo-mutants --version 27.0.0 \\
    --root target/mutation-tools/cargo-mutants-27.0.0
Alternatively set CARGO_MUTANTS_BIN to that version's cargo-mutants executable.
An exact-version executable on PATH is also accepted; nothing is auto-installed.

With no scope: phux-core, crates/phux-core/src/window.rs. With --file alone or
--in-diff alone: discover across workspace members, test only mutated packages.
--file is a cargo-mutants glob; --package is an exact package name (repeatable).
--in-diff REF uses git merge-base REF HEAD, then git diff BASE against the working
tree (tracked staged/unstaged edits included; untracked files are not in Git diffs).
Fetch the base ref/history in shallow CI first. Files/regex/package/diff intersect.
Test-only/deletion-only diffs may select nothing: this is not coverage evidence.

The deterministic first slice shard selects AT MOST --limit mutants, not a random
sample or full coverage. --list discovers and writes JSON without compiling.
Default: 8 mutants, 1 mutation worker, 2 compiler tasks, build 300s, test 60s,
each tool invocation 1800s. Increase limits explicitly for costly native packages.
Cargo dependencies may resolve across the workspace; only selected packages test.
Scratch copies have private target directories (no shared CARGO_TARGET_DIR), no
target-cache copying, and are removed by cargo-mutants. Registry/git download
caches are reused. This pays a cold package build per run/worker, not all crates.

Artifacts: unique target/mutation/rust-* directory, or NEW --output directory;
summary.json, files.json, candidates.json, selected.json, command logs, and raw
mutants.out/{outcomes.json,mutants.json,log/,diff/}. No score threshold.
Exit 0: completed findings (including survivors, unviable mutants and timeouts),
or listed/empty scope. Exit 1: tool/discovery/incomplete-run error. Exit 4: baseline
failed/timed out. Exit 2: CLI usage error. summary.json records status, counts,
baseline, raw tool exit, selection and commands. Timeouts need investigation;
only 'caught' means killed by tests. Unviable means compile failure, not a kill.
SIGTERM/SIGINT exit 143/130 with status 'interrupted'. Partial stdout/stderr stay
in command logs. Shutdown allows 5s for graceful cleanup, then kills tracked
descendants across process groups, checking PID/start-time identity via ps.

Rust cargo test is not a Rust->FFI->Zig test. For that evidence, each mutated
scratch checkout must rebuild phux-client-ffi (--profile ffi-dev or ffi-release)
and point the Zig host's -Dphux-client-ffi-lib-dir at THAT checkout's archive
directory (and -Dphux-client-ffi-include-dir at its header directory) before
running host tests. cargo-mutants 27 has cargo/nextest test tools, no arbitrary
post-build Zig command hook. This runner does not claim cross-language coverage.
"""


def positive(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return number


def arguments():
    parser = argparse.ArgumentParser(
        description=__doc__, epilog=HELP,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--file", action="append", default=[], help="source glob (repeatable)")
    parser.add_argument("--package", action="append", default=[], help="package (repeatable)")
    parser.add_argument("--in-diff", metavar="REF", help="changed lines since merge-base with REF")
    parser.add_argument("--re", help="cargo-mutants mutation-name regex")
    parser.add_argument("--features", help="Cargo features, comma-separated")
    parser.add_argument("--limit", type=positive, default=8)
    parser.add_argument("--jobs", type=positive, choices=range(1, 5), default=1)
    parser.add_argument("--build-timeout", type=positive, default=300)
    parser.add_argument("--test-timeout", type=positive, default=60)
    parser.add_argument("--run-timeout", type=positive, default=1800)
    parser.add_argument("--output", type=Path, help="new artifact directory (never overwrites)")
    parser.add_argument("--list", action="store_true", help="discovery only; do not build/test")
    parser.add_argument("--root", type=Path, default=ROOT, help="workspace root (also for fixtures)")
    return parser.parse_args()


def write_json(path, data):
    path.write_text(json.dumps(data, indent=2) + "\n")


class Interrupted(Exception):
    def __init__(self, signum):
        self.signum = signum
        super().__init__(f"interrupted by {signal.Signals(signum).name}")


def interrupt(signum, _frame):
    raise Interrupted(signum)


def process_snapshot():
    """Portable PID/PPID traversal; birth times prevent targeting reused PIDs."""
    output = subprocess.check_output(
        ["ps", "-axo", "pid=,ppid=,lstart="], text=True, timeout=1,
    )
    processes = {}
    for line in output.splitlines():
        pid, parent, birth = line.split(None, 2)
        processes[int(pid)] = (int(parent), birth)
    return processes


def living(owned, processes):
    return {pid for pid, birth in owned.items()
            if processes.get(pid, (None, None))[1] == birth}


def track_descendants(owned, processes):
    """Retain identities even after a known child is orphaned/reparented."""
    while True:
        parents = living(owned, processes)
        children = {pid: birth for pid, (parent, birth) in processes.items()
                    if parent in parents and pid not in owned}
        if not children:
            return
        owned.update(children)


def signal_process(pid, signum):
    try:
        os.kill(pid, signum)
    except ProcessLookupError:
        pass


def stop_invocation(child, report):
    """Allow cargo-mutants cleanup, then escalate across its owned child groups."""
    # Repeated termination signals must not interrupt cleanup or the final report.
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    processes = process_snapshot()
    owned = {}
    # An unreaped direct child retains its PID even if it has already exited.
    # Never adopt a PID after Popen has reaped it: it could belong to a new process.
    if child.returncode is None and child.pid in processes:
        owned[child.pid] = processes[child.pid][1]
    track_descendants(owned, processes)
    if child.pid in living(owned, processes):
        signal_process(child.pid, signal.SIGINT)
    deadline = time.monotonic() + 5
    while living(owned, processes) and time.monotonic() < deadline:
        child.poll()  # Reap the leader, including after early graceful exit.
        time.sleep(0.05)
        processes = process_snapshot()
        track_descendants(owned, processes)
    # Refresh immediately before escalation, including any late descendants.
    processes = process_snapshot()
    track_descendants(owned, processes)
    remaining = living(owned, processes)
    for pid in sorted(remaining, key=lambda pid: pid == child.pid):
        signal_process(pid, signal.SIGKILL)
    report["cleanup"] = {"tracked_pids": sorted(owned), "escalated_pids": sorted(remaining)}
    child.wait(timeout=1)


def invoke(command, cwd, output, label, timeout, report):
    """File-backed output survives interruption and cannot block pipe draining."""
    report["commands"].append(command)
    env = os.environ.copy()
    env.pop("CARGO_TARGET_DIR", None)
    env.pop("CARGO_BUILD_TARGET_DIR", None)
    stdout = output / (label + ".stdout.log")
    with stdout.open("w") as log, (output / (label + ".stderr.log")).open("w") as errors:
        child = subprocess.Popen(command, cwd=cwd, env=env, stdout=log,
                                 stderr=errors, text=True, start_new_session=True)
        try:
            child.wait(timeout=timeout)
        except (subprocess.TimeoutExpired, Interrupted):
            stop_invocation(child, report)
            raise
    return child.returncode, stdout.read_text()


def checked(command, args, output, label, report):
    code, stdout = invoke(command, args.root, output, label, args.run_timeout, report)
    if code:
        raise RuntimeError(f"{label} exited {code}; see {output}/{label}.stderr.log")
    return stdout


def tool(args, output, report):
    local = ROOT / f"target/mutation-tools/cargo-mutants-{VERSION}/bin/cargo-mutants"
    binary = os.environ.get("CARGO_MUTANTS_BIN") or shutil.which("cargo-mutants")
    if local.is_file():
        binary = os.environ.get("CARGO_MUTANTS_BIN", str(local))
    if not binary:
        raise RuntimeError("cargo-mutants missing; see --help for scoped installation")
    command = [binary, "mutants"]
    version = checked(command + ["--version"], args, output, "version", report).strip()
    if version != f"cargo-mutants {VERSION}":
        raise RuntimeError(f"expected cargo-mutants {VERSION}; found {version}")
    return command


def scope(args, output, report):
    flags = []
    if not (args.file or args.package or args.in_diff):
        args.package = ["phux-core"]
        args.file = ["crates/phux-core/src/window.rs"]
    if not args.package:
        flags.append("--workspace")
    for package in args.package:
        flags += ["--package", package]
    for file in args.file:
        flags += ["--file", file]
    for option in ("re", "features"):
        if getattr(args, option):
            flags += ["--" + option, getattr(args, option)]
    if args.in_diff:
        flags += diff_scope(args, output, report)
    return flags


def diff_scope(args, output, report):
    base = checked(["git", "merge-base", args.in_diff, "HEAD"], args, output,
                   "merge-base", report).strip()
    report["merge_base"] = base
    diff = checked(["git", "diff", "--no-ext-diff", "--no-textconv", "--no-renames",
                    "--src-prefix=a/", "--dst-prefix=b/", "--unified=0", base, "--"],
                   args, output, "git-diff", report)
    path = output / "scope.diff"
    path.write_text(diff)
    return ["--in-diff", str(path)]


def discover(command, args, output, report):
    files = checked(command + ["--list-files", "--json"], args, output, "files", report)
    write_json(output / "files.json", json.loads(files))
    raw = checked(command + ["--list", "--json"], args, output, "candidates", report)
    # cargo-mutants emits no JSON for an empty diff.
    candidates = json.loads(raw or "[]")
    write_json(output / "candidates.json", candidates)
    report["candidates"] = len(candidates)
    shards = max(1, math.ceil(len(candidates) / args.limit))
    command += ["--shard", f"0/{shards}", "--sharding", "slice"]
    raw = checked(command + ["--list", "--json"], args, output, "selected", report)
    selected = json.loads(raw or "[]")
    write_json(output / "selected.json", selected)
    if len(selected) > args.limit:
        raise RuntimeError("cargo-mutants shard exceeded requested limit")
    report["selected"] = len(selected)
    return command


def outcomes(output, code, report):
    report["tool_exit"] = code
    raw = json.loads((output / "mutants.out/outcomes.json").read_text())
    report["counts"] = {key: raw[key] for key in ("caught", "missed", "unviable", "timeout")}
    report["baseline"] = [row for row in raw["outcomes"] if row["scenario"] == "Baseline"]
    if code == 4:
        report["status"] = "baseline_failed"
        return 4
    if code not in (0, 2, 3):
        raise RuntimeError(f"cargo-mutants exited {code}")
    if sum(report["counts"].values()) != report["selected"]:
        raise RuntimeError("incomplete or unclassified mutation outcomes")
    if not report["baseline"] or report["baseline"][0]["summary"] != "Success":
        raise RuntimeError("missing successful baseline")
    report["status"] = "completed"
    return 0


def run(args, output, report):
    command = tool(args, output, report) + [
        "--no-config", "--no-shuffle", "--gitignore=true",
        "--test-workspace=false", "--test-tool=cargo", "--baseline=run",
        "--colors=never", "--annotations=none", "--cap-lints=true",
        "--cargo-arg=--locked", "--cargo-arg=--target-dir=target",
        "--jobs", str(args.jobs), "--jobserver-tasks", str(args.jobs * 2),
        "--build-timeout", str(args.build_timeout), "--timeout", str(args.test_timeout),
    ]
    command += scope(args, output, report)
    command = discover(command, args, output, report)
    if report["selected"] == 0:
        report["status"] = "empty_scope"
        return 0
    if args.list:
        report["status"] = "listed"
        return 0
    command += ["--output", str(output), "--caught", "--unviable"]
    code, stdout = invoke(command, args.root, output, "run", args.run_timeout, report)
    print(stdout, end="")
    return outcomes(output, code, report)


def main():
    args = arguments()
    args.root = args.root.resolve()
    if args.output:
        output = args.output.resolve()
        output.mkdir(parents=True, exist_ok=False)
    else:
        parent = args.root / "target/mutation"
        parent.mkdir(parents=True, exist_ok=True)
        output = Path(tempfile.mkdtemp(prefix="rust-", dir=parent))
    report = {"schema_version": 1, "tool_version": VERSION, "root": str(args.root),
              "status": "tool_error", "commands": [], "counts": {}, "baseline": []}
    code = 1
    signal.signal(signal.SIGTERM, interrupt)
    signal.signal(signal.SIGINT, interrupt)
    try:
        code = run(args, output, report)
    except Interrupted as error:
        report["status"] = "interrupted"
        report["error"] = str(error)
        code = 128 + error.signum
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
        report["status"] = "tool_error"
        report["error"] = str(error)
        print(f"error: {error}", file=sys.stderr)
    finally:
        write_json(output / "summary.json", report)
        print(f"Rust mutation report: {output}/summary.json")
    return code


if __name__ == "__main__":
    sys.exit(main())
