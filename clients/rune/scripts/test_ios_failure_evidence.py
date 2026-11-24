"""Offline failure recovery: real waiters/cleanup, inert external commands."""
import contextlib
import hashlib
import io
import json
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

from test_ios_build import load, valid_report
from test_ios_diagnostics import trace_bytes

qualify = load("qualify")
failure = load("failure_evidence")
FIRST_FAILURE = b'{"schema":1,"error":"SYNTHETIC first native failure"}\n'


class FailureEvidenceFixture:
    def lane(self, root, prefix=""):
        # resolve only our test fixture: production capture must reject symlinks.
        root = root.resolve()
        out = root / "out"
        out.mkdir()
        lane = qualify.Qualification(root / "App", root / "phux", out, root)
        lane.documents = root / "Documents"
        lane.documents.mkdir()
        lane.udid = "EXACT-OWNED-SYNTHETIC-UDID"
        lane.diagnostic_pid = 123
        lane.diagnostic_prefixes = [prefix]
        lane.server = Mock()
        lane.server.poll.return_value = None
        lane.stop_server = Mock()
        lane.finish_connection_diagnostics = Mock()
        for name, data in (
                (prefix + "rune-connection-diagnostics.jsonl", trace_bytes()),
                (prefix + "rune-connection-diagnostics.status.json",
                 b'{"complete":true,"failed":false,"events":8}')):
            (lane.documents / name).write_bytes(data)
        return lane

    def retained(self, lane, name, prefix=""):
        return lane.out / failure.DIRECTORY / (prefix + name)

    def seed_native(self, lane, report=FIRST_FAILURE, tree=b'{"synthetic":"tree"}'):
        if report is not None:
            (lane.documents / "rune-qualification.json").write_bytes(report)
        if tree is not None:
            (lane.documents / "rune-tree.json").write_bytes(tree)
        (lane.documents / "rune-input-gate.json").write_bytes(
            b'{"schema":1,"frame_shown":true,"input_ready":true}')
        (lane.documents / "qualification-connection.json").write_bytes(b"CREDENTIAL-SENTINEL")
        (lane.documents / "preferences.json").write_bytes(b"PRIVATE-PREFERENCES")
        (lane.documents / "unexpected.json").write_bytes(b"UNLISTED-SENTINEL")

    def pipeline(self, lane):
        lane.app.mkdir()
        (lane.app / "Info.plist").write_bytes(plistlib.dumps({
            "CFBundleIdentifier": qualify.BUNDLE_ID,
            "CFBundleSupportedPlatforms": ["iPhoneSimulator"],
        }))
        lane.start_server = Mock(return_value="inert-owned-socket")
        lane.create_simulator = Mock()
        lane.launch = Mock(return_value=123)
        lane.failure_screenshot = Mock()
        lane.qualify_glass = Mock()
        lane.resume_identity = Mock()
        with patch.object(qualify, "Qualification", return_value=lane), \
                contextlib.redirect_stderr(io.StringIO()):
            qualify.qualify(lane.app, lane.phux, lane.out)

    def assert_private_allowlist(self, lane):
        directory = lane.out / failure.DIRECTORY
        self.assertEqual(directory.stat().st_mode & 0o777, 0o700)
        permitted = {row["file"] for row in lane.failure_evidence_receipt["files"]}
        for path in directory.iterdir():
            self.assertIn(path.name, permitted | {"receipt.json"})
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertNotIn(b"CREDENTIAL-SENTINEL", path.read_bytes())
            self.assertNotIn(b"PRIVATE-PREFERENCES", path.read_bytes())
        self.assertNotIn("qualification-connection.json", permitted)
        for row in lane.failure_evidence_receipt["files"]:
            if row["status"] == "UNVALIDATED":
                data = (directory / row["file"]).read_bytes()
                self.assertEqual(row["bytes"], len(data))
                self.assertEqual(row["sha256"], hashlib.sha256(data).hexdigest())


class FailureEvidenceTests(FailureEvidenceFixture, unittest.TestCase):
    def test_actual_collector_late_report_timeout_stays_primary_and_copies_after_shutdown(self):
        for prefix in ("", "resume-"):
            with self.subTest(prefix=prefix), tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary), prefix)
                queries = []
                actions = []
                final_error = None

                def health_query(pid, udid):
                    nonlocal final_error
                    self.assertEqual((pid, udid), (123, lane.udid))
                    queries.append(pid)
                    pending = lane.documents / "report.pending"
                    pending.write_bytes(FIRST_FAILURE)
                    pending.replace(lane.documents / "rune-qualification.json")
                    self.seed_native(lane)
                    final_error = subprocess.TimeoutExpired(["/bin/ps"], 10)
                    raise final_error

                def command(args, timeout=30, env=None):
                    self.assertEqual(args[:2], [qualify.XCRUN, "simctl"])
                    self.assertEqual(args[3], lane.udid)
                    actions.append(args[2])
                    self.assertEqual(timeout, 60)
                    if args[2] == "shutdown":
                        self.assertFalse((lane.out / failure.DIRECTORY).exists())
                        # A producer write at the shutdown boundary is retained.
                        (lane.documents / "rune-tree.json").write_bytes(b"last tree before quiescence")
                    if args[2] == "delete":
                        self.assertEqual(self.retained(lane, "rune-qualification.json", prefix).read_bytes(),
                                         FIRST_FAILURE)
                        self.assertEqual(self.retained(lane, "rune-tree.json", prefix).read_bytes(),
                                         b"last tree before quiescence")
                    return ""

                lane.run = Mock(side_effect=command)
                with patch.object(qualify, "app_running", side_effect=health_query):
                    if not prefix:
                        with self.assertRaises(subprocess.TimeoutExpired) as raised:
                            self.pipeline(lane)
                    else:
                        with self.assertRaises(subprocess.TimeoutExpired) as raised:
                            lane.collect_report(123, resumed=True)
                        lane.failure_evidence_requested = True
                        lane.cleanup()
                self.assertIs(raised.exception, final_error)
                self.assertEqual(len(queries), 1)
                self.assertEqual(actions, ["shutdown", "delete"])
                self.assertFalse((lane.out / "evidence.json").exists())
                self.assertFalse((lane.out / (prefix + "rune-qualification.json")).exists())
                self.assertEqual(lane.failure_evidence_receipt["status"], "UNVALIDATED")
                self.assertTrue(lane.failure_evidence_receipt["quiescent"])
                self.assert_private_allowlist(lane)

    def test_actual_scene_callers_recover_late_bytes_without_accepting_them(self):
        for prefix in ("", "resume-"):
            for kind in ("glass", "background", "lifecycle", "seal-status", "seal-background"):
                with self.subTest(prefix=prefix, kind=kind), tempfile.TemporaryDirectory() as temporary:
                    self.scene_race(Path(temporary), prefix, kind)

    def scene_race(self, root, prefix, kind):
        lane = self.lane(root, prefix)
        # Real cleanup must not replace a late lifecycle acknowledgement with
        # its later sealing acknowledgement before forensic preservation.
        lane.finish_connection_diagnostics = qualify._trace.finish_connection_diagnostics.__get__(lane)
        names = {"glass": "rune-glass.json", "background": "rune-background.json",
                 "lifecycle": "rune-lifecycle.json",
                 "seal-status": prefix + "rune-connection-diagnostics.status.json",
                 "seal-background": "rune-background.json"}
        name = names[kind]
        retained = {"seal-status": name, "background": prefix + "rune-background.before-seal.json"}.get(
            kind, prefix + name)
        (lane.documents / name).unlink(missing_ok=True)
        data = b'{"SYNTHETIC":"late scene receipt, NOT qualification proof"}'
        queries = []
        actions = []
        foregrounded = False

        def health_query(pid, udid):
            if kind == "lifecycle" and not foregrounded:
                return True
            queries.append(pid)
            (lane.documents / name).write_bytes(data)
            raise subprocess.TimeoutExpired(["/bin/ps"], 10)

        def launch_scene(bundle):
            nonlocal foregrounded
            finish_requested = (lane.documents / (prefix + "rune-connection-diagnostics.finish")).exists()
            if finish_requested and not kind.startswith("seal-"):
                (lane.documents / "rune-background.json").write_bytes(b'{"synthetic":"post-seal acknowledgement"}')
            elif kind == "lifecycle":
                if bundle == "com.apple.springboard":
                    (lane.documents / "rune-background.json").write_bytes(b'{"background_frames":3}')
                else:
                    foregrounded = True

        def check_cleanup_boundary(action):
            if action not in ("shutdown", "delete"):
                return
            actions.append(action)
            if action == "shutdown":
                self.assertFalse((lane.out / failure.DIRECTORY).exists())
            else:
                self.assertEqual((lane.out / failure.DIRECTORY / retained).read_bytes(), data)

        def command(args, timeout=30, env=None):
            if args[2] == "launch":
                launch_scene(args[4])
            check_cleanup_boundary(args[2])
            return ""

        lane.run = Mock(side_effect=command)
        with self.assertRaises(subprocess.TimeoutExpired), \
                patch.object(qualify, "app_running", side_effect=health_query):
            if kind == "glass":
                lane.qualify_glass(123)
            elif kind in ("background", "lifecycle"):
                lane.cycle_scene(lane.documents, 123, prefix)
            else:
                lane.finish_connection_diagnostics()
        lane.failure_evidence_requested = True
        lane.cleanup()
        self.assertEqual(len(queries), 1)
        self.assertEqual(actions, ["shutdown", "delete"])
        self.assertEqual(lane.failure_evidence_receipt["status"], "UNVALIDATED")
        self.assertFalse((lane.out / "evidence.json").exists())

    def test_missing_malformed_failure_and_missing_tree_never_become_success(self):
        cases = ((None, None), (b"{malformed", b"{truncated"), (b"\xffinvalid encoding", None), (FIRST_FAILURE, None),
                 (json.dumps(valid_report()).encode(), None))
        for report, tree in cases:
            with self.subTest(report=report), tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary))
                self.seed_native(lane, report, tree)
                lane.simctl = Mock()
                lane.require_alive = Mock(side_effect=TimeoutError("original host failure"))
                with self.assertRaises((ValueError, OSError)):
                    self.pipeline(lane)
                rows = {row["source"]: row for row in lane.failure_evidence_receipt["files"]}
                self.assertEqual(rows["rune-qualification.json"]["status"],
                                 "missing" if report is None else "UNVALIDATED")
                self.assertEqual(rows["rune-tree.json"]["status"],
                                 "missing" if tree is None else "UNVALIDATED")
                if report is not None:
                    self.assertEqual(self.retained(lane, "rune-qualification.json").read_bytes(), report)
                self.assertFalse((lane.out / "evidence.json").exists())
                lane.resume_identity.assert_not_called()
                self.assert_private_allowlist(lane)

    def test_existing_first_failure_is_never_overwritten_by_later_native_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            self.seed_native(lane)
            lane.simctl = Mock()
            lane.finish_connection_diagnostics.side_effect = lambda: (
                lane.documents / "rune-qualification.json").write_bytes(b'{"later":"not first failure"}')
            with self.assertRaisesRegex(ValueError, "SYNTHETIC first native failure"):
                self.pipeline(lane)
            self.assertEqual((lane.out / "rune-qualification.json").read_bytes(), FIRST_FAILURE)
            retained = self.retained(lane, "rune-qualification.json")
            self.assertEqual(retained.read_bytes(), b'{"later":"not first failure"}')
            receipt = lane.preserve_failure_evidence(quiescent=True)
            (lane.documents / "rune-qualification.json").write_bytes(b"changed again")
            self.assertEqual(lane.preserve_failure_evidence(quiescent=True), receipt)
            self.assertEqual(retained.read_bytes(), b'{"later":"not first failure"}')

    def test_preservation_errors_never_replace_original_host_failure_or_skip_delete(self):
        for error in (PermissionError("private-path-sentinel"), RuntimeError("unexpected sentinel")):
            with self.subTest(error=type(error).__name__), tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary))
                original = subprocess.TimeoutExpired(["inert-health-query"], 30)
                lane.require_alive = Mock(side_effect=original)
                lane.simctl = Mock()
                lane.preserve_failure_evidence = Mock(side_effect=error)
                with self.assertRaises(subprocess.TimeoutExpired) as raised:
                    self.pipeline(lane)
                self.assertIs(raised.exception, original)
                self.assertEqual(lane.simctl.call_args_list[-1].args, ("delete", lane.udid))
                errors = json.loads((lane.out / "cleanup.json").read_text())["errors"]
                self.assertIn("UNVALIDATED native failure evidence could not be retained", errors)
                self.assertNotIn("sentinel", json.dumps(errors))

if __name__ == "__main__":
    unittest.main()
