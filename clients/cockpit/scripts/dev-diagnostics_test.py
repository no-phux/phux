#!/usr/bin/env python3
"""Offline diagnostics acceptance: fixtures and child CLI stubs, never the app."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from lib import dev_diagnostics as d


SNAPSHOT = b'''ready=true protocol=0x0123456789abcdef frame=12 commands=2 runtime_uptime_ns=500 dispatch_errors=1 dropped_trace_records=0 publisher_pid=42 markup_watch=armed
window @w1 "PRIVATE WINDOW" bounds=(0,0 100x100) focused=true frame=12 commands=2
  view @w1/phux-cockpit-canvas kind=gpu_surface role="panel" accessibility_label="SECRET" text="TERMINAL PAYLOAD" bounds=(0,0 100x100) focused=true gpu_present_path=packet gpu_frame=12 gpu_nonblank=true
    widget @w1/phux-cockpit-canvas#123 role=textbox name="PRIVATE TITLE" bounds=(0,0 100x100) focused=true enabled=true text_value="PASTE SECRET" selected=true
  error event=key name=Failed detail="KEYSTROKE SECRET" timestamp_ns=4
'''


class PrivacyTests(unittest.TestCase):
    def test_snapshot_preserves_identity_and_diagnostics_without_payload(self):
        value = d.sanitize_snapshot(SNAPSHOT, 42)
        serialized = json.dumps(value)
        for secret in ("PRIVATE", "SECRET", "PAYLOAD", "text_value", "detail"):
            self.assertNotIn(secret, serialized)
        self.assertEqual(value["header"]["publisher_pid"], "42")
        self.assertEqual(value["records"][1]["gpu_present_path"], "packet")
        self.assertEqual(value["records"][2]["address"], "@w1/phux-cockpit-canvas#123")
        self.assertEqual(value["records"][2]["focused"], "true")
        self.assertEqual(value["input_scope_observed"], "unavailable")

    def test_hostile_quoted_and_multiline_text_cannot_copy_payload(self):
        hostile = SNAPSHOT.replace(b"TERMINAL PAYLOAD", b'private focused=false gpu_frame=999\nSECRET\n')
        hostile += b'  view @w2/private-secret kind=gpu_surface text="SECRET"\n'
        value = d.sanitize_snapshot(hostile, 42)
        serialized = json.dumps(value)
        self.assertNotIn("private", serialized)
        self.assertNotIn("SECRET", serialized)
        self.assertNotIn("999", serialized)

    def test_wrong_publisher_empty_and_unrecognized_snapshot_refused(self):
        for raw in (b"", SNAPSHOT.replace(b"publisher_pid=42", b"publisher_pid=99"),
                    SNAPSHOT.splitlines()[0], b"private\n" + SNAPSHOT):
            with self.subTest(raw=raw[:30]), self.assertRaises(d.EvidenceError):
                d.sanitize_snapshot(raw, 42)

    def test_log_retention_has_no_raw_lines(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = Path(tmp) / "log"
            log.write_text("warning: SECRET CLIPBOARD\nzero_canvas_ui payload=TERMINAL\n")
            value = d.log_summary(log)
            self.assertEqual(value["category_counts"]["zero_canvas_ui"], 1)
            self.assertNotIn("SECRET", json.dumps(value))
            self.assertNotIn("TERMINAL", json.dumps(value))
            self.assertFalse(d.log_summary(Path(tmp) / "absent")["available"])


class RunTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cockpit-diagnostics-")
        self.addCleanup(self.temp.cleanup)
        repo = Path(self.temp.name).resolve()
        self.root = repo / "clients/cockpit"
        self.root.mkdir(parents=True)
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        (repo / ".gitignore").write_text(".dev-run/\n.zig-cache/\n")
        self.binary = self.root / "phux-cockpit"
        self.binary.write_bytes(b"fixture Mach-O")
        self.native = self.root / "native"
        self.native.write_text("#!/usr/bin/env python3\nimport sys\n"
                               "assert sys.argv[1:] == ['automate', 'snapshot']\n"
                               f"sys.stdout.buffer.write({SNAPSHOT!r})\n")
        self.native.chmod(0o755)
        dropbox = self.root / ".zig-cache/native-sdk-automation"
        dropbox.mkdir(parents=True)
        self.snapshot_file = dropbox / "snapshot.txt"
        self.snapshot_file.write_bytes(SNAPSHOT)
        (self.root / "build.zig.zon").write_text('.{ .native_sdk = .{ .url = "https://github.com/phall1/native/archive/' + "a" * 40 + '.tar.gz" }, }\n')
        subprocess.run(["git", "-C", str(repo), "add", "."], check=True)
        subprocess.run(["git", "-C", str(repo), "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                        "commit", "-qm", "fixture"], check=True)
        self.process = {"pid": 42, "executable": str(self.binary), "started": "fixture start",
                        "cwd": str(self.root), "started_unix": 0}
        self.options = {"pid": 42, "native": str(self.native), "binary": str(self.binary),
                        "ffi_lib": None, "log": None, "socket": None, "phux_cli": None,
                        "require_markup_watch": True}
        self.proc_patch = patch.object(d, "process_identity", return_value=self.process)
        self.proc_patch.start()
        self.addCleanup(self.proc_patch.stop)
        self.pids_patch = patch.object(d, "live_publishers", return_value=[42])
        self.pids = self.pids_patch.start()
        self.addCleanup(self.pids_patch.stop)

    def start(self):
        return d.new_run(self.root, self.options)

    def test_real_stub_cli_bind_mark_retention_and_dirty_source(self):
        first, second = self.start(), self.start()
        self.assertNotEqual(first, second)
        manifest = json.loads((first / "run.json").read_text())
        self.assertFalse(manifest["source"]["dirty"])
        self.assertIn("unknown", manifest["loaded_build_configuration"])
        self.assertFalse(manifest["linked_ffi_verified"])
        self.assertEqual(manifest["sdk_inputs"]["declared_commit"], "a" * 40)
        (self.root / "edit.native").write_text("SECRET SOURCE")
        a, valid = d.capture(first, "mark-problem", "@w1/phux-cockpit-canvas#123", "terminal")
        self.assertTrue(valid)
        b, valid = d.capture(first, "sample")
        self.assertTrue(valid)
        self.assertNotEqual(a, b)
        self.assertTrue(a.exists())
        report = json.loads(a.read_text())
        self.assertTrue(report["source_now"]["dirty"])
        self.assertEqual(report["input_scope_declared"], "terminal")
        self.assertNotIn("SECRET", a.read_text())

    def test_second_publisher_refused_before_cli_and_retained(self):
        run = self.start()
        self.pids.return_value = [42, 43]
        path, valid = d.capture(run, "mark-problem")
        self.assertFalse(valid)
        report = json.loads(path.read_text())
        self.assertNotIn("snapshot", report)
        self.assertEqual(report["refusal"], "single-publisher check failed")

    def test_binary_replaced_after_bind_is_not_current_source_evidence(self):
        run = self.start()
        self.binary.write_bytes(b"new build from a different source")
        path, valid = d.capture(run, "mark-problem")
        self.assertFalse(valid)
        self.assertIn("on-disk artifact changed", json.loads(path.read_text())["refusal"])

    def test_pid_reuse_and_mid_capture_publisher_swap_refused(self):
        run = self.start()
        with patch.object(d, "process_identity", return_value={**self.process, "started": "new start"}):
            _, valid = d.capture(run, "mark-problem")
            self.assertFalse(valid)
        self.pids.side_effect = [[42], [42, 43]]
        path, valid = d.capture(run, "mark-problem")
        self.assertFalse(valid)
        self.assertNotIn("snapshot", json.loads(path.read_text()))

    def test_runtime_watch_off_rejected_without_relabeling_build(self):
        self.native.write_text(self.native.read_text().replace("markup_watch=armed", "markup_watch=off"))
        run = self.start()
        path, valid = d.capture(run, "mark-problem")
        self.assertFalse(valid)
        self.assertIn("watcher is not armed", json.loads(path.read_text())["refusal"])

    def test_capture_lock_refuses_concurrent_owner(self):
        run = self.start()
        with d.exclusive(self.root / ".dev-run/diagnostics/capture.lock"):
            with self.assertRaisesRegex(d.EvidenceError, "owns this dropbox"):
                d.capture(run, "mark-problem")

    def test_cached_snapshot_from_reused_pid_is_refused(self):
        run = self.start()
        os.utime(self.snapshot_file, ns=(0, 0))
        path, valid = d.capture(run, "mark-problem")
        self.assertFalse(valid)
        self.assertIn("predates publisher start", json.loads(path.read_text())["refusal"])

    def test_absent_target_is_not_bound_to_another_widget(self):
        run = self.start()
        path, valid = d.capture(run, "mark-problem", "@w1/phux-cockpit-canvas#999")
        self.assertFalse(valid)
        self.assertIn("target is absent", json.loads(path.read_text())["refusal"])

    def test_atomic_evidence_never_replaces_previous_file(self):
        path = self.root / "evidence.json"
        d.write_json(path, {"first": True})
        with self.assertRaises(FileExistsError):
            d.write_json(path, {"first": False})
        self.assertEqual(json.loads(path.read_text()), {"first": True})
        self.assertEqual(list(self.root.glob("*.partial")), [])

    def test_local_sdk_override_never_claims_ghostty_pin(self):
        (self.root / "build.zig.zon").write_text('.native_sdk = .{ .path = "../native" },\n'
                                                 '.ghostty = .{ .url = "https://github.com/ghostty-org/ghostty/archive/' + "b" * 40 + '.tar.gz" },\n')
        self.assertIsNone(d.sdk_identity(self.root)["declared_commit"])

    def test_coordinator_status_allowlists_pid_not_sessions_or_commands(self):
        self.options.update(phux_cli="phux", socket="/private/test.sock")
        with patch.object(d, "command", return_value=b'{"pid": 123, "sessions": [{"name": "SECRET"}]}'):
            result = d.coordinator_identity(self.options)
        self.assertEqual(result["pid_observed"], 123)
        self.assertIsNone(result["incarnation_observed"])
        self.assertNotIn("SECRET", json.dumps(result))

    def test_unavailable_coordinator_does_not_break_client_diagnostics(self):
        self.options.update(phux_cli="phux", socket="/private/test.sock")
        for raw in (b"not JSON SECRET", b"[]", b'{"pid": null}'):
            with self.subTest(raw=raw), patch.object(d, "command", return_value=raw):
                result = d.coordinator_identity(self.options)
            self.assertEqual(result["probe"], "unavailable")
            self.assertNotIn("SECRET", json.dumps(result))


class LauncherTests(unittest.TestCase):
    def test_no_build_banner_does_not_claim_requested_configuration(self):
        # Stop at the launch boundary with pure shell stubs: no app/compilers.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scripts = root / "scripts"
            (scripts / "lib").mkdir(parents=True)
            script = scripts / "dev-run.sh"
            script.write_text((d.ROOT / "scripts/dev-run.sh").read_text())
            binary = root / "binary"
            binary.write_bytes(b"old ReleaseSafe non-automation binary")
            (scripts / "lib/dev-app.sh").write_text('''DEV_APP_INSTALLED_BUNDLE=/not-installed
dev_app_home_init() { :; }
dev_app_stage() { printf '%s/binary\\n' "$ROOT"; }
dev_app_identity() { echo 'dev.test phux-cockpit-dev Test'; }
dev_app_state_path() { echo /test/state; }
dev_app_launch() { exit 23; }
''')
            (scripts / "lib/app-instance.sh").write_text("")
            result = subprocess.run(["bash", str(script), "--no-build", "--debug", "--automation", "--phux"],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 23, result.stderr)
            self.assertIn("optimize/provider/automation/FFI profile UNKNOWN", result.stdout)
            self.assertNotIn("build:      Debug", result.stdout)
            self.assertIn(d.digest(binary.read_bytes()), result.stdout)

    def test_no_build_measurement_refuses_before_any_build_or_app(self):
        result = subprocess.run(["bash", str(d.ROOT / "scripts/dev-run.sh"),
                                 "--no-build", "--measure-first-frame"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("requires a build", result.stderr)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
