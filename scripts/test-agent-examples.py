#!/usr/bin/env python3
"""Fast negative-path regressions; native execution is in examples-smoke.sh."""
import importlib.util
import json
import os
import shlex
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("agent_loop", ROOT / "examples/agents/agent_loop.py")
agent_loop = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(agent_loop)
PROOF_SPEC = importlib.util.spec_from_file_location("verify_pi_fleet", ROOT / "examples/agents/verify_pi_fleet.py")
proof = importlib.util.module_from_spec(PROOF_SPEC)
PROOF_SPEC.loader.exec_module(proof)


class AgentExamplesTests(unittest.TestCase):
    def test_explicit_missing_binary_never_falls_back_or_builds(self):
        with patch.dict(os.environ, PHUX="/missing/phux"), self.assertRaises(FileNotFoundError):
            agent_loop.find_phux()

    def test_cleanup_reaps_server_before_returning(self):
        server = Mock()
        agent_loop.stop_server(server)
        server.terminate.assert_called_once()
        server.wait.assert_called_once_with(timeout=5)
        server.kill.assert_not_called()
        server.reset_mock()
        server.wait.side_effect = [subprocess.TimeoutExpired("phux", 5), 0]
        agent_loop.stop_server(server)
        server.kill.assert_called_once()
        self.assertEqual(server.wait.call_count, 2)

    def test_proof_requires_real_child_inventory(self):
        claude = {"session": "proof", "cwd": "/work", "terminal": "@2", "agent": {"kind": "claude"}}
        codex = {"session": "proof", "cwd": "/work", "terminal": "@3", "agent": {"kind": "codex"}}
        seed = {"session": "proof", "cwd": "/work", "terminal": "@1", "agent": {"kind": None}}
        self.assertEqual(proof.child_panes({"agents": [seed, claude, codex]}, "proof", "/work"),
                         {"claude": "@2", "codex": "@3"})
        for children in ([], [claude], [claude, claude],
                         [claude, dict(codex, terminal="@2")],
                         [claude, dict(codex, cwd="/elsewhere")],
                         [claude, dict(codex, session="elsewhere")]):
            with self.subTest(children=children), self.assertRaises(ValueError):
                proof.child_panes({"agents": children}, "proof", "/work")

    def test_proof_response_must_be_child_output(self):
        for lines in (["TOKEN"], ["  ● TOKEN  "]):
            proof.verify_response({"pane": 2, "lines": lines}, "@2", "TOKEN")
        for snapshot in ({"pane": 3, "lines": ["TOKEN"]},
                         {"pane": 2, "lines": ["Reply with TOKEN"]},
                         {"pane": 2, "lines": ["tool_args: TOKEN"]},
                         {"pane": 2, "lines": []}):
            with self.subTest(snapshot=snapshot), self.assertRaises(ValueError):
                proof.verify_response(snapshot, "@2", "TOKEN")

    @unittest.skipUnless(os.environ.get("PHUX_EXAMPLES_NATIVE") == "1", "native smoke opt-in")
    def test_delayed_wait_start_cannot_finish_the_producer(self):
        with tempfile.TemporaryDirectory(prefix="delayed-example.") as directory:
            binary = str(Path(os.environ["PHUX"]).resolve())
            marker = Path(directory) / "delayed"
            wrapper = Path(directory) / "phux"
            wrapper.write_text(
                "#!/bin/sh\n"
                f"if [ \"$1\" = wait ] && [ ! -f {shlex.quote(str(marker))} ]; then\n"
                f"  touch {shlex.quote(str(marker))}; sleep 4\nfi\n"
                f"exec {shlex.quote(binary)} \"$@\"\n"
            )
            wrapper.chmod(0o755)
            result = subprocess.run(
                ["bash", str(ROOT / "examples/agents/03-send-keys-and-wait.sh")],
                env=dict(os.environ, PHUX=str(wrapper)), capture_output=True, text=True, timeout=30,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue(marker.exists())

    @unittest.skipUnless(os.environ.get("PHUX_EXAMPLES_NATIVE") == "1", "native smoke opt-in")
    def test_fake_pi_success_narration_cannot_pass(self):
        with tempfile.TemporaryDirectory(prefix="fake-pi-proof.") as directory:
            pi = Path(directory) / "pi"
            event = {"type": "message_end", "message": {"role": "assistant", "content": [
                {"type": "text", "text": "CLAUDE_LIVE_FLEET_OK CODEX_LIVE_FLEET_OK LIVE_PI_PHUX_FLEET_OK"}]}}
            pi.write_text("#!/bin/sh\nprintf '%s\\n' '" + json.dumps(event) + "'\n")
            pi.chmod(0o755)
            env = dict(os.environ, PHUX_DOGFOOD_REAL_AGENTS="1", HOME=directory,
                       PATH=directory + os.pathsep + os.environ["PATH"])
            result = subprocess.run(["bash", str(ROOT / "examples/agents/record-pi-live-fleet")],
                                    env=env, capture_output=True, text=True, timeout=20)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("LIVE_PI_PHUX_FLEET_OK", result.stdout, result.stderr)
            self.assertIn("exactly two child agents", result.stderr)

    def test_live_proof_requires_explicit_quota_opt_in(self):
        env = dict(os.environ, PHUX_DOGFOOD_REAL_AGENTS="0")
        result = subprocess.run(["bash", str(ROOT / "examples/agents/record-pi-live-fleet")],
                                env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("opt in", result.stderr)

    def test_wait_only_timeout_is_false(self):
        phux = agent_loop.Phux("phux", "private.sock", "demo", {})
        for method in (phux.wait_until, phux.wait_idle):
            args = ("output",) if method == phux.wait_until else ()
            for status in (0, 124, 1, 2):
                with self.subTest(method=method.__name__, status=status):
                    phux._run = Mock(return_value=subprocess.CompletedProcess([], status))
                    if status in (0, 124):
                        self.assertEqual(method(*args), status == 0)
                    else:
                        with self.assertRaises(subprocess.CalledProcessError):
                            method(*args)

    def test_run_preserves_failure_and_command_exit(self):
        phux = agent_loop.Phux("phux", "private.sock", "demo", {})
        phux._run = Mock(return_value=subprocess.CompletedProcess([], 2, stdout=""))
        with self.assertRaises(subprocess.CalledProcessError):
            phux.run("true")
        phux._run.return_value = subprocess.CompletedProcess([], 7, stdout='{"exit_code":7}')
        self.assertEqual(phux.run("false")["exit_code"], 7)

    def test_environment_is_private(self):
        inherited = {"PHUX_PROFILE": "default", "PHUX_WS_TOKENS": "/production/tokens",
                     "PHUX_WS_FUTURE_SETTING": "secret", "PHUX_SOCKET": "/production/socket"}
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, inherited):
            env = agent_loop.private_env(directory)
            self.assertEqual({k for k in env if k.startswith("PHUX_")}, {"PHUX_NO_UPDATE_CHECK"})
            self.assertEqual(env["SHELL"], "/bin/sh")
            self.assertTrue(Path(env["XDG_STATE_HOME"]).is_relative_to(directory))

    def test_prompt_wait_cannot_match_source_and_idle_must_succeed(self):
        phux = Mock()
        phux.ls.return_value = []
        phux.run.return_value = {"exit_code": 0, "output": "hello"}
        phux.snapshot.return_value = {"lines": ["Pick a number:"]}
        phux.wait_until.return_value = True
        phux.wait_idle.return_value = False
        with self.assertRaises(TimeoutError):
            agent_loop.run_example(phux)
        source = phux.send_keys.call_args_list[0].args[0]
        self.assertNotIn("Pick a number:", source)
        self.assertNotIn("result=49", source)

    def test_shell_timeout_and_environment(self):
        for status, expected in ((0, 1), (124, 0), (1, 1), (2, 2)):
            with self.subTest(status=status):
                script = '''
source "$1/examples/agents/lib.sh"
[[ -z "${PHUX_PROFILE:-}${PHUX_WS_TOKENS:-}${PHUX_WS_FUTURE_SETTING:-}" ]]
[[ "$SHELL" == /bin/sh && "$XDG_STATE_HOME" == "$PHUX_TMPDIR/"* ]]
phux() { return "$test_status"; }
phux_expect_timeout --until missing
'''
                env = dict(os.environ, PHUX="/usr/bin/true", PHUX_PROFILE="default",
                           PHUX_WS_TOKENS="/production/tokens", PHUX_WS_FUTURE_SETTING="secret",
                           test_status=str(status))
                result = subprocess.run(["bash", "-c", script, "test", str(ROOT)],
                                        env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, expected, result.stderr)


if __name__ == "__main__":
    unittest.main()
