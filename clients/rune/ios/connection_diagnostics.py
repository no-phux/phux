"""Qualification trace validation, evidence capture and owned teardown."""
import hashlib
import json
import os
import subprocess

PHASES = frozenset((
    "attempt_started", "dial_started", "dial_connected", "dial_deadline",
    "hello_sent", "hello_received", "hello_accepted", "topology_received", "topology_applied",
    "attach_ready", "initial_deadline", "end_closed", "end_dropped", "end_refused",
    "end_resync", "session_ended", "resync_requested", "nudge_requested",
    "path_changed", "probe_deadline",
))
NUMBERS = ("connection", "attempt", "elapsed_ms", "process_elapsed_ms")
FIELDS = frozenset((*NUMBERS, "phase"))


def unique_fields(pairs):
    result = dict(pairs)
    if len(result) != len(pairs):
        raise ValueError("duplicate diagnostic field")
    return result


def diagnostic_row(line):
    try:
        row = json.loads(line, object_pairs_hook=unique_fields)
    except (ValueError, TypeError):
        raise ValueError("malformed diagnostic row") from None
    if not isinstance(row, dict) or row.keys() != FIELDS:
        raise ValueError("unexpected diagnostic fields")
    validate_numbers(row)
    if type(row["phase"]) is not str or row["phase"] not in PHASES:
        raise ValueError("unexpected diagnostic phase")
    return row


def validate_numbers(row):
    if any(type(row[key]) is not int or not 0 <= row[key] <= 2**64 - 1 for key in NUMBERS):
        raise ValueError("invalid diagnostic number")
    if not row["connection"] or not row["attempt"]:
        raise ValueError("invalid diagnostic identity")


def trace_lines(data):
    if not data or not data.endswith(b"\n"):
        raise ValueError("empty trace or partial diagnostic tail")
    try:
        return data.decode("utf-8").splitlines()
    except UnicodeError:
        raise ValueError("invalid diagnostic encoding") from None


def advance_attempt(row, previous):
    elapsed, phases = previous
    if row["elapsed_ms"] < elapsed:
        raise ValueError("nonmonotonic diagnostic attempt time")
    if not phases and row["phase"] != "attempt_started":
        raise ValueError("missing diagnostic attempt start")
    if row["phase"] == "attempt_started" and phases:
        raise ValueError("duplicate diagnostic attempt start")
    phases.add(row["phase"])
    return row["elapsed_ms"], phases


def validate_trace(data, require_ready=False):
    lines = trace_lines(data)
    attempts = {}
    process_elapsed = 0
    deadlines = 0
    recovered_after_deadline = False
    for line in lines:
        row = diagnostic_row(line)
        key = (row["connection"], row["attempt"])
        if row["process_elapsed_ms"] < process_elapsed:
            raise ValueError("nonmonotonic diagnostic process time")
        attempts[key] = advance_attempt(row, attempts.get(key, (0, set())))
        process_elapsed = row["process_elapsed_ms"]
        deadlines += row["phase"] == "initial_deadline"
        if deadlines and row["phase"] == "topology_applied":
            recovered_after_deadline = True
    ready = {"dial_started", "dial_connected", "hello_sent", "hello_received",
             "hello_accepted", "topology_received", "topology_applied"}
    ready_attempts = sum(ready <= phases for _, phases in attempts.values())
    if require_ready and not ready_attempts:
        raise ValueError("missing usable inventory diagnostic phases")
    return {"events": len(lines), "attempts": len(attempts),
            "initial_deadlines": deadlines, "ready_attempts": ready_attempts,
            "ready_after_initial_deadline": recovered_after_deadline}


# Bound by Qualification as methods. Keep its existing callable seams (including
# inert test doubles) while grouping diagnostic capture and owned teardown here.
def preserve_connection_diagnostics(self, prefixes=None, quiescent=True):
    """Copy once, only after owned terminate/shutdown; retain malformed bytes too."""
    for prefix in self.diagnostic_prefixes if prefixes is None else prefixes:
        if prefix in self.diagnostic_receipts:
            continue
        name = prefix + "rune-connection-diagnostics.jsonl"
        receipt = {"file": name, "status": "invalid", "quiescent": quiescent}
        try:
            if self.documents is None:
                raise ValueError("diagnostic container unavailable")
            data = (self.documents / name).read_bytes()
            with open(self.out / name, "xb", opener=lambda path, flags: os.open(path, flags, 0o600)) as destination:
                destination.write(data)
            receipt["sha256"] = hashlib.sha256(data).hexdigest()
            receipt.update(validate_trace(data, require_ready=prefix in self.diagnostic_reports))
            if not quiescent:
                raise ValueError("diagnostic producer not quiescent")
            receipt["status"] = "retained"
        except OSError:
            receipt["error"] = "diagnostic read or exclusive copy failed"
        except ValueError as error:
            receipt["error"] = str(error)  # Validator emits fixed labels, never row contents.
        self.diagnostic_receipts[prefix] = receipt
    status = {"files": list(self.diagnostic_receipts.values())}
    (self.out / "connection-diagnostics.json").write_text(json.dumps(status, indent=2) + "\n")
    return status


def require_connection_diagnostics(self):
    if self.diagnostic_prefixes != ["", "resume-"]:
        raise ValueError("connection diagnostics did not cover both fixture processes")
    if any(self.diagnostic_receipts.get(prefix, {}).get("status") != "retained"
           for prefix in self.diagnostic_prefixes):
        raise ValueError("connection diagnostics incomplete; see connection-diagnostics.json")


def failure_screenshot(self):
    if self.udid is None:
        return
    try:
        self.simctl("io", self.udid, "screenshot", self.out / "ios-native-failure.png")
    except (OSError, subprocess.SubprocessError):
        # Preserve the failing command and its log; a boot failure has no screen.
        pass
    try:
        self.simctl("spawn", self.udid, "log", "show", "--last", "1m", "--style", "compact",
                    "--predicate", 'eventMessage CONTAINS "sh.phux.rune.ios"', timeout=30)
    except (OSError, subprocess.SubprocessError):
        pass


def cleanup(self):
    errors = []
    try:
        self.stop_server()
    except (OSError, subprocess.SubprocessError) as error:
        errors.append(f"server cleanup: {error}")
    if self.udid is not None:
        errors.extend(self.cleanup_simulator())
    (self.out / "cleanup.json").write_text(json.dumps({"errors": errors}) + "\n")
    if errors:
        raise ValueError("owned simulator cleanup failed: " + "; ".join(errors))


def cleanup_simulator(self):
    errors = []
    try:
        self.simctl("shutdown", self.udid, timeout=60)
    except (OSError, subprocess.SubprocessError) as error:
        errors.append(f"shutdown {self.udid}: {error}")
    try:
        self.preserve_connection_diagnostics(quiescent=not errors)
    except OSError:
        errors.append("diagnostic receipt could not be written")
    finally:
        try:
            self.simctl("delete", self.udid, timeout=60)
        except (OSError, subprocess.SubprocessError) as error:
            errors.append(f"delete {self.udid}: {error}")
    return errors
