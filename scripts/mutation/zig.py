#!/usr/bin/env python3
"""Scope/limits/artifacts adapter around Zentinel's AST engine and test runner.

Zentinel generates, patches, baselines, executes and classifies every mutant.
Its CLI has no count limit: select its first N stable IDs and invoke --mutant
serially. Each invocation baselines again and uses --no-cache. Own the process
group because upstream std.process.run only kills the immediate child on timeout.
"""

import argparse
import hashlib
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PILOT = ROOT / "clients/cockpit/mutation/pilot.json"
DIAGNOSTIC_OUTCOMES = ("killed", "survived", "compile_error", "timeout")
REPORT_OUTCOMES = (*DIAGNOSTIC_OUTCOMES, "invalid", "compiler_crash", "skipped")


def write_json(path, data):
    path.write_text(json.dumps(data, indent=2) + "\n")


def kill_group(pid):
    try:
        os.killpg(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    except PermissionError:
        # macOS can return EPERM for an already emptied process group. Only
        # tolerate it after checking that no running member remains.
        output = subprocess.check_output(["ps", "-eo", "pgid=,stat="], text=True)
        members = [line.split() for line in output.splitlines()]
        if any(int(group) == pid and not state.startswith("Z") for group, state in members):
            raise


def process_snapshot():
    """PID identity includes start time so cleanup does not target a reused PID."""
    output = subprocess.check_output(["ps", "-eo", "pid=,ppid=,pgid=,lstart="], text=True)
    result = {}
    for line in output.splitlines():
        pid, parent, group, started = line.split(maxsplit=3)
        result[int(pid)] = (int(parent), int(group), started)
    return result


def remember_descendants(owned, snapshot):
    parents = {pid for pid, started in owned.items()
               if pid in snapshot and snapshot[pid][2] == started}
    pending = True
    while pending:
        pending = False
        for pid, (parent, _, started) in snapshot.items():
            if parent in parents and pid not in parents:
                owned[pid] = started
                parents.add(pid)
                pending = True


def stop_descendants(child, owned):
    snapshot = process_snapshot()
    remember_descendants(owned, snapshot)
    # Kill rather than merely signal TERM: mutants may deliberately ignore TERM.
    # The launch session owns these groups; detached descendants observed during
    # execution remain owned even after their immediate parent has exited.
    groups = {child.pid}
    for pid, started in owned.items():
        current = snapshot.get(pid)
        if current is None or current[2] != started:
            continue
        if current[1] != os.getpgrp():
            groups.add(current[1])
    for group in groups:
        kill_group(group)


def run_command(argv, cwd, timeout, log, env=None):
    """Reap the launch group and observed descendants on return or cancellation.

    This is process cleanup, not an OS sandbox: an immediate double-fork into a
    new session can escape observation, as can SIGKILL of this controller.
    Zentinel and the pilot's ordinary `zig test` commands do not daemonize.
    """
    owned = {}
    with log.open("w") as output:
        child = subprocess.Popen(argv, cwd=cwd, env=env, stdout=output,
                                 stderr=subprocess.STDOUT, start_new_session=True)
        try:
            snapshot = process_snapshot()
            if child.pid in snapshot:
                owned[child.pid] = snapshot[child.pid][2]
            deadline = time.monotonic() + timeout
            while child.poll() is None:
                remember_descendants(owned, process_snapshot())
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise subprocess.TimeoutExpired(argv, timeout)
                try:
                    return child.wait(timeout=min(0.1, remaining))
                except subprocess.TimeoutExpired:
                    continue
            return child.returncode
        finally:
            try:
                stop_descendants(child, owned)
            finally:
                kill_group(child.pid)
                child.wait()


def positive(value):
    result = int(value)
    if result < 1:
        raise argparse.ArgumentTypeError("must be positive")
    return result


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    scope = parser.add_mutually_exclusive_group()
    scope.add_argument("--scope", action="append", metavar="FILE",
                       help="repository-relative pilot module (repeatable)")
    scope.add_argument("--diff", metavar="REF", help="changed pilot files vs commit REF, including working-tree edits")
    parser.add_argument("--out", type=Path, default=ROOT / "target/mutation/zig",
                        help="new or empty artifact directory")
    parser.add_argument("--max-mutants", type=positive, default=8,
                        help="first N Zentinel-generated IDs, serially (default 8, maximum 100)")
    parser.add_argument("--timeout", type=positive, default=60,
                        help="seconds per baseline/mutant test command (default 60)")
    parser.add_argument("--list", action="store_true", help="list generated candidates without testing")
    args = parser.parse_args()
    if args.max_mutants > 100:
        parser.error("--max-mutants must be <= 100")
    return args


def changed_files(ref):
    commit = subprocess.check_output(
        ["git", "rev-parse", "--verify", "--end-of-options", ref + "^{commit}"],
        cwd=ROOT, text=True).strip()
    output = subprocess.check_output(
        ["git", "diff", "--name-only", "-z", "--diff-filter=ACMR", commit, "--"], cwd=ROOT)
    return output.decode().rstrip("\0").split("\0")


def select_targets(args, modules):
    requested = modules
    if args.diff:
        requested = changed_files(args.diff)
    elif args.scope:
        requested = args.scope
        unsupported = sorted(set(requested) - set(modules))
        if unsupported:
            raise ValueError(f"outside standalone pilot scope: {unsupported}; supported: {modules}")
    return sorted(set(requested) & set(modules))


def prepare_output(path):
    path = path.resolve()
    path.mkdir(parents=True, exist_ok=True)
    if any(path.iterdir()):
        raise ValueError(f"artifact directory must be empty: {path}")
    return path


def prepare_project(stage, targets):
    hashes = {}
    for target in targets:
        source = ROOT / target
        if source.is_symlink() or not source.resolve().is_relative_to(ROOT):
            raise ValueError(f"source must stay inside checkout: {target}")
        destination = stage / target
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        hashes[target] = hashlib.sha256(destination.read_bytes()).hexdigest()
    return hashes


def configuration(stage, targets, operators, timeout):
    # This pilot uses module-local tests from verbatim shipping source. No fake
    # import root, SDK substitutes, generated product code or test filtering.
    commands = [f"zig test {target}" for target in targets]
    text = f'''[project]
name = "cockpit-standalone-module-pilot"
root = "."
include = {json.dumps(targets)}
exclude = [".zig-cache/**", "zig-out/**"]
[zig]
version = "0.16.0"
modes = ["Debug"]
[backend]
default = "ast"
[mutators]
enabled = {json.dumps(operators)}
[test]
commands = {json.dumps(commands)}
selection = "all"
timeout_ms = {timeout * 1000}
baseline_required = true
[run]
jobs = 1
[cache]
enabled = false
[report]
output_dir = ".zig-cache/report"
formats = ["json"]
'''
    (stage / "zentinel.toml").write_text(text)


def tool_binary(out):
    installer = Path(__file__).with_name("zig-tool.py")
    log = out / "tool-install.log"
    # Parent owns this directory even if the installer must be force-killed.
    with tempfile.TemporaryDirectory(prefix="phux-zig-tool-") as temporary:
        code = run_command([sys.executable, str(installer), temporary], ROOT, 660, log)
    if code:
        raise ValueError(f"tool installation failed; see {out / 'tool-install.log'}")
    return log.read_text().splitlines()[-1]


def list_candidates(binary, stage, out, env):
    log = out / "candidates.json"
    code = run_command([binary, "list-mutants", "--format", "json"], stage, 60, log, env)
    if code:
        raise ValueError(f"candidate generation failed ({code}); see {log}")
    return json.loads(log.read_text())["mutants"]


def report_object(value, label):
    if not isinstance(value, dict):
        raise TypeError(f"Zentinel {label} must be an object")
    return value


def validate_report_header(report):
    report_object(report, "report")
    if report.get("schema_version") != "zentinel.report.v1":
        raise ValueError("unsupported or missing Zentinel report schema")
    baseline = report_object(report.get("baseline"), "baseline")
    if baseline.get("status") != "passed":
        raise ValueError("baseline failed or missing")
    run = report_object(report.get("run"), "run")
    if run.get("status") != "completed":
        raise ValueError("Zentinel run did not complete")
    if run.get("error", "missing") is not None:
        raise ValueError("Zentinel completed run must have a null error")


def selected_outcome(report, mutant_id):
    entries = report.get("mutants")
    if not isinstance(entries, list) or len(entries) != 1:
        raise ValueError("Zentinel must report exactly one selected mutant")
    entry = report_object(entries[0], "mutant")
    if entry.get("id") != mutant_id:
        raise ValueError(f"Zentinel report does not match selected mutant {mutant_id}")
    result = report_object(entry.get("result"), "mutant result")
    status = result.get("status")
    if status not in DIAGNOSTIC_OUTCOMES:
        raise ValueError(f"non-diagnostic or missing Zentinel outcome: {status!r}")
    return status


def validate_report_summary(report, status):
    summary = report_object(report.get("summary"), "summary")
    expected = dict.fromkeys(REPORT_OUTCOMES, 0)
    expected.update(total=1)
    expected[status] = 1
    if any(type(count) is not int for count in summary.values()):
        raise ValueError("Zentinel summary counts must be integers")
    if summary != expected:
        raise ValueError("Zentinel summary must account exactly for the selected mutant")


def validate_report(report, mutant_id, report_path):
    # Validate the pinned outcome contract before accepting an exit-0 run. The
    # upstream tool also exits 0 for operational failures and unmatched IDs.
    try:
        validate_report_header(report)
        status = selected_outcome(report, mutant_id)
        validate_report_summary(report, status)
    except (TypeError, ValueError) as error:
        raise ValueError(f"{error}; see {report_path}") from error


def execute_mutant(binary, stage, mutant, index, timeout, out, env):
    report_path = out / f"mutant-{index:03d}.json"
    relative_report = f".zig-cache/report/mutant-{index:03d}.json"
    argv = [binary, "run", "--mutant", mutant["id"], "--jobs", "1", "--no-cache",
            "--output", relative_report]
    code = run_command(argv, stage, 2 * timeout + 30, out / f"mutant-{index:03d}.log", env)
    if (stage / relative_report).is_file():
        shutil.copy2(stage / relative_report, report_path)
    if not report_path.is_file():
        raise ValueError(f"Zentinel exited {code} without report for {mutant['id']}")
    report = json.loads(report_path.read_text())
    validate_report(report, mutant["id"], report_path)
    if code:
        raise ValueError(f"Zentinel failed ({code}); see {report_path}")
    return report


def aggregate_outcomes(reports, selected_count):
    outcomes = Counter(item["result"]["status"] for report in reports for item in report["mutants"])
    if sum(outcomes.values()) != selected_count:
        raise ValueError("Zentinel outcome count does not match selected mutant count")
    return dict(outcomes)


def execute_project(binary, stage, out, maximum, timeout, list_only=False):
    # Both compiler caches are private; the cwd-relative global cache also
    # resolves separately in every mutant sandbox. No cached mutant verdicts.
    env = dict(os.environ, ZIG_GLOBAL_CACHE_DIR=".zig-cache/global")
    candidates = list_candidates(binary, stage, out, env)
    if list_only:
        return {"status": "listed", "candidates": len(candidates), "outcomes": {}}
    selected = candidates[:maximum]
    reports = []
    for index, mutant in enumerate(selected, 1):
        reports.append(execute_mutant(binary, stage, mutant, index, timeout, out, env))
    outcomes = aggregate_outcomes(reports, len(selected))
    return {"status": "complete" if candidates else "no_mutants", "candidates": len(candidates),
            "selected": len(selected), "outcomes": outcomes}


def main():
    args = arguments()
    pilot = json.loads(PILOT.read_text())
    targets = select_targets(args, pilot["modules"])
    out = prepare_output(args.out)
    summary = {"schema_version": 1, "engine": "zig", "graph": pilot["graph"],
               "tool": json.loads(Path(__file__).with_name("zig-tool.json").read_text()),
               "targets": targets, "workers": 1, "timeout_seconds": args.timeout,
               "max_mutants": args.max_mutants, "result_cache": False}
    try:
        if not targets:
            summary.update(status="no_targets", outcomes={})
        else:
            binary = tool_binary(out)
            with tempfile.TemporaryDirectory(prefix="phux-zig-mutation-") as temporary:
                stage = Path(temporary)
                summary["source_sha256"] = prepare_project(stage, targets)
                configuration(stage, targets, pilot["operators"], args.timeout)
                shutil.copy2(stage / "zentinel.toml", out / "zentinel.toml")
                summary.update(execute_project(binary, stage, out, args.max_mutants,
                                               args.timeout, args.list))
    except (OSError, ValueError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        summary.update(status="error", error=str(error))
        raise
    finally:
        write_json(out / "summary.json", summary)
        print(json.dumps(summary, indent=2))


def interrupted(signum, frame):
    raise KeyboardInterrupt(f"signal {signum}")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupted)
    try:
        main()
    except (OSError, ValueError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        print(f"zig-mutation: {error}", file=sys.stderr)
        sys.exit(2)
