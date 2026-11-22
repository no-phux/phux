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
import subprocess
import threading
import time


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
        self.cancelled = threading.Event()
        self.lock = threading.Lock()
        self.helpers = []
        self.thread = threading.Thread(target=self.observe, daemon=True)

    def stop(self):
        self.cancelled.set()
        with self.lock:
            for helper in self.helpers:
                if helper.poll() is None:
                    helper.kill()  # Only a Popen we started and have not reaped.
        self.thread.join()

    def helper(self, label, command):
        if self.cancelled.is_set():
            return
        output = self.prefix.parent / (self.prefix.name + "-" + label)
        with output.with_suffix(".log").open("w") as log:
            with self.lock:
                if self.cancelled.is_set():
                    return
                process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                           env=self.lane.env)
                self.helpers.append(process)
            record = {"pid": process.pid, "started_utc": utc(), "deadline_seconds": 2}
            try:
                record["identity"] = process_identity(process.pid)
                record["returncode"] = process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                record["timed_out"] = True
            except BaseException:
                if process.poll() is None:
                    process.kill()
                process.wait()
                raise
            finally:
                write_json(output.with_suffix(".json"), record)

    def sample(self, role, identity):
        if not same_process(identity, process_identity(identity["pid"])):
            write_json(self.prefix.parent / (self.prefix.name + f"-{role}-skipped.json"),
                       {"reason": "process exited or identity changed"})
            return
        destination = self.prefix.parent / (self.prefix.name + f"-{role}.sample.txt")
        self.helper(role + "-sampler", ["/usr/bin/sample", str(identity["pid"]), "1", "20",
                                       "-file", str(destination)])

    def resources(self):
        commands = (
            ("ps", ["/bin/ps", "-axo", "pid,ppid,lstart,state,time,%cpu,rss,comm"]),
            ("top", ["/usr/bin/top", "-l", "2", "-s", "1", "-F", "-R", "-o", "cpu",
                     "-n", "30", "-stats", "pid,command,cpu,time,threads,state,mem"]),
            ("vm", ["/usr/bin/vm_stat", "-c", "2", "1"]),
            ("host", ["/usr/sbin/sysctl", "hw.logicalcpu", "hw.memsize",
                      "vm.loadavg", "vm.swapusage"]),
        )
        for label, command in commands:
            self.helper(label, command)
        if self.lane.udid:
            predicate = ('process == "CoreSimulatorService" AND eventMessage CONTAINS "'
                         + self.lane.udid + '"')
            self.helper("service-log", ["/usr/bin/log", "show", "--last", "2m",
                                      "--style", "compact", "--predicate", predicate])

    def observe(self):
        try:
            if self.cancelled.wait(10) or self.process.poll() is not None:
                return
            if not same_process(self.identity, process_identity(self.process.pid)):
                return
            write_json(self.prefix.with_suffix(".observation.json"), {
                "diagnostic_only": True, "started_utc": utc(),
                "note": "sample briefly suspends targets; not pristine qualification timing",
                "owned_targets": self.targets,
            })
            # Capture the blocked command first, before its existing timeout.
            for role, identity in self.targets.items():
                self.sample(role, identity)
            self.resources()
        except Exception:
            # Neither a helper error nor observation failure may mask the original.
            write_json(self.prefix.with_suffix(".observation-error.json"),
                       {"error": "diagnostic observation failed"})


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
                if process.poll() is None:
                    process.kill()
                process.wait()
                raise
            finally:
                record["command_finished_monotonic"] = time.monotonic()
                if observation is not None:
                    observation.stop()
    finally:
        record.update({"finished_utc": utc(), "elapsed_seconds": time.monotonic() - started})
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
        "sampling": "owned processes only, 1 second at 20ms; each helper bounded to 2 seconds",
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
