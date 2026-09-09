"""Regression checks for fail-closed browser execution and bounded cleanup."""

import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch


spec = importlib.util.spec_from_file_location("web_browser", Path(__file__).with_name("web-browser.py"))
browser = importlib.util.module_from_spec(spec)
spec.loader.exec_module(browser)


class BrowserRunnerTests(unittest.TestCase):
    def test_node_success_is_not_browser_execution(self):
        with self.assertRaisesRegex(RuntimeError, "renders_engine_grid_to_canvas"):
            browser.require_browser_tests("no tests to run!\ntest result: ok. 0 passed")

    def test_every_required_browser_test_must_pass(self):
        output = "\n".join(f"test {name} ... ok" for name in browser.REQUIRED_TESTS)
        browser.require_browser_tests(output)
        with self.assertRaisesRegex(RuntimeError, "synthesized_only"):
            browser.require_browser_tests(output.replace("synthesized_only", "skipped_only"))

    def test_early_server_exit_is_not_readiness(self):
        server = Mock()
        server.poll.return_value = 1
        with self.assertRaisesRegex(RuntimeError, "exited before"):
            browser.wait_ready(server, 1)

    def test_readiness_is_bounded(self):
        server = Mock()
        server.poll.return_value = None
        with patch.object(browser, "websocket_ready", side_effect=ConnectionRefusedError) as probe:
            with patch.object(browser.time, "sleep"):
                with self.assertRaises(TimeoutError):
                    browser.wait_ready(server, 1)
        self.assertEqual(probe.call_count, 120)

    def test_readiness_waits_for_websocket_upgrade(self):
        server = Mock()
        server.poll.return_value = None
        with patch.object(browser, "websocket_ready", side_effect=[False, True]) as probe:
            with patch.object(browser.time, "sleep"):
                browser.wait_ready(server, 1)
        self.assertEqual(probe.call_count, 2)

    def test_command_failure_is_not_masked(self):
        with tempfile.TemporaryFile() as log:
            with self.assertRaises(subprocess.CalledProcessError):
                browser.run([sys.executable, "-c", "raise SystemExit(7)"],
                            cwd=browser.ROOT, env=os.environ, timeout=5, log=log)

    def test_timeout_reaps_child(self):
        with tempfile.TemporaryDirectory() as scratch:
            pid_file = Path(scratch) / "pid"
            script = "import os,time,pathlib; pathlib.Path('pid').write_text(str(os.getpid())); time.sleep(30)"
            with tempfile.TemporaryFile() as log:
                with self.assertRaises(subprocess.TimeoutExpired):
                    browser.run([sys.executable, "-c", script], cwd=scratch,
                                env=os.environ, timeout=1, log=log)
            pid = int(pid_file.read_text())
            with self.assertRaises(ProcessLookupError):
                os.kill(pid, 0)


if __name__ == "__main__":
    unittest.main()
