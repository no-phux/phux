#!/usr/bin/env python3
"""One-off instrumented native sequence. NEVER release qualification evidence."""
import argparse
import ctypes
from datetime import datetime, timezone
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import threading
import time


STALL_SECONDS = 10
SAMPLE_SECONDS = 15
RESOURCE_SECONDS = 5
CAPTURE_SECONDS = 20
CLEANUP_SECONDS = 1


def utc():
    return datetime.now(timezone.utc).isoformat()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


class BSDInfo(ctypes.Structure):
    # Darwin sys/proc_info.h, PROC_PIDTBSDINFO. Start time includes microseconds:
    # ps lstart alone cannot distinguish rapid PID reuse within one second.
    _fields_ = ([(name, ctypes.c_uint32) for name in (
        "flags", "status", "xstatus", "pid", "ppid", "uid", "gid", "ruid", "rgid",
        "svuid", "svgid", "reserved")] + [("comm", ctypes.c_char * 16),
        ("name", ctypes.c_char * 32)] + [(name, ctypes.c_uint32) for name in (
        "nfiles", "pgid", "jobc", "tdev", "tpgid")] + [("nice", ctypes.c_int32),
        ("start_seconds", ctypes.c_uint64), ("start_microseconds", ctypes.c_uint64)])


def process_identity(pid):
    library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    info = BSDInfo()
    if library.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info)) != ctypes.sizeof(info):
        return None
    if info.pid != pid or info.status == 5:  # SZOMB
        return None
    path = ctypes.create_string_buffer(4096)
    if library.proc_pidpath(pid, path, len(path)) <= 0:
        return None
    return {"pid": pid, "start_seconds": info.start_seconds,
            "start_microseconds": info.start_microseconds,
            "executable": os.fsdecode(path.value)}


def same_process(original, current):
    if not original or not current:
        return False
    # exec (xcrun -> simctl) preserves ownership and start identity.
    keys = ("pid", "start_seconds", "start_microseconds")
    return all(original[key] == current[key] for key in keys)


class Observation:
    """A single command's cancellable, bounded diagnostic sidecar."""
    def __init__(self, lane, process, prefix, identity, targets):
        self.lane, self.process, self.prefix = lane, process, prefix
        self.identity, self.targets = identity, targets
        self.command_finished = threading.Event()
        self.cancelled = threading.Event()
        self.deadline = None
        # Workers publish their retained child before any identity lookup/wait.
        # List append/remove/copy are atomic in this CPython diagnostic harness;
        # no registry lock may prevent cancellation or deadline snapshots.
        self.helpers = []
        self.workers = []
        self.thread = threading.Thread(target=self.observe, daemon=True)

    def stop(self):
        # Wake an observer still waiting for the stall threshold immediately.
        # An active capture may finish, without moving the command's deadline.
        self.command_finished.set()
        deadline = self.deadline or time.monotonic() + CAPTURE_SECONDS
        self.thread.join(timeout=max(0, deadline - CLEANUP_SECONDS - time.monotonic()))
        self.cancel_helpers()
        self.thread.join(timeout=max(0, deadline - time.monotonic()))
        cleanup = self.cleanup_status()
        write_json(self.prefix.with_suffix(".cleanup.json"), cleanup)
        return cleanup

    def cancel_helpers(self):
        self.cancelled.set()
        for helper in self.helpers.copy():
            self.signal_helper(helper)

    def signal_helper(self, helper):
        process = helper.get("process")
        if process is None:
            return
        try:
            if process.poll() is None:
                process.kill()  # Retained helper Popen only; never a PID lookup.
        except Exception as error:
            helper["record"]["signal_error"] = type(error).__name__

    def cleanup_status(self):
        helpers = [{**helper["record"], "reaped": False} for helper in self.helpers.copy()]
        workers = [label for label, worker in self.workers.copy() if worker.is_alive()]
        complete = not helpers and not workers and not self.thread.is_alive()
        return {"cleanup_complete": complete, "unreaped_helpers": helpers,
                "unfinished_workers": workers, "observer_alive": self.thread.is_alive(),
                "external_blocker": None if complete else
                "owned helper termination/reaping or diagnostic worker completion unavailable"}

    def helper(self, label, command, timeout=RESOURCE_SECONDS, target=None):
        output = self.prefix.parent / (self.prefix.name + "-" + label)
        record = {"label": label, "pid": None, "identity": None,
                  "started_utc": utc(), "started_monotonic": time.monotonic(),
                  "deadline_seconds": timeout, "target_identity": target}
        helper = {"record": record, "process": None}
        self.helpers.append(helper)
        with output.with_suffix(".log").open("w") as log:
            try:
                if self.cancelled.is_set() or not self.target_matches(target, record):
                    return record
                process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                           env=self.lane.env)
                helper["process"] = process
                record["pid"] = process.pid
                if self.cancelled.is_set():
                    return record
                record["identity"] = process_identity(process.pid)
                if self.deadline is not None:
                    timeout = min(timeout, max(0, self.deadline - time.monotonic()))
                record["returncode"] = process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                record["timed_out"] = True
            except Exception as error:
                record["error"] = type(error).__name__
            finally:
                self.reap_helper(helper, record)
                if helper["process"] is not None:
                    record["returncode"] = helper["process"].returncode
                record.update({"finished_utc": utc(), "cancelled": self.cancelled.is_set()})
                write_json(output.with_suffix(".json"), record)
        return record

    def target_matches(self, target, record):
        if target is None:
            return True
        record["target_identity_at_start"] = process_identity(target["pid"])
        if same_process(target, record["target_identity_at_start"]):
            return True
        record["skipped"] = "process exited or identity changed"
        return False

    def reap_helper(self, helper, record):
        process = helper["process"]
        if process is None:
            self.helpers.remove(helper)
            return
        self.signal_helper(helper)
        deadline = min(self.deadline or float("inf"), time.monotonic() + CLEANUP_SECONDS)
        try:
            process.wait(timeout=max(0, deadline - time.monotonic()))
        except Exception as error:
            record["cleanup_error"] = type(error).__name__
            record["cleanup_complete"] = False
            return
        record["cleanup_complete"] = True
        self.helpers.remove(helper)

    def sample(self, role, identity):
        destination = self.prefix.parent / (self.prefix.name + f"-{role}.sample.txt")
        record = {"target_identity": identity, "capture_complete": False}
        try:
            if not same_process(identity, process_identity(identity["pid"])):
                record["skipped"] = "process exited or identity changed"
                return
            helper = self.helper(role + "-sampler", [
                "/usr/bin/sample", str(identity["pid"]), "1", "20", "-mayDie",
                "-file", str(destination)], timeout=SAMPLE_SECONDS, target=identity)
            record["helper"] = helper
            report = destination.read_bytes() if destination.exists() else b""
            record["report_bytes"] = len(report)
            record["capture_complete"] = sample_complete(helper, identity, report.decode(errors="replace"))
        except Exception as error:
            record["error"] = type(error).__name__
        finally:
            record["finished_utc"] = utc()
            write_json(self.prefix.parent / (self.prefix.name + f"-{role}-capture.json"), record)

    def resources(self):
        commands = [
            ("host", ["/usr/sbin/sysctl", "hw.logicalcpu", "hw.memsize",
                      "vm.loadavg", "vm.swapusage"]),
            ("ps", ["/bin/ps", "-axo", "pid,ppid,lstart,state,time,%cpu,rss,comm"]),
            ("top", ["/usr/bin/top", "-l", "2", "-s", "1", "-F", "-R", "-o", "cpu",
                     "-n", "30", "-stats", "pid,command,cpu,time,threads,state,mem"]),
            ("vm", ["/usr/bin/vm_stat", "-c", "2", "1"]),
        ]
        if self.lane.udid:
            predicate = ('process == "CoreSimulatorService" AND eventMessage CONTAINS "'
                         + self.lane.udid + '"')
            commands.append(("service-log", ["/usr/bin/log", "show", "--last", "2m",
                                             "--style", "compact", "--predicate", predicate]))
        return commands

    def capture(self, deadline):
        try:
            # Fixed fanout: two cheap resources, up to three owned samplers,
            # then three heavier resources. No helper waits for another helper.
            resources = self.resources()
            tasks = [(self.helper, args) for args in resources[:2]]
            tasks += [(self.sample, args) for args in self.targets.items()]
            tasks += [(self.helper, args) for args in resources[2:]]
            for function, args in tasks:
                worker = threading.Thread(target=function, args=args, daemon=True)
                self.workers.append((args[0], worker))
                worker.start()
            for _, worker in self.workers:
                worker.join(timeout=max(0, deadline - CLEANUP_SECONDS - time.monotonic()))
        finally:
            self.cancel_helpers()
            for _, worker in self.workers:
                worker.join(timeout=max(0, deadline - time.monotonic()))

    def observe(self):
        try:
            if self.command_finished.wait(STALL_SECONDS) or self.process.poll() is not None:
                return
            if not same_process(self.identity, process_identity(self.process.pid)):
                return
            self.deadline = time.monotonic() + CAPTURE_SECONDS
            write_json(self.prefix.with_suffix(".observation.json"), {
                "diagnostic_only": True, "started_utc": utc(),
                "started_monotonic": time.monotonic(), "capture_budget_seconds": CAPTURE_SECONDS,
                "note": "sample briefly suspends targets; not pristine qualification timing",
                "owned_targets": self.targets,
            })
            self.capture(self.deadline)
        except Exception as error:
            # Neither a helper error nor observation failure may mask the original.
            write_json(self.prefix.with_suffix(".observation-error.json"),
                       {"error": type(error).__name__})


def sample_complete(helper, identity, report):
    if (helper.get("returncode") != 0 or helper.get("cancelled")
            or helper.get("timed_out") or helper.get("error")):
        return False
    if not same_process(identity, helper.get("target_identity_at_start")):
        return False
    if helper.get("cleanup_complete") is not True:
        return False
    # A "Sampling process" preamble is not a stack report. Require its target
    # header, call graph and at least one real (possibly unsymbolicated) frame.
    return (re.search(r"(?m)^Process:\s+.+\[" + str(identity["pid"]) + r"\]\s*$", report) is not None
            and "Call graph:" in report
            and re.search(r"(?m)^\s*[+!:| ]*\d+\s+.+\s+\(in .+\)", report) is not None)


def capture_summary(directory):
    try:
        observations = list(directory.glob("command-*.observation.json"))
        expected = sum(len(json.loads(path.read_text())["owned_targets"]) for path in observations)
        captures = [json.loads(path.read_text()) for path in directory.glob("command-*-capture.json")]
        completed = sum(record["capture_complete"] for record in captures)
        errors = len(list(directory.glob("command-*.observation-error.json")))
        incomplete = incomplete_cleanup(directory)
        return {"observations": len(observations), "expected_samples": expected,
                "completed_stack_samples": completed, "incomplete_samples": expected - completed,
                "observation_errors": errors,
                "incomplete_cleanup": incomplete, "cleanup_complete": not incomplete,
                "capture_complete": expected > 0 and completed == expected and errors == 0
                                    and not incomplete}
    except (OSError, ValueError, KeyError, TypeError) as error:
        # Damaged diagnostic output must not mask the original sequence result.
        return {"capture_complete": False, "error": type(error).__name__}


def incomplete_cleanup(directory):
    records = [json.loads(path.read_text()) for path in directory.glob("command-*.cleanup.json")]
    return [record for record in records if not record["cleanup_complete"]]


def owned_targets(lane, command_identity):
    targets = {}
    if command_identity:
        targets["command"] = command_identity
    if not hasattr(lane, "owned_identities"):
        lane.owned_identities = {}
    if lane.server is not None and lane.server.poll() is None:
        if "server" not in lane.owned_identities:
            identity = process_identity(lane.server.pid)
            if identity and identity["executable"] == str(lane.phux.resolve()):
                lane.owned_identities["server"] = identity
    for role, identity in lane.owned_identities.items():
        if same_process(identity, process_identity(identity["pid"])):
            targets[role] = identity
    return targets


def owned_app(lane, executable):
    path = Path(executable)
    parts = {part.lower() for part in path.parts}
    return (lane.udid is not None and lane.udid.lower() in parts
            and path.name == lane.app_executable and path.parent.name == lane.app.name)


def diagnostic_run(lane, command, timeout=30, env=None):
    lane.sequence += 1
    command = [str(arg) for arg in command]
    prefix = lane.out / f"command-{lane.sequence:03}"
    write_json(prefix.with_suffix(".json"), command)
    started, started_utc = time.monotonic(), utc()
    record = {"started_utc": started_utc, "started_monotonic": started,
              "deadline_seconds": timeout, "succeeded": False}
    try:
        with prefix.with_suffix(".log").open("w") as log, \
                prefix.with_suffix(".stderr.log").open("w") as diagnostics:
            process = subprocess.Popen(command, env=lane.env if env is None else env,
                                       stdout=log, stderr=diagnostics)
            deadline = time.monotonic() + timeout
            record["pid"] = process.pid
            observation = None
            try:
                try:
                    identity = process_identity(process.pid)
                    record["identity"] = identity
                    targets = owned_targets(lane, identity)
                    write_json(prefix.with_suffix(".process.json"), {
                        **record, "owned_targets": targets})
                    candidate = Observation(lane, process, prefix, identity, targets)
                    candidate.thread.start()
                    observation = candidate
                except Exception:
                    record["diagnostic_setup_failed"] = True
                result = process.wait(timeout=max(0, deadline - time.monotonic()))
                record["returncode"] = result
                if result:
                    raise subprocess.CalledProcessError(result, command)
                record["succeeded"] = True
            except BaseException as error:
                record["failure_class"] = type(error).__name__
                try:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=CLEANUP_SECONDS)
                except Exception as cleanup_error:
                    record["command_cleanup_error"] = type(cleanup_error).__name__
                raise
            finally:
                record["command_finished_monotonic"] = time.monotonic()
                record["finished_utc"] = utc()
                record["command_elapsed_seconds"] = record["command_finished_monotonic"] - started
                if observation is not None:
                    record["diagnostic_cleanup"] = observation.stop()
    finally:
        record.update({"observation_finished_utc": utc(), "elapsed_seconds": time.monotonic() - started})
        write_json(prefix.with_suffix(".timing.json"), record)
    return prefix.with_suffix(".log").read_text()


def load_qualifier():
    path = Path(__file__).with_name("qualify.py")
    spec = importlib.util.spec_from_file_location("diagnostic_original_qualify", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run_sequence(app, phux, out):
    out.mkdir(parents=True, exist_ok=False)
    write_json(out / "DIAGNOSTIC-ONLY.json", {
        "diagnostic_only": True, "release_qualification": False,
        "sampling": "owned targets only, 1 second at 20ms; 15s sampler / 5s resource wait; "
                    "20s capture/cleanup budget, including command-exit drain",
        "sequence": "unmodified qualifier ordering, retries, assertions and budgets; QUIC then WSS",
    })
    module = load_qualifier()
    original = module.Qualification
    with (app / "Info.plist").open("rb") as handle:
        executable = plistlib.load(handle)["CFBundleExecutable"]

    class DiagnosticQualification(original):
        run = diagnostic_run
        app_executable = executable

        def collect_report(self, pid, resumed=False):
            try:
                identity = process_identity(pid)
                if identity and owned_app(self, identity["executable"]):
                    self.owned_identities["app"] = identity
            except Exception:
                write_json(self.out / "app-identity-error.json", {"error": "app identity unavailable"})
            return super().collect_report(pid, resumed)

    module.Qualification = DiagnosticQualification
    for transport in ("quic", "wss"):
        directory = out / transport
        succeeded = False
        try:
            module.qualify(app, phux, directory, transport)
            succeeded = True
        finally:
            evidence = directory / "evidence.json"
            if evidence.exists():
                evidence.rename(directory / "DIAGNOSTIC-ONLY-evidence.json")
            write_json(out / (transport + "-diagnostic-status.json"), {
                "diagnostic_only": True, "release_qualification": False,
                "sequence_completed": succeeded,
                "capture": capture_summary(directory),
            })


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, required=True)
    parser.add_argument("--phux", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    try:
        run_sequence(args.app.resolve(), args.phux.resolve(), args.out.resolve())
    except Exception:
        parser.exit(1, "Diagnostic native sequence failed; original command logs retained. NOT qualification.\n")
    print("Diagnostic sequences completed. NOT release qualification.")


if __name__ == "__main__":
    main()
