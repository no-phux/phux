"""Regression checks for fail-closed browser execution and bounded cleanup."""

import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch


spec = importlib.util.spec_from_file_location("web_browser", Path(__file__).with_name("web-browser.py"))
browser = importlib.util.module_from_spec(spec)
spec.loader.exec_module(browser)


class BrowserRunnerTests(unittest.TestCase):
    def test_chrome_binary_is_in_runner_capabilities(self):
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            env = browser.webdriver_environment({"CHROME": "/selected/chromium"}, directory, directory)
            capabilities = json.loads(Path(env["WASM_BINDGEN_TEST_WEBDRIVER_JSON"]).read_text())
            self.assertEqual(capabilities["goog:chromeOptions"]["binary"], "/selected/chromium")

    def test_user_capabilities_survive_binary_selection(self):
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            source = directory / "custom.json"
            original = {"acceptInsecureCerts": True,
                        "goog:chromeOptions": {"args": ["--disable-gpu"], "binary": "/old/chrome"}}
            source.write_text(json.dumps(original))
            env = browser.webdriver_environment(
                {"CHROME": "/selected/chromium", "WASM_BINDGEN_TEST_WEBDRIVER_JSON": "custom.json"},
                directory, directory)
            capabilities = json.loads(Path(env["WASM_BINDGEN_TEST_WEBDRIVER_JSON"]).read_text())
            self.assertTrue(capabilities["acceptInsecureCerts"])
            self.assertEqual(capabilities["goog:chromeOptions"]["args"], ["--disable-gpu"])
            self.assertEqual(capabilities["goog:chromeOptions"]["binary"], "/selected/chromium")
            self.assertEqual(json.loads(source.read_text()), original)

    def test_default_capabilities_are_preserved_without_chrome_override(self):
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            source = directory / "webdriver.json"
            original = {"goog:chromeOptions": {"binary": "/user/chrome", "args": ["--disable-gpu"]}}
            source.write_text(json.dumps(original))
            with tempfile.TemporaryDirectory() as output:
                env = browser.webdriver_environment({}, Path(output), directory)
                self.assertEqual(json.loads(Path(env["WASM_BINDGEN_TEST_WEBDRIVER_JSON"]).read_text()), original)
            self.assertEqual(json.loads(source.read_text()), original)

    def test_chrome_command_receives_temporary_capabilities(self):
        paths = []

        def inspect_command(_command, *, cwd, env, timeout, log):
            path = Path(env["WASM_BINDGEN_TEST_WEBDRIVER_JSON"])
            paths.append(path)
            self.assertEqual(json.loads(path.read_text())["goog:chromeOptions"]["binary"], "/selected/chromium")
            self.assertEqual(cwd, browser.ROOT / "clients/phux-web")
            self.assertEqual(env["CARGO_TARGET_DIR"], str(cwd / "target"))

        with tempfile.TemporaryDirectory() as scratch:
            with patch.object(browser, "run", side_effect=inspect_command):
                browser.run_chrome(["wasm-pack"], {"CHROME": "/selected/chromium", "TMPDIR": scratch}, Path(scratch))
            self.assertEqual(len(paths), 1)
            self.assertFalse(paths[0].exists(), "temporary capabilities must be removed after Chrome exits")

    def assert_descendant_stopped(self, pid):
        # Linux init may briefly retain an orphan zombie; it is no longer running.
        for _ in range(100):
            state = subprocess.run(["ps", "-p", str(pid), "-o", "stat="],
                                   capture_output=True, text=True, timeout=2).stdout.strip()
            if not state or state.startswith("Z"):
                return
            time.sleep(0.01)
        self.fail(f"SIGINT-ignoring descendant {pid} survived owned-group cleanup")

    def check_descendant_cleanup(self, parent_exits):
        child = "import os,signal,time; signal.signal(signal.SIGINT, signal.SIG_IGN); print(os.getpid(),flush=True); time.sleep(60)"
        parent = (
            "import os,subprocess,sys,time,pathlib; "
            "pathlib.Path('group').write_text(str(os.getpid())); "
            f"child=subprocess.Popen([sys.executable,'-c',{child!r}],stdout=subprocess.PIPE,text=True); "
            "pathlib.Path('child').write_text(child.stdout.readline()); "
            f"time.sleep({0 if parent_exits else 60})"
        )
        with tempfile.TemporaryDirectory() as scratch:
            try:
                with tempfile.TemporaryFile() as log:
                    if parent_exits:
                        browser.run([sys.executable, "-c", parent], cwd=scratch,
                                    env=os.environ, timeout=5, log=log)
                    else:
                        with self.assertRaises(subprocess.TimeoutExpired):
                            browser.run([sys.executable, "-c", parent], cwd=scratch,
                                        env=os.environ, timeout=1, log=log)
                self.assert_descendant_stopped(int((Path(scratch) / "child").read_text()))
            finally:
                # Clean up even when run() regresses: only this fixture's own group.
                group_file = Path(scratch) / "group"
                if group_file.exists():
                    try:
                        os.killpg(int(group_file.read_text()), signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    def test_timeout_stops_sigint_ignoring_descendant(self):
        self.check_descendant_cleanup(parent_exits=False)

    def test_exited_leader_stops_sigint_ignoring_descendant(self):
        self.check_descendant_cleanup(parent_exits=True)

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
