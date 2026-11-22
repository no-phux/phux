"""Diagnostic-only harness tests. Native commands and sampling are inert stubs."""
import importlib.util
import hashlib
import io
import json
from pathlib import Path
import plistlib
import stat
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import zipfile


def load(name):
    path = Path(__file__).resolve().parents[1] / "ios" / (name + ".py")
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


diagnose = load("diagnose_commands")
artifact = load("diagnose_artifact")


def identity(pid=123, microseconds=10):
    return {"pid": pid, "start_seconds": 100, "start_microseconds": microseconds,
            "executable": "/owned/simctl"}


class CommandDiagnosticsTests(unittest.TestCase):
    def lane(self, root):
        return SimpleNamespace(out=root, sequence=0, env={"PATH": "/usr/bin:/bin"},
                               server=None, diagnostic_pid=None, udid="OWNED", phux=root / "phux")

    def command(self, root, outcome):
        lane = self.lane(root)
        process = Mock(pid=123)
        self.last_process = process
        process.wait.side_effect = outcome
        process.poll.return_value = None
        observer = Mock()
        with patch.object(diagnose.subprocess, "Popen", return_value=process), \
                patch.object(diagnose, "process_identity", return_value=identity()), \
                patch.object(diagnose, "Observation", return_value=observer):
            try:
                diagnose.diagnostic_run(lane, ["/owned/simctl", "spawn", "OWNED", "launchctl", "list"])
            finally:
                observer.stop.assert_called_once()
        return process

    def test_success_records_owned_pid_identity_and_budget_without_killing(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            process = self.command(root, [0])
            process.kill.assert_not_called()
            record = json.loads((root / "command-001.timing.json").read_text())
            self.assertEqual(record["identity"], identity())
            self.assertEqual(record["pid"], 123)
            self.assertEqual(record["deadline_seconds"], 30)
            self.assertTrue(record["succeeded"])
            self.assertLessEqual(process.wait.call_args.kwargs["timeout"], 30)

    def test_timeout_is_same_exception_and_owned_child_is_killed_and_reaped(self):
        timeout = subprocess.TimeoutExpired("owned-command", 30)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaises(subprocess.TimeoutExpired) as raised:
                self.command(root, [timeout, -9])
            self.assertIs(raised.exception, timeout)
            self.last_process.kill.assert_called_once()
            self.assertEqual(self.last_process.wait.call_count, 2)
            record = json.loads((root / "command-001.timing.json").read_text())
            self.assertFalse(record["succeeded"])

    def test_nonzero_exit_remains_called_process_error(self):
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaises(subprocess.CalledProcessError) as raised:
                self.command(Path(temporary), [7, 7])
            self.assertEqual(raised.exception.returncode, 7)

    def test_reused_or_exited_pid_never_sampled(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            observation = diagnose.Observation(self.lane(root), Mock(pid=123), root / "command-001",
                                              identity(), {"command": identity()})
            observation.helper = Mock()
            for current in (None, identity(microseconds=11)):
                with self.subTest(current=current), \
                        patch.object(diagnose, "process_identity", return_value=current):
                    observation.sample("command", identity())
            observation.helper.assert_not_called()
            self.assertFalse(diagnose.same_process(None, None))

    def test_server_start_identity_is_not_reacquired_after_pid_reuse(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            lane.server = Mock(pid=123)
            lane.server.poll.return_value = None
            old = {**identity(), "executable": str(lane.phux.resolve())}
            with patch.object(diagnose, "process_identity", return_value=old):
                self.assertIn("server", diagnose.owned_targets(lane, None))
            with patch.object(diagnose, "process_identity", return_value={**old, "start_microseconds": 11}):
                self.assertNotIn("server", diagnose.owned_targets(lane, None))

    def test_app_path_must_belong_to_exact_owned_device_and_bundle(self):
        lane = SimpleNamespace(udid="OWNED", app=Path("/build/Phux Rune.app"), app_executable="PhuxRune")
        self.assertTrue(diagnose.owned_app(lane, "/Devices/owned/data/Phux Rune.app/PhuxRune"))
        self.assertFalse(diagnose.owned_app(lane, "/Devices/other/data/Phux Rune.app/PhuxRune"))

    def test_stop_cancels_and_reaps_helper_without_waiting_for_observation_delay(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            observation = diagnose.Observation(self.lane(root), Mock(pid=123), root / "command-001",
                                              identity(), {})
            helper = Mock()
            helper.poll.return_value = None
            observation.helpers.append(helper)
            observation.thread.start()
            observation.stop()
            helper.kill.assert_called_once()
            self.assertFalse(observation.thread.is_alive())

    def test_helpers_are_bounded_and_reaped_on_timeout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            observation = diagnose.Observation(self.lane(root), Mock(pid=123), root / "command-001",
                                              identity(), {})
            helper = Mock(pid=456)
            helper.wait.side_effect = [subprocess.TimeoutExpired("inert-sample", 2), -9]
            with patch.object(diagnose.subprocess, "Popen", return_value=helper), \
                    patch.object(diagnose, "process_identity", return_value=identity(456)):
                observation.helper("inert", ["/inert/native-command-stub"])
            helper.kill.assert_called_once()
            self.assertEqual(helper.wait.call_count, 2)
            self.assertEqual(helper.wait.call_args_list[0].kwargs["timeout"], 2)

    def test_success_and_failure_never_leave_release_receipt(self):
        for fail in (False, True):
            with self.subTest(fail=fail), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                app = root / "app"
                app.mkdir()
                (app / "Info.plist").write_bytes(plistlib.dumps({"CFBundleExecutable": "PhuxRune"}))
                original_error = ValueError("original inert qualification failure")
                calls = []

                def qualify(app, phux, directory, transport):
                    calls.append(transport)
                    directory.mkdir()
                    (directory / "evidence.json").write_text("{}")
                    if fail:
                        raise original_error

                module = SimpleNamespace(Qualification=object, qualify=qualify)
                with patch.object(diagnose, "load_qualifier", return_value=module):
                    if fail:
                        with self.assertRaises(ValueError) as raised:
                            diagnose.run_sequence(app, root / "phux", root / "out")
                        self.assertIs(raised.exception, original_error)
                    else:
                        diagnose.run_sequence(app, root / "phux", root / "out")
                self.assertEqual(calls, ["quic"] if fail else ["quic", "wss"])
                self.assertFalse(list((root / "out").rglob("evidence.json")))
                status = json.loads((root / "out/quic-diagnostic-status.json").read_text())
                self.assertFalse(status["release_qualification"])
                self.assertEqual(status["sequence_completed"], not fail)


class ArtifactTests(unittest.TestCase):
    def archive(self, names):
        data = io.BytesIO()
        with zipfile.ZipFile(data, "w") as archive:
            for name, mode in names:
                info = zipfile.ZipInfo(name)
                info.external_attr = mode << 16
                archive.writestr(info, b"inert")
        data.seek(0)
        return zipfile.ZipFile(data)

    def test_archive_rejects_escape_duplicates_symlinks_special_modes_and_collisions(self):
        regular = stat.S_IFREG | 0o644
        cases = [
            [("../escape", regular)], [("/absolute", regular)], [("a\\b", regular)],
            [("duplicate", regular), ("DUPLICATE", regular)],
            [("link", stat.S_IFLNK | 0o755)], [("setuid", stat.S_IFREG | 0o4755)],
            [("a", regular), ("a/b", regular)],
        ]
        for members in cases:
            with self.subTest(members=members), self.archive(members) as archive:
                with self.assertRaises(ValueError):
                    artifact.checked_members(archive)

    def test_metadata_requires_exact_digest_size_name_and_run_head(self):
        metadata = {"id": artifact.ARTIFACT, "name": "qualification-ios-native",
                    "size_in_bytes": artifact.SIZE, "digest": "sha256:" + artifact.SHA256,
                    "expired": False, "workflow_run": {
                        "id": artifact.RUN, "head_sha": artifact.HEAD,
                        "repository_id": 1249328322, "head_repository_id": 1249328322}}
        artifact.validate_metadata(metadata)
        for key, value in (("digest", "wrong"), ("size_in_bytes", 1), ("name", "wrong")):
            with self.subTest(key=key), self.assertRaises(ValueError):
                artifact.validate_metadata({**metadata, key: value})
        metadata["workflow_run"]["head_sha"] = "wrong"
        with self.assertRaises(ValueError):
            artifact.validate_metadata(metadata)

    def test_extracts_only_original_app_and_build_restoring_executable_mode(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "original.zip"
            with zipfile.ZipFile(source, "w") as archive:
                binary = zipfile.ZipInfo(artifact.APP + "/PhuxRune")
                binary.external_attr = (stat.S_IFREG | 0o755) << 16
                archive.writestr(binary, b"inert native stub, never launched")
                archive.writestr("ios-simulator/build.json", json.dumps({
                    "revision": artifact.BUILD, "target": "aarch64-apple-ios-sim"}))
                archive.writestr("ios-qualification/server.log", "do not extract")
            # ZipFile.writestr's default mode has no file type; the immutable
            # original has explicit regular-file types. Match that in this stub.
            with zipfile.ZipFile(source) as archive:
                members = [(i.filename, archive.read(i), i.external_attr) for i in archive.infolist()]
            with zipfile.ZipFile(source, "w") as archive:
                for name, data, mode in members:
                    info = zipfile.ZipInfo(name)
                    info.external_attr = (stat.S_IFREG | stat.S_IMODE(mode >> 16)) << 16
                    archive.writestr(info, data)
            app = artifact.extract_app(source, root / "extracted")
            self.assertEqual(stat.S_IMODE((app / "PhuxRune").stat().st_mode), 0o755)
            self.assertFalse((root / "extracted/ios-qualification").exists())

    def test_token_sent_only_to_api_not_blob(self):
        redirect = SimpleNamespace(
            open=Mock(side_effect=__import__("urllib").error.HTTPError(
                artifact.API, 302, "redirect", {"Location": "https://blob.example/download"}, None)))
        with patch.object(artifact.urllib.request, "build_opener", return_value=redirect):
            self.assertEqual(artifact.download_url("private-token"), "https://blob.example/download")
        request = redirect.open.call_args.args[0]
        self.assertEqual(request.get_header("Authorization"), "Bearer private-token")

    def test_download_checks_bytes_sha_and_overflow_without_forwarding_api_token(self):
        data = b"inert original archive stub"
        for body, digest, succeeds in (
                (data, hashlib.sha256(data).hexdigest(), True),
                (data, "wrong", False), (data + b"overflow", "wrong", False)):
            with self.subTest(succeeds=succeeds), tempfile.TemporaryDirectory() as temporary:
                with patch.object(artifact, "SIZE", len(data)), \
                        patch.object(artifact, "SHA256", digest), \
                        patch.object(artifact.urllib.request, "urlopen", return_value=io.BytesIO(body)) as open_url:
                    target = Path(temporary) / "original.zip"
                    if succeeds:
                        artifact.save_archive("https://blob.example/download", target)
                        self.assertEqual(target.read_bytes(), data)
                    else:
                        with self.assertRaises(ValueError):
                            artifact.save_archive("https://blob.example/download", target)
                    self.assertEqual(open_url.call_args.args, ("https://blob.example/download",))


class WorkflowRoutingTests(unittest.TestCase):
    def test_only_explicit_manual_diagnostics_skip_plan_without_changing_normal_build_or_receipt(self):
        root = Path(__file__).resolve().parents[3]
        workflow = (root / ".github/workflows/rune-macos.yml").read_text()
        mode = "github.event_name == 'workflow_dispatch' && inputs.native_diagnostic"
        self.assertIn("native_diagnostic:\n", workflow)
        self.assertIn("        type: boolean\n        default: false", workflow)
        self.assertIn("    if: " + mode, workflow)
        self.assertIn("    if: github.event.action != 'closed' && !(" + mode + ")", workflow)
        self.assertIn(" && 'diagnostic:' || ''", workflow)
        self.assertIn("github.event_name != 'push' && !(" + mode + ")", workflow)
        self.assertIn("if: (github.event_name != 'pull_request' || inputs.qualify) && "
                      "needs.plan.outputs.macos == 'true'", workflow)
        self.assertIn('.plan.result == "success"', workflow)
        self.assertEqual(workflow.count("needs.plan.outputs.coverage != ''"), 2)
        child = (root / ".github/workflows/rune-native-diagnose.yml").read_text()
        self.assertIn("on:\n  workflow_call:", child)
        self.assertNotIn("workflow_dispatch:", child)
        self.assertNotIn("secrets:", child)
        self.assertIn("timeout-minutes: 90", child)
        self.assertIn("name: diagnostic-ios-command-stalls", child)


if __name__ == "__main__":
    unittest.main()
