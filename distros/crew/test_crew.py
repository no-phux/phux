"""Credential-free launcher and board tests; never use the installed server."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import pty
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("crew", ROOT / "crew.py")
crew = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(crew)


class CrewTests(unittest.TestCase):
    def test_home_precedence(self):
        with patch.dict(os.environ, {"PHUX_CREW_HOME": "/crew/custom", "XDG_STATE_HOME": "/crew/state"}):
            self.assertEqual(crew.home_path(), Path("/crew/custom"))
            self.assertEqual(crew.home_path("/crew/explicit"), Path("/crew/explicit"))
        with patch.dict(os.environ, {"XDG_STATE_HOME": "/crew/state"}, clear=True):
            self.assertEqual(crew.home_path(), Path("/crew/state/phux/crew"))

    def test_prepare_is_repeatable_and_preserves_operator_policy(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            crew.prepare_home(home)
            self.assertIn(str(ROOT / "lead.md"), (home / "AGENTS.md").read_text())
            self.assertEqual((home / "CLAUDE.md").read_text(), "@AGENTS.md\n")
            policy = home / "POLICY.md"
            policy.write_text("Do not publish.\n")
            crew.prepare_home(home)
            self.assertEqual(policy.read_text(), "Do not publish.\n")

    def test_modified_agent_instructions_are_not_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            (home / "AGENTS.md").write_text("user work\n")
            with self.assertRaisesRegex(ValueError, "refusing to replace"):
                crew.prepare_home(home)
            self.assertEqual((home / "AGENTS.md").read_text(), "user work\n")

    def test_home_lease_excludes_a_second_lead_and_recovers(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            with crew.home_lease(home):
                with self.assertRaisesRegex(ValueError, "already owns"):
                    with crew.home_lease(home):
                        self.fail("second lead admitted")
            with crew.home_lease(home):
                pass

    def test_start_print_is_server_free_and_does_not_create_home(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory) / "not-created"
            result = subprocess.run(
                [sys.executable, str(ROOT / "crew.py"), "start", "--agent", "codex",
                 "--home", str(home), "--print", "--", "--model", "two words"],
                capture_output=True, text=True, check=True,
            )
            self.assertEqual(json.loads(result.stdout)["argv"], ["codex", "--model", "two words"])
            self.assertFalse(home.exists())

    def test_real_launch_has_home_environment_and_exact_argv(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = (root / "home with spaces").resolve()
            binary = root / "opencode"
            binary.write_text(
                f"#!{sys.executable}\nimport json,os,sys\n"
                "print(json.dumps({'cwd':os.getcwd(),'home':os.environ['PHUX_CREW_HOME'],"
                "'bundle':os.environ['PHUX_CREW_BUNDLE'],'args':sys.argv[1:]}))\n"
                "sys.exit(7)\n"
            )
            binary.chmod(0o755)
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"])
            result = subprocess.run(
                [sys.executable, str(ROOT / "crew.py"), "start", "--home", str(home),
                 "--agent", "opencode", "--", "literal; not a shell"],
                env=env, capture_output=True, text=True, check=False,
            )
            self.assertEqual(result.returncode, 7, result.stderr)
            report = json.loads(result.stdout.splitlines()[-1])
            self.assertEqual(report["cwd"], str(home))
            self.assertEqual(report["home"], str(home))
            self.assertEqual(report["bundle"], str(ROOT))
            self.assertEqual(report["args"], ["literal; not a shell"])
            with crew.home_lease(home):
                pass

    def test_missing_agent_creates_no_home(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory) / "home"
            args = crew.parser().parse_args(["start", "--home", str(home)])
            with patch.object(crew.shutil, "which", return_value=None):
                with self.assertRaisesRegex(ValueError, "not on PATH"):
                    crew.start(args)
            self.assertFalse(home.exists())

    def test_running_lead_holds_lease_and_forwards_termination(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = (root / "home").resolve()
            ready = root / "ready"
            binary = root / "opencode"
            binary.write_text(
                f"#!{sys.executable}\nimport signal,sys\nfrom pathlib import Path\n"
                "signal.signal(signal.SIGTERM, lambda *_: sys.exit(42))\n"
                f"Path({str(ready)!r}).write_text('ready')\n"
                "signal.pause()\n"
            )
            binary.chmod(0o755)
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"])
            process = subprocess.Popen(
                [sys.executable, str(ROOT / "crew.py"), "start", "--home", str(home), "--agent", "opencode"],
                env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            try:
                deadline = time.monotonic() + 5
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(ready.exists(), "fake agent started")
                with self.assertRaisesRegex(ValueError, "already owns"):
                    with crew.home_lease(home):
                        self.fail("second lead admitted")
                process.terminate()
                self.assertEqual(process.wait(timeout=5), 42)
                with crew.home_lease(home):
                    pass
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()

    def test_invalid_agent_environment_has_readable_error(self):
        with patch.dict(os.environ, {"PHUX_CREW_AGENT": "invalid"}):
            args = crew.parser().parse_args(["start", "--print"])
            with self.assertRaisesRegex(ValueError, "unknown agent"):
                crew.start(args)

    def test_start_refuses_a_home_inside_a_project_before_writing(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            (repo / ".git").mkdir()
            home = repo / "nested-home"
            args = crew.parser().parse_args(["start", "--home", str(home)])
            with patch.object(crew.shutil, "which", return_value="/fake/opencode"):
                with self.assertRaisesRegex(ValueError, "inside a git checkout"):
                    crew.start(args)
            self.assertFalse(home.exists())

    def test_terminal_ctrl_c_reaches_agent_once(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ready, count = root / "ready", root / "count"
            binary = root / "opencode"
            binary.write_text(
                f"#!{sys.executable}\nimport signal,sys\nfrom pathlib import Path\n"
                f"count = Path({str(count)!r})\n"
                "def interrupt(*_):\n count.write_text(count.read_text()+'x')\n"
                "count.write_text('')\n"
                "signal.signal(signal.SIGINT, interrupt)\n"
                "signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))\n"
                f"Path({str(ready)!r}).write_text('ready')\n"
                "while True: signal.pause()\n"
            )
            binary.chmod(0o755)
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"])
            pid, terminal = pty.fork()
            if pid == 0:
                os.execve(sys.executable,
                          [sys.executable, str(ROOT / "crew.py"), "start", "--home", str(root / "home"), "--agent", "opencode"], env)
            try:
                deadline = time.monotonic() + 5
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(ready.exists())
                os.write(terminal, b"\x03")
                deadline = time.monotonic() + 5
                while not count.read_text() and time.monotonic() < deadline:
                    time.sleep(0.01)
                time.sleep(0.1)  # A second forwarded interrupt must not follow.
                self.assertEqual(count.read_text(), "x")
            finally:
                os.kill(pid, signal.SIGTERM)
                os.waitpid(pid, 0)
                os.close(terminal)

    def test_direct_agent_retains_lease_after_launcher_crash(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = (root / "home").resolve()
            ready = root / "agent-pid"
            binary = root / "opencode"
            binary.write_text(
                f"#!{sys.executable}\nimport os,signal\nfrom pathlib import Path\n"
                f"Path({str(ready)!r}).write_text(str(os.getpid()))\n"
                "signal.pause()\n"
            )
            binary.chmod(0o755)
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"])
            process = subprocess.Popen(
                [sys.executable, str(ROOT / "crew.py"), "start", "--home", str(home), "--agent", "opencode"],
                env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            child_pid = None
            try:
                deadline = time.monotonic() + 5
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(ready.exists(), "fake direct agent started")
                child_pid = int(ready.read_text())
                process.kill()
                process.wait(timeout=5)
                with self.assertRaisesRegex(ValueError, "already owns"):
                    with crew.home_lease(home):
                        self.fail("orphaned agent lost its lease")
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=5)
                if child_pid is not None:
                    try:
                        os.kill(child_pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass

    def test_phux_is_an_executable_not_shell_and_preserves_socket(self):
        with patch.dict(os.environ, {"PHUX": "/build with spaces/phux", "PHUX_SOCKET": "/isolated/server.sock"}):
            self.assertEqual(crew.phux_command("agent", "list"), ["/build with spaces/phux", "agent", "list"])
            self.assertEqual(os.environ["PHUX_SOCKET"], "/isolated/server.sock")

    def test_status_keeps_unavailable_distinct_from_empty(self):
        ok = subprocess.CompletedProcess([], 0, '{"schema_version":1,"tasks":[]}', "")
        failed = subprocess.CompletedProcess([], 1, "", "isolated server unavailable")
        with patch.object(crew.subprocess, "run", side_effect=[ok, failed]):
            result = crew.status_document()
        self.assertTrue(result["missions"]["available"])
        self.assertFalse(result["agents"]["available"])
        self.assertIn("server unavailable", result["agents"]["error"])

    def test_status_handles_timeout_bad_json_and_missing_binary(self):
        failures = [subprocess.TimeoutExpired("phux", 10), FileNotFoundError("phux")]
        with patch.object(crew.subprocess, "run", side_effect=failures):
            self.assertFalse(crew.status_document()["missions"]["available"])
        bad = subprocess.CompletedProcess([], 0, "not JSON", "")
        with patch.object(crew.subprocess, "run", return_value=bad):
            self.assertFalse(crew.status_document()["agents"]["available"])

    def test_status_refuses_unknown_documents(self):
        for text in ('null', '{}', '{"schema_version":2,"tasks":[]}',
                     '{"schema_version":1,"tasks":[1]}'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                crew.parse_status("missions", text)

    def test_board_escapes_terminal_controls_and_uses_native_identity(self):
        data = {"agents": [{"terminal": "@7", "state": "blocked", "agent": {"id": "opencode"}, "address": "%lead\033[2J"}]}
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            crew.print_rows("agents", data)
        self.assertIn("opencode", output.getvalue())
        self.assertNotIn("\033", output.getvalue())
        self.assertIn("\\u001b", output.getvalue())

    def test_board_interval_is_bounded(self):
        for value in ("0", "-1", "3601"):
            with self.assertRaises(Exception):
                crew.positive_seconds(value)
        self.assertEqual(crew.positive_seconds("5"), 5)

    def test_all_skill_contracts_exist_and_are_internal(self):
        for name in ("crew-ship", "crew-scout", "crew-pulse"):
            text = (ROOT / "skills" / name / "SKILL.md").read_text()
            self.assertTrue(text.startswith(f"---\nname: {name}\n"))
            self.assertIn("internal: true", text)
            self.assertIn(f"skills/{name}/SKILL.md", (ROOT / "lead.md").read_text())


if __name__ == "__main__":
    unittest.main()
