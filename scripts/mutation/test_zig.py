#!/usr/bin/env python3
"""Runner contracts; PHUX_ZIG_MUTATION_INTEGRATION=1 also exercises pinned Zig/Zentinel."""

import argparse
import importlib.util
import io
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("zig_runner", Path(__file__).with_name("zig.py"))
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)
INSTALLER_SPEC = importlib.util.spec_from_file_location("zig_installer", Path(__file__).with_name("zig-tool.py"))
installer = importlib.util.module_from_spec(INSTALLER_SPEC)
INSTALLER_SPEC.loader.exec_module(installer)
MODULE = "clients/cockpit/src/cockpit/native/ts_protocol.zig"


class InstallerTests(unittest.TestCase):
    def test_checksum_mismatch_cannot_build_or_install(self):
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.dict(os.environ, {"XDG_CACHE_HOME": temporary}),
            patch.object(installer.subprocess, "check_output", return_value="0.16.0\n"),
            patch.object(installer.urllib.request, "urlopen", return_value=io.BytesIO(b"corrupt archive")),
            patch.object(installer.subprocess, "run") as build,
        ):
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                installer.install()
            build.assert_not_called()
            self.assertEqual(list(Path(temporary).rglob("install-*")), [])
            self.assertEqual(list(Path(temporary).rglob("binary.sha256")), [])


class ScopeTests(unittest.TestCase):
    def test_non_zig_diff_noops_without_installing_tool(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(["git", "init", "-q", root], check=True)
            subprocess.run(["git", "-C", root, "-c", "user.name=Test", "-c",
                            "user.email=test@example.org", "commit", "--allow-empty", "-qm", "base"], check=True)
            (root / "docs.txt").write_text("change")
            subprocess.run(["git", "-C", root, "add", "docs.txt"], check=True)
            args = argparse.Namespace(diff="HEAD", scope=None)
            with patch.object(runner, "ROOT", root):
                self.assertEqual(runner.select_targets(args, [MODULE]), [])

    def test_invalid_ref_fails(self):
        with self.assertRaises(subprocess.CalledProcessError):
            runner.changed_files("mutation-ref-that-does-not-exist")

    def test_explicit_scope_cannot_escape_pilot(self):
        for scope in ["../outside.zig", "clients/cockpit/src/ts_engine.zig", "/etc/passwd"]:
            with self.subTest(scope=scope), self.assertRaisesRegex(ValueError, "outside standalone"):
                runner.select_targets(argparse.Namespace(diff=None, scope=[scope]), [MODULE])

    def test_refuses_to_overwrite_artifacts(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            report = root / "summary.json"
            report.write_text("previous evidence")
            with self.assertRaisesRegex(ValueError, "must be empty"):
                runner.prepare_output(root)
            self.assertEqual(report.read_text(), "previous evidence")

    def test_stage_copies_actual_source_without_rewriting(self):
        with tempfile.TemporaryDirectory() as temporary:
            stage = Path(temporary)
            runner.prepare_project(stage, [MODULE])
            self.assertEqual((stage / MODULE).read_bytes(), (runner.ROOT / MODULE).read_bytes())


class CleanupTests(unittest.TestCase):
    def test_timeout_reaps_grandchild(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            child = root / "child.py"
            child.write_text("import os,time,signal\nsignal.signal(signal.SIGTERM,signal.SIG_IGN)\nopen('grandchild.pid','w').write(str(os.getpid()))\ntime.sleep(60)\n")
            parent = root / "parent.py"
            parent.write_text("import subprocess,sys,time\nsubprocess.Popen([sys.executable,'child.py'],start_new_session=True)\ntime.sleep(60)\n")
            with self.assertRaises(subprocess.TimeoutExpired):
                runner.run_command([sys.executable, str(parent)], root, 1, root / "log")
            pid = int((root / "grandchild.pid").read_text())
            self.assert_process_stopped(pid)

    def test_sigterm_reaps_separate_group_resistant_descendant(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "child.py").write_text(
                "import os,time,signal\nsignal.signal(signal.SIGTERM,signal.SIG_IGN)\n"
                "open('grandchild.pid','w').write(str(os.getpid()))\ntime.sleep(60)\n")
            (root / "parent.py").write_text(
                "import subprocess,sys,time\nsubprocess.Popen([sys.executable,'child.py'],start_new_session=True)\ntime.sleep(60)\n")
            script = root / "launch.py"
            script.write_text(
                f"import sys,signal\nsys.path.insert(0,{str(Path(__file__).parent)!r})\n"
                "import zig\nfrom pathlib import Path\n"
                "signal.signal(signal.SIGTERM,zig.interrupted)\n"
                "zig.run_command([sys.executable,'parent.py'],Path.cwd(),60,Path('log'))\n")
            with subprocess.Popen([sys.executable, "-B", script], cwd=root, stderr=subprocess.DEVNULL) as launch:
                try:
                    self.wait_for_file(root / "grandchild.pid")
                    time.sleep(0.2)  # Let the descendant observation happen.
                    launch.send_signal(signal.SIGTERM)
                    self.assertNotEqual(launch.wait(timeout=5), 0)
                    self.assert_process_stopped(int((root / "grandchild.pid").read_text()))
                finally:
                    launch.kill()

    def wait_for_file(self, path):
        for _ in range(100):
            if path.exists():
                return
            time.sleep(0.02)
        self.fail(f"child did not start: {path}")

    def assert_process_stopped(self, pid):
        # Reparenting/reaping is asynchronous on macOS; a zombie is stopped.
        for _ in range(50):
            result = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                                    capture_output=True, text=True, check=False)
            if not result.stdout.strip() or result.stdout.strip().startswith("Z"):
                return
            time.sleep(0.02)
        self.fail(f"descendant {pid} still running")


@unittest.skipUnless(os.environ.get("PHUX_ZIG_MUTATION_INTEGRATION") == "1", "opt-in real mutation integration")
class RealToolTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workspace = tempfile.TemporaryDirectory(prefix="phux-zig-proof-")
        cls.root = Path(cls.workspace.name)
        cls.binary = runner.tool_binary(cls.root)

    @classmethod
    def tearDownClass(cls):
        cls.workspace.cleanup()

    def project(self, name, source, operators, timeout=10):
        stage = self.root / name
        stage.mkdir()
        (stage / "fixture.zig").write_text(source)
        out = self.root / (name + "-out")
        out.mkdir()
        runner.configuration(stage, ["fixture.zig"], operators, timeout)
        return stage, out

    def test_actual_cockpit_module_kills_generated_mutation(self):
        stage = self.root / "cockpit"
        stage.mkdir()
        out = self.root / "cockpit-out"
        out.mkdir()
        runner.prepare_project(stage, [MODULE])
        runner.configuration(stage, [MODULE], ["equality_swap"], 30)
        summary = runner.execute_project(self.binary, stage, out, 1, 30)
        self.assertEqual(summary["outcomes"], {"killed": 1})
        report = json.loads((out / "mutant-001.json").read_text())
        self.assertEqual(report["baseline"]["status"], "passed")
        self.assertFalse(report["diagnostics"]["cache"]["enabled"])

    def test_under_tested_fixture_survives(self):
        source = '''const std = @import("std");
fn add(a: u8, b: u8) u8 { return a + b; }
test "only tests the zero identity" { try std.testing.expectEqual(@as(u8, 2), add(2, 0)); }
'''
        stage, out = self.project("survivor", source, ["arithmetic_add_sub"])
        summary = runner.execute_project(self.binary, stage, out, 1, 10)
        self.assertEqual(summary["outcomes"], {"survived": 1})

    def test_compile_error_is_not_killed(self):
        source = '''const std = @import("std");
fn number() u8 { return if (true) 1 else "not an integer"; }
test "value" { try std.testing.expectEqual(@as(u8, 1), number()); }
'''
        stage, out = self.project("compile-error", source, ["boolean_literal"])
        summary = runner.execute_project(self.binary, stage, out, 1, 10)
        self.assertEqual(summary["outcomes"], {"compile_error": 1})

    def test_hanging_mutant_is_timeout_and_workspace_removed(self):
        source = '''fn done() bool { return true; }
test "terminates" { while (!done()) {} }
'''
        stage, out = self.project("timeout", source, ["boolean_literal"], timeout=5)
        summary = runner.execute_project(self.binary, stage, out, 1, 5)
        self.assertEqual(summary["outcomes"], {"timeout": 1})
        self.assertEqual(list((stage / ".zig-cache/zentinel/workspaces").glob("run_*")), [])

    def test_broken_baseline_aborts_instead_of_claiming_kill(self):
        source = '''const std = @import("std");
fn value() bool { return true; }
test "broken baseline" { try std.testing.expect(!value()); }
'''
        stage, out = self.project("bad-baseline", source, ["boolean_literal"])
        with self.assertRaisesRegex(ValueError, "baseline failed"):
            runner.execute_project(self.binary, stage, out, 1, 10)

    def test_real_cli_sigterm_removes_staging_tree(self):
        temporary = self.root / "cli-temporary"
        temporary.mkdir()
        out = self.root / "cli-abort-out"
        env = dict(os.environ, TMPDIR=str(temporary))
        with subprocess.Popen([sys.executable, str(runner.ROOT / "scripts/mutation/zig.py"),
                               "--out", str(out)], env=env, stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL) as launch:
            try:
                self.wait_for_stage(temporary)
                time.sleep(0.2)
                launch.send_signal(signal.SIGTERM)
                self.assertEqual(launch.wait(timeout=5), 2)
                self.assertEqual(list(temporary.iterdir()), [])
                self.assertEqual(json.loads((out / "summary.json").read_text())["status"], "error")
            finally:
                launch.kill()

    def wait_for_stage(self, temporary):
        for _ in range(200):
            if list(temporary.glob("phux-zig-mutation-*/zentinel.toml")):
                return
            time.sleep(0.02)
        self.fail("CLI did not begin a real mutation run")


if __name__ == "__main__":
    unittest.main()
