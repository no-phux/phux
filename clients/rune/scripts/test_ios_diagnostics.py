"""Inert retention/validation tests: no server, simulator, GUI or network."""
import contextlib
import io
import json
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest.mock import Mock, patch

from test_ios_build import load

qualify = load("qualify")
diagnostics = load("connection_diagnostics")


def trace_bytes(connection=1):
    phases = ("attempt_started", "dial_started", "dial_connected", "hello_sent",
              "hello_received", "hello_accepted", "topology_received", "topology_applied")
    return b"".join(json.dumps({
        "connection": connection, "attempt": 1, "elapsed_ms": index,
        "process_elapsed_ms": index, "phase": phase,
    }).encode() + b"\n" for index, phase in enumerate(phases))


class ConnectionDiagnostics(unittest.TestCase):
    def lane(self, root):
        out = root / "out"
        out.mkdir()
        lane = qualify.Qualification(root / "App", root / "phux", out, root)
        lane.documents = root / "Documents"
        lane.documents.mkdir()
        lane.diagnostic_prefixes = ["", "resume-"]
        lane.diagnostic_reports = {"", "resume-"}
        return lane

    def seed(self, lane):
        for prefix in lane.diagnostic_prefixes:
            (lane.documents / (prefix + "rune-connection-diagnostics.jsonl")).write_bytes(trace_bytes())
            (lane.documents / (prefix + "rune-connection-diagnostics.status.json")).write_text(
                json.dumps({"complete": True, "failed": False, "events": 8}))

    def test_only_initial_and_resumed_traces_are_copied_once_and_receipted(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            self.seed(lane)
            (lane.documents / "preferences.json").write_text("private-preferences-sentinel")
            status = lane.preserve_connection_diagnostics()
            lane.require_connection_diagnostics()
            self.assertEqual([row["status"] for row in status["files"]], ["retained", "retained"])
            self.assertEqual(len(list(lane.out.iterdir())), 5)
            for row in status["files"]:
                self.assertEqual((lane.out / row["file"]).read_bytes(), trace_bytes())
                self.assertEqual((lane.out / row["file"]).stat().st_mode & 0o777, 0o600)
                self.assertEqual(row["events"], 8)
            # No overwrite, even if the original producer path changes later.
            (lane.documents / "rune-connection-diagnostics.jsonl").write_bytes(b"changed\n")
            self.assertEqual(lane.preserve_connection_diagnostics(), status)
            self.assertEqual((lane.out / "rune-connection-diagnostics.jsonl").read_bytes(), trace_bytes())

    def test_missing_or_malformed_trace_cannot_claim_diagnostic_success(self):
        for data in (None, b"", b"{}", b"{}\n", trace_bytes()[:-1]):
            with self.subTest(data=data), tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary))
                self.seed(lane)
                source = lane.documents / "rune-connection-diagnostics.jsonl"
                source.unlink()
                if data is not None:
                    source.write_bytes(data)
                status = lane.preserve_connection_diagnostics()
                self.assertEqual(status["files"][0]["status"], "invalid")
                with self.assertRaisesRegex(ValueError, "diagnostics incomplete"):
                    lane.require_connection_diagnostics()

    def test_no_clobber_and_nonquiescent_capture_are_invalid(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            self.seed(lane)
            retained = lane.out / "rune-connection-diagnostics.jsonl"
            retained.write_bytes(b"original-sentinel")
            status = lane.preserve_connection_diagnostics(quiescent=False)
            self.assertEqual(retained.read_bytes(), b"original-sentinel")
            self.assertTrue(all(row["status"] == "invalid" for row in status["files"]))
            self.assertFalse(status["files"][1]["quiescent"])
            with self.assertRaises(ValueError):
                lane.require_connection_diagnostics()

    def test_sticky_lost_write_receipt_rejects_valid_prefix_and_later_recovery(self):
        for name in ("lost-deadline-then-recovery", "lost-tail-after-ready"):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary))
                self.seed(lane)
                # Both remain syntactically valid, usable prefixes. Only the
                # sealed producer receipt can expose a missing whole write.
                events = 8
                if name == "lost-deadline-then-recovery":
                    first = trace_bytes().splitlines(keepends=True)[:6]
                    later = [json.loads(row) for row in trace_bytes(2).splitlines()]
                    for row in later:
                        row["process_elapsed_ms"] += 6
                    data = b"".join(first) + b"".join(
                        json.dumps(row).encode() + b"\n" for row in later)
                    (lane.documents / "rune-connection-diagnostics.jsonl").write_bytes(data)
                    events = 14
                (lane.documents / "rune-connection-diagnostics.status.json").write_text(
                    json.dumps({"complete": True, "failed": True, "events": events}))
                status = lane.preserve_connection_diagnostics()
                self.assertEqual(status["files"][0]["status"], "invalid")
                self.assertIn("lost events", status["files"][0]["error"])
                self.assertTrue((lane.out / "rune-connection-diagnostics.status.json").is_file())
                with self.assertRaises(ValueError):
                    lane.require_connection_diagnostics()

    def test_missing_malformed_duplicate_or_mismatched_completion_is_invalid(self):
        for data in (None, b"", b"{}", b'{"complete":true,"failed":false,"events":7}',
                     b'{"complete":true,"failed":false,"events":8,"events":8}'):
            with self.subTest(data=data), tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary))
                self.seed(lane)
                path = lane.documents / "rune-connection-diagnostics.status.json"
                path.unlink()
                if data is not None:
                    path.write_bytes(data)
                status = lane.preserve_connection_diagnostics()
                self.assertEqual(status["files"][0]["status"], "invalid")
                with self.assertRaises(ValueError):
                    lane.require_connection_diagnostics()

    def test_final_background_handshake_is_once_before_owned_shutdown(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            self.seed(lane)
            lane.diagnostic_pid = 123
            lane.udid = "EXACT-OWNED-UDID"
            seen = []
            marker = lane.documents / "resume-rune-connection-diagnostics.finish"

            def simctl(action, udid, *args, **kwargs):
                self.assertEqual(udid, lane.udid)
                seen.append(action)
                if action == "launch":
                    self.assertTrue(marker.is_file())
                    self.assertEqual(args, ("com.apple.springboard",))
                elif action == "shutdown":
                    self.assertEqual(seen, ["launch", "shutdown"])

            lane.simctl = Mock(side_effect=simctl)
            lane.await_scene_report = Mock(return_value={"complete": True, "failed": False, "events": 8})
            lane.finish_connection_diagnostics()
            lane.finish_connection_diagnostics()
            lane.cleanup()
            self.assertEqual(seen, ["launch", "shutdown", "delete"])
            self.assertEqual(lane.await_scene_report.call_count, 2)
            self.assertEqual(lane.await_scene_report.call_args_list[-1].args[0],
                             lane.documents / "rune-background.json")
            lane.require_connection_diagnostics()

    def test_preparation_failure_report_acknowledges_the_seal_without_a_consumer(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            lane.diagnostic_prefixes = [""]
            lane.diagnostic_reports = set()
            lane.diagnostic_pid = 123
            phases = ("attempt_started", "enroll_runtime", "enroll_validate", "enroll_plan",
                      "enroll_prepare", "enroll_connect", "enroll_read", "enroll_deadline",
                      "enroll_failed")
            data = b"".join(json.dumps({
                "connection": 1, "attempt": 1, "elapsed_ms": index,
                "process_elapsed_ms": index, "phase": phase,
            }).encode() + b"\n" for index, phase in enumerate(phases))
            (lane.documents / "rune-connection-diagnostics.jsonl").write_bytes(data)
            (lane.documents / "rune-connection-diagnostics.status.json").write_text(json.dumps(
                {"complete": True, "failed": False, "events": len(phases)}))
            (lane.out / "rune-qualification.json").write_text(json.dumps(
                {"schema": 1, "error": "original sanitized failure", "preparation_failed": True}))
            lane.simctl = Mock(side_effect=AssertionError("no competing background transition"))
            lane.finish_connection_diagnostics()
            status = lane.preserve_connection_diagnostics()
            self.assertEqual(status["files"][0]["status"], "retained")
            self.assertEqual(status["files"][0]["enrollment_failures"], 1)
            self.assertEqual(status["files"][0]["enrollment_deadlines"], 1)
            self.assertEqual(status["files"][0]["ready_attempts"], 0)
            with self.assertRaisesRegex(ValueError, "missing usable inventory"):
                diagnostics.validate_trace(data, require_ready=True)
            with self.assertRaisesRegex(ValueError, "both fixture processes"):
                lane.require_connection_diagnostics()

    def test_preparation_shortcut_never_bypasses_success_or_invalid_completion(self):
        for successful, pending, lost in ((True, False, False), (False, True, False),
                                           (False, False, True)):
            with self.subTest(successful=successful, pending=pending, lost=lost), \
                    tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary))
                self.seed(lane)
                lane.diagnostic_prefixes = [""]
                lane.diagnostic_reports = {""} if successful else set()
                lane.diagnostic_pid = 123
                (lane.out / "rune-qualification.json").write_text(json.dumps(
                    {"schema": 1, "error": "original failure", "preparation_failed": True}))
                stem = lane.documents / "rune-connection-diagnostics"
                if pending:
                    stem.with_suffix(".status.pending").write_bytes(b'{"complete":')
                if lost:
                    stem.with_suffix(".status.json").write_text(json.dumps(
                        {"complete": True, "failed": True, "events": 8}))
                lane.simctl = Mock()
                lane.await_scene_report = Mock()
                lane.finish_connection_diagnostics()
                self.assertEqual(lane.simctl.call_count, int(successful))
                self.assertEqual(lane.await_scene_report.call_count, 2 * int(successful))
                if not successful:
                    self.assertEqual(lane.preserve_connection_diagnostics()["files"][0]["status"],
                                     "invalid")

    def test_pending_completion_is_retained_and_never_certifies_capture(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            self.seed(lane)
            pending = "rune-connection-diagnostics.status.pending"
            (lane.documents / pending).write_bytes(b'{"complete":')
            status = lane.preserve_connection_diagnostics()
            self.assertEqual((lane.out / pending).read_bytes(), b'{"complete":')
            self.assertEqual(status["files"][0]["status"], "invalid")
            self.assertEqual(status["files"][0]["pending_completion_file"], pending)
            with self.assertRaises(ValueError):
                lane.require_connection_diagnostics()

    def test_shutdown_then_copy_then_delete(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            self.seed(lane)
            lane.udid = "EXACT-OWNED-UDID"
            seen = []

            def simctl(action, udid, **kwargs):
                self.assertEqual(udid, lane.udid)
                seen.append(action)
                if action == "shutdown":
                    self.assertFalse((lane.out / "rune-connection-diagnostics.jsonl").exists())
                if action == "delete":
                    lane.require_connection_diagnostics()

            lane.simctl = Mock(side_effect=simctl)
            lane.cleanup()
            self.assertEqual(seen, ["shutdown", "delete"])

    def test_original_app_failure_dominates_missing_diagnostics_and_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lane = self.lane(root)
            lane.udid = "EXACT-OWNED-UDID"
            lane.simctl = Mock()
            lane.app.mkdir()
            (lane.app / "Info.plist").write_bytes(plistlib.dumps({
                "CFBundleIdentifier": qualify.BUNDLE_ID,
                "CFBundleSupportedPlatforms": ["iPhoneSimulator"],
            }))
            lane.start_server = Mock(return_value="owned-socket")
            lane.create_simulator = Mock()
            lane.launch = Mock(return_value=123)
            lane.collect_report = Mock(side_effect=ValueError("original initial-budget failure"))
            lane.resume_identity = Mock()
            lane.failure_screenshot = Mock()
            with patch.object(qualify, "Qualification", return_value=lane), \
                    contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaisesRegex(ValueError, "original initial-budget failure"):
                    qualify.qualify(lane.app, lane.phux, lane.out)
            lane.collect_report.assert_called_once()
            lane.resume_identity.assert_not_called()
            lane.failure_screenshot.assert_called_once()
            self.assertEqual(lane.simctl.call_args_list[-1].args, ("delete", lane.udid))
            status = json.loads((lane.out / "connection-diagnostics.json").read_text())
            self.assertTrue(all(row["status"] == "invalid" for row in status["files"]))

    def test_successful_reports_with_missing_diagnostics_fail_closed_after_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lane = self.lane(root)
            lane.udid = "EXACT-OWNED-UDID"
            lane.simctl = Mock()
            lane.app.mkdir()
            lane.phux.write_bytes(b"inert-binary-fixture")
            (lane.app / "Info.plist").write_bytes(plistlib.dumps({
                "CFBundleIdentifier": qualify.BUNDLE_ID,
                "CFBundleSupportedPlatforms": ["iPhoneSimulator"],
            }))
            lane.start_server = Mock(return_value="owned-socket")
            lane.create_simulator = Mock()
            lane.launch = Mock(return_value=123)
            lane.collect_report = Mock(return_value={"fixture": True})
            lane.qualify_glass = Mock()
            lane.resume_identity = Mock(return_value={"fixture": True})
            with patch.object(qualify, "Qualification", return_value=lane):
                with self.assertRaisesRegex(ValueError, "diagnostics incomplete"):
                    qualify.qualify(lane.app, lane.phux, lane.out)
            self.assertEqual(lane.simctl.call_args_list[-1].args, ("delete", lane.udid))
            self.assertEqual(json.loads((lane.out / "cleanup.json").read_text()), {"errors": []})

    def test_exclusive_copy_failure_returns_invalid_status_without_raw_details(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            self.seed(lane)
            with patch.object(Path, "open", side_effect=PermissionError("private-path-sentinel")):
                # read_bytes uses open too. The collector still returns a
                # fixed error, except if writing the receipt also fails.
                with self.assertRaises(PermissionError):
                    lane.preserve_connection_diagnostics()
            self.assertTrue(all(row["status"] == "invalid" for row in lane.diagnostic_receipts.values()))
            self.assertNotIn("private-path-sentinel", json.dumps(lane.diagnostic_receipts))

    def test_receipt_write_error_during_teardown_cannot_replace_first_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            app = root / "App"
            app.mkdir()
            (app / "Info.plist").write_bytes(plistlib.dumps({
                "CFBundleIdentifier": qualify.BUNDLE_ID,
                "CFBundleSupportedPlatforms": ["iPhoneSimulator"],
            }))
            lane = Mock()
            lane.collect_report.side_effect = ValueError("first app failure")
            lane.cleanup.side_effect = PermissionError("receipt write failed")
            with patch.object(qualify, "Qualification", return_value=lane), \
                    contextlib.redirect_stderr(io.StringIO()) as log:
                with self.assertRaisesRegex(ValueError, "first app failure"):
                    qualify.qualify(app, root / "phux", root / "out")
            self.assertIn("Additional qualification teardown failure", log.getvalue())
            lane.require_connection_diagnostics.assert_not_called()


class TraceValidation(unittest.TestCase):
    def test_real_phase_chain_and_allowlists_stay_aligned(self):
        summary = diagnostics.validate_trace(trace_bytes(), require_ready=True)
        self.assertEqual(summary["ready_attempts"], 1)
        root = Path(__file__).resolve().parents[1]
        subscriber = (root / "ios/bridge/src/diagnostics.rs").read_text().split("const PHASES:")[1].split("];")[0]
        import re
        self.assertEqual(set(re.findall(r'"([a-z_]+)"', subscriber)), diagnostics.PHASES)

    def test_unknown_duplicate_wrong_type_and_nonmonotonic_rows_are_rejected(self):
        row = json.loads(trace_bytes().splitlines()[0])
        variants = [
            {**row, "phase": "private-phase-sentinel"}, {**row, "endpoint": "private-sentinel"},
            {**row, "attempt": True}, {**row, "elapsed_ms": -1}, {**row, "connection": 0},
            {**row, "phase": []}, {**row, "elapsed_ms": 2**64},
        ]
        for value in variants:
            with self.subTest(value=value), self.assertRaises(ValueError):
                diagnostics.validate_trace(json.dumps(value).encode() + b"\n")
        with self.assertRaises(ValueError):
            diagnostics.validate_trace(trace_bytes() + trace_bytes())
        with self.assertRaises(ValueError):
            diagnostics.diagnostic_row(json.dumps(row)[:-1] + ', "attempt": 2}')
        with self.assertRaisesRegex(ValueError, "missing usable inventory"):
            diagnostics.validate_trace(trace_bytes().splitlines()[0] + b"\n", require_ready=True)


if __name__ == "__main__":
    unittest.main()
