"""Regression checks for fail-closed browser execution and bounded cleanup."""

import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch
from urllib.error import HTTPError
from urllib.request import Request, urlopen


spec = importlib.util.spec_from_file_location("web_browser", Path(__file__).with_name("web-browser.py"))
browser = importlib.util.module_from_spec(spec)
spec.loader.exec_module(browser)


class BrowserRunnerTests(unittest.TestCase):
    def test_agent_fixture_is_canned_token_gated_and_cleans_up(self):
        fixture = browser.AgentFixture("/test/phux", "/tmp/owned-test.sock", {})
        with patch.object(fixture, "command") as command:
            with fixture:
                with self.assertRaises(HTTPError) as error:
                    urlopen(Request(fixture.url.replace(fixture.token, "wrong") + "/open", method="POST"))
                self.assertEqual(error.exception.code, 404)
                error.exception.close()
                command.assert_not_called()
                with urlopen(Request(fixture.url + "/open", method="POST")) as response:
                    self.assertEqual(response.status, 200)
                self.assertTrue(fixture.opened)
                with self.assertRaises(HTTPError) as error:
                    urlopen(Request(fixture.url + "/arbitrary-command", method="POST"))
                error.exception.close()
            command.assert_any_call("agent", "session", "open", "default", "--provider", "pi")
            command.assert_called_with("agent", "session", "close", "default")
        self.assertFalse(fixture.thread.is_alive())

    def test_agent_fixture_cli_always_targets_its_owned_socket(self):
        fixture = browser.AgentFixture("/test/phux", "/tmp/owned-test.sock", {"PATH": "/bin"})
        try:
            with patch.object(subprocess, "run", return_value=Mock(returncode=0)) as run:
                fixture.command("agent", "session", "close", "default")
            self.assertEqual(run.call_args.args[0], ["/test/phux", "agent", "session", "close", "default",
                                                    "--socket", "/tmp/owned-test.sock"])
            self.assertEqual(run.call_args.kwargs["timeout"], 10)
            self.assertEqual(run.call_args.kwargs["env"], {"PATH": "/bin"})
        finally:
            fixture.server.server_close()

    def test_udp_blackhole_receives_and_cleans_up_owned_socket(self):
        with browser.UdpBlackhole() as blackhole:
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
                sender.sendto(b"quic", ("127.0.0.1", blackhole.port))
            for _ in range(100):
                if blackhole.packets:
                    break
                time.sleep(0.01)
            self.assertGreater(blackhole.packets, 0)
        self.assertFalse(blackhole.thread.is_alive())

    def test_chrome_binary_is_in_runner_capabilities(self):
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            env = browser.webdriver_environment({"CHROME": "/selected/chromium"}, directory, directory)
            capabilities = json.loads(Path(env["WASM_BINDGEN_TEST_WEBDRIVER_JSON"]).read_text())
            self.assertEqual(capabilities["goog:chromeOptions"]["binary"], "/selected/chromium")

    def test_fixture_only_accepts_insecure_certificates_in_temporary_capabilities(self):
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            env = browser.webdriver_environment(
                {"PHUX_TEST_ACCEPT_INSECURE_CERTS": "1"}, directory, directory)
            capabilities = json.loads(Path(env["WASM_BINDGEN_TEST_WEBDRIVER_JSON"]).read_text())
            self.assertTrue(capabilities["acceptInsecureCerts"])

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
        # The descendant ignores SIGINT on purpose, so stop() always spends its
        # whole grace before escalating; the escalation is what is under test,
        # not the production grace's length.
        with tempfile.TemporaryDirectory() as scratch, patch.object(browser, "STOP_GRACE_SECONDS", 0.5):
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

    def test_bound_port_is_read_from_the_listening_line(self):
        server = Mock()
        server.poll.return_value = None
        with tempfile.TemporaryDirectory() as scratch:
            log = Path(scratch) / "server.log"
            log.write_text("ws-demo-server listening on wss://127.0.0.1:43127/  (seed: default)\n")
            self.assertEqual(browser.bound_port(server, log), 43127)

    def test_bound_port_waits_for_the_line_and_is_bounded(self):
        server = Mock()
        server.poll.return_value = None
        with tempfile.TemporaryDirectory() as scratch:
            log = Path(scratch) / "server.log"
            log.write_text("")
            with patch.object(browser.time, "sleep") as sleep:
                with self.assertRaises(TimeoutError):
                    browser.bound_port(server, log)
        self.assertEqual(sleep.call_count, 300)

    def test_early_server_exit_is_not_a_bound_port(self):
        server = Mock()
        server.poll.return_value = 1
        with tempfile.TemporaryDirectory() as scratch:
            log = Path(scratch) / "server.log"
            log.write_text("")
            with self.assertRaisesRegex(RuntimeError, "exited before"):
                browser.bound_port(server, log)

    def test_command_failure_is_not_masked(self):
        with tempfile.TemporaryFile() as log:
            with self.assertRaises(subprocess.CalledProcessError):
                browser.run([sys.executable, "-c", "raise SystemExit(7)"],
                            cwd=browser.ROOT, env=os.environ, timeout=5, log=log)

    def test_inherited_phux_configuration_never_reaches_the_demo_server(self):
        env = browser.isolated_environment({
            "PATH": "/bin",
            "PHUX_SOCKET": "/tmp/phux-user/phux.sock",
            "PHUX_QUIC_ADDR": "0.0.0.0:8788",
            "PHUX_WS_TOKENS": "/home/user/.local/state/phux/remote-tokens",
            "PHUX_WS_TLS_CERT": "/home/user/.local/state/phux/remote-cert.pem",
            "PHUX_WEB_CARGO_TARGET_DIR": "/scratch/web",
            "PHUX_BROWSER_AUTH_ONLY": "1",
        })
        self.assertEqual(env, {
            "PATH": "/bin",
            "PHUX_WEB_CARGO_TARGET_DIR": "/scratch/web",
            "PHUX_BROWSER_AUTH_ONLY": "1",
        })

    def test_unsignalable_zombie_group_is_already_stopped(self):
        # A successful run must not turn into a failure when Darwin refuses
        # to signal a group that holds only zombies.
        process = Mock(pid=12345)
        with patch.object(browser.os, "killpg", side_effect=PermissionError):
            browser.stop(process)
        process.wait.assert_called_once_with(timeout=5)

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
