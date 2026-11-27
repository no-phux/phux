"""Qualification trace validation, evidence capture and owned teardown."""
import hashlib
import json
import os
import subprocess

PHASES = frozenset((
    "attempt_started", "dial_started", "dial_connected", "dial_deadline",
    "dial_plan_started", "dial_plan_ready", "ws_resolve", "ws_tcp_connect",
    "ws_tls_config", "ws_tls_handshake", "ws_upgrade", "ws_upgraded",
    "tls_sign_started", "tls_sign_returned",
    "hello_sent", "hello_received", "hello_accepted", "topology_received", "topology_applied",
    "attach_ready", "initial_deadline", "end_closed", "end_dropped", "end_refused",
    "end_resync", "session_ended", "resync_requested", "nudge_requested",
    "path_changed", "probe_deadline", "input_ready", "input_injected", "input_refused",
    "enroll_runtime", "enroll_validate", "enroll_plan", "enroll_public_key", "enroll_csr",
    "enroll_prepare", "enroll_connect", "enroll_relay_connect", "enroll_open_stream",
    "enroll_write", "enroll_read", "enroll_drain", "enroll_decode", "enroll_chain",
    "enroll_succeeded", "enroll_failed", "enroll_deadline", "enroll_refused",
    "enroll_authority_changed", "enroll_unreachable", "enroll_stalled",
    "enroll_exchange_failed",
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
    enrollment_failures = 0
    enrollment_deadlines = 0
    recovered_after_deadline = False
    for line in lines:
        row = diagnostic_row(line)
        key = (row["connection"], row["attempt"])
        if row["process_elapsed_ms"] < process_elapsed:
            raise ValueError("nonmonotonic diagnostic process time")
        attempts[key] = advance_attempt(row, attempts.get(key, (0, set())))
        process_elapsed = row["process_elapsed_ms"]
        deadlines += row["phase"] == "initial_deadline"
        enrollment_failures += row["phase"] == "enroll_failed"
        enrollment_deadlines += row["phase"] == "enroll_deadline"
        if deadlines and row["phase"] == "topology_applied":
            recovered_after_deadline = True
    ready = {"dial_started", "dial_connected", "hello_sent", "hello_received",
             "hello_accepted", "topology_received", "topology_applied"}
    ready_attempts = sum(ready <= phases for _, phases in attempts.values())
    if require_ready and not ready_attempts:
        raise ValueError("missing usable inventory diagnostic phases")
    if require_ready and enrollment_failures:
        raise ValueError("enrollment failure cannot be erased by later inventory")
    return {"events": len(lines), "attempts": len(attempts),
            "initial_deadlines": deadlines, "ready_attempts": ready_attempts,
            "ready_after_initial_deadline": recovered_after_deadline,
            "enrollment_failures": enrollment_failures,
            "enrollment_deadlines": enrollment_deadlines}

def validate_completion(data, events):
    try:
        status = json.loads(data, object_pairs_hook=unique_fields)
    except (ValueError, TypeError):
        raise ValueError("malformed capture completion") from None
    if not isinstance(status, dict) or status.keys() != {"complete", "failed", "events"}:
        raise ValueError("unexpected capture completion fields")
    if status["complete"] is not True or status["failed"] is not False:
        raise ValueError("connection capture unfinished or lost events")
    if type(status["events"]) is not int or status["events"] != events:
        raise ValueError("capture completion event count mismatch")


def copy_private(path, data):
    with open(path, "xb", opener=lambda path, flags: os.open(path, flags, 0o600)) as destination:
        destination.write(data)


def finish_connection_diagnostics(self):
    if self.documents is None or self.diagnostic_pid is None or not self.diagnostic_prefixes:
        return
    prefix = self.diagnostic_prefixes[-1]
    if prefix in self.diagnostic_sealed:
        return
    # One state-changing request, never retry an uncertain lifecycle operation.
    self.diagnostic_sealed.add(prefix)
    stem = prefix + "rune-connection-diagnostics"
    if prefix not in self.diagnostic_reports and preparation_failure_sealed(self, prefix, stem):
        # Preparation publishes its first failure only AFTER synchronous sealing
        # returns. No consumer exists to acknowledge a background transition.
        # Preservation still validates the seal, event count and pending files.
        return
    # Same readiness policy as cycle_scene: the native callback writes this
    # after sealing returns, so termination cannot interrupt receipt cleanup.
    background = self.documents / "rune-background.json"
    if self.failure_evidence_requested:
        # The seal handshake replaces this file. Retain its original inode now,
        # but do not read/copy its bytes until owned shutdown establishes quiescence.
        self.stage_failure_background()
    background.unlink(missing_ok=True)
    copy_private(self.documents / (stem + ".finish"), b"")
    self.simctl("launch", self.udid, "com.apple.springboard")
    self.await_scene_report(self.documents / (stem + ".status.json"), self.diagnostic_pid,
                            "no sealed connection capture receipt before termination")
    self.await_scene_report(background, self.diagnostic_pid,
                             "no post-seal native background acknowledgement")


def preparation_failure_sealed(self, prefix, stem):
    if not (self.documents / (stem + ".status.json")).is_file():
        return False
    try:
        report = json.loads((self.out / (prefix + "rune-qualification.json")).read_bytes(),
                            object_pairs_hook=unique_fields)
    except (OSError, ValueError):
        return False
    return (isinstance(report, dict) and report.keys() == {"schema", "error", "preparation_failed"}
            and type(report["schema"]) is int and report["schema"] == 1
            and report["preparation_failed"] is True
            and type(report["error"]) is str and bool(report["error"]))


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
            copy_private(self.out / name, data)
            pending_name = name.removesuffix(".jsonl") + ".status.pending"
            pending_path = self.documents / pending_name
            pending = pending_path.is_file()
            if pending:
                copy_private(self.out / pending_name, pending_path.read_bytes())
                receipt["pending_completion_file"] = pending_name
            completion_name = name.removesuffix(".jsonl") + ".status.json"
            completion = (self.documents / completion_name).read_bytes()
            copy_private(self.out / completion_name, completion)
            receipt["completion_file"] = completion_name
            receipt["completion_sha256"] = hashlib.sha256(completion).hexdigest()
            receipt["sha256"] = hashlib.sha256(data).hexdigest()
            receipt.update(validate_trace(data, require_ready=prefix in self.diagnostic_reports))
            validate_completion(completion, receipt["events"])
            if pending:
                raise ValueError("capture completion still pending")
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
        self.finish_connection_diagnostics()
    except (ValueError, OSError, subprocess.SubprocessError):
        errors.append("diagnostic finalization failed; see command and capture receipts")
    try:
        self.stop_server()
    except (OSError, subprocess.SubprocessError) as error:
        errors.append(f"server cleanup: {error}")
    if errors:
        self.failure_evidence_requested = True
    if self.udid is not None:
        errors.extend(self.cleanup_simulator())
    (self.out / "cleanup.json").write_text(json.dumps({"errors": errors}) + "\n")
    if errors:
        raise ValueError("owned simulator cleanup failed: " + "; ".join(errors))


def cleanup_simulator(self):
    errors = []
    quiescent = False
    try:
        self.simctl("shutdown", self.udid, timeout=60)
        quiescent = True
    except (OSError, subprocess.SubprocessError) as error:
        errors.append(f"shutdown {self.udid}: {error}")
    try:
        status = self.preserve_connection_diagnostics(quiescent=quiescent)
        if any(row["status"] != "retained" for row in status["files"]):
            self.failure_evidence_requested = True
    except OSError:
        errors.append("diagnostic receipt could not be written")
    finally:
        if self.failure_evidence_requested or errors:
            preserve_native_failure(self, quiescent, errors)
        try:
            self.simctl("delete", self.udid, timeout=60)
        except (OSError, subprocess.SubprocessError) as error:
            errors.append(f"delete {self.udid}: {error}")
    return errors


def preserve_native_failure(self, quiescent, errors):
    try:
        self.preserve_failure_evidence(quiescent=quiescent)
    except Exception:
        # Diagnostic retention must never replace the original host/app error,
        # and even an unexpected capture error must not skip owned deletion.
        errors.append("UNVALIDATED native failure evidence could not be retained")


def stop_server(self):
    if self.server is not None and self.server.poll() is None:
        self.server.terminate()  # Popen owns this exact PID, never a port/name.
        try:
            self.server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.server.kill()
            self.server.wait(timeout=10)
