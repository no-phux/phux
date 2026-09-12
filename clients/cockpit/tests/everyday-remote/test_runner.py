#!/usr/bin/env python3
"""Regression tests use disposable mock processes; no app or Phux server starts."""
from contextlib import nullcontext
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
from unittest import mock


sys.dont_write_bytecode = True
RUNNER = Path(os.environ.get("EVERYDAY_REMOTE_RUNNER", Path(__file__).parents[2] / "scripts/everyday-remote-live.py")).resolve()
HOST = "aarch64-apple-darwin"


def load_runner():
    spec = importlib.util.spec_from_file_location("remote_runner", RUNNER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def process_state(pid):
    result = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True)
    return result.stdout.strip()


def wait_until(predicate, message):
    deadline = time.monotonic() + 8
    while not predicate():
        if time.monotonic() >= deadline:
            raise AssertionError(message)
        time.sleep(0.02)


def executable(path, body):
    path.write_text(f"#!{sys.executable}\n" + body)
    path.chmod(0o700)
    return path


def stop_orphan(pid, directory):
    # Only PIDs published by this test's own disposable executables.
    command = subprocess.run(["ps", "-o", "command=", "-p", str(pid)], capture_output=True, text=True)
    if str(directory) not in command.stdout:
        return
    try:
        os.kill(pid, signal.SIGCONT)
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


class RunnerRegression(unittest.TestCase):
    def test_main_rebuilds_provider_with_pinned_checkout_inputs(self):
        for profile in ("ffi-dev", "ffi-release"):
            with self.subTest(profile=profile):
                self.check_pinned_build(profile)

    def check_pinned_build(self, profile_name):
        runner = load_runner()
        with tempfile.TemporaryDirectory(dir="/private/tmp/opencode") as temporary:
            root = Path(temporary)
            legacy = root / "target" / profile_name
            legacy.mkdir(parents=True)
            (legacy / "phux").write_text("stale legacy CLI")
            (legacy / "libphux_client_ffi.a").write_text("stale legacy archive")
            profile = root / "target" / HOST / profile_name
            profile.mkdir(parents=True)
            (profile / "phux").write_text("fixture CLI")
            (profile / "libphux_client_ffi.a").write_text("fixture archive")
            header = root / "crates/phux-client-ffi/include/phux/client.h"
            header.parent.mkdir(parents=True)
            header.write_text("fixture header")
            provider = root / "clients/cockpit/zig-out/bin/everyday-remote-provider"
            provider.parent.mkdir(parents=True)
            provider.write_text("stale provider must be rebuilt")
            calls = []

            def build(argv, directory, env, **kwargs):
                calls.append((list(map(str, argv)), dict(env)))
                provider.write_text("fresh provider")
                return subprocess.CompletedProcess(argv, 0, stdout=f"host: {HOST}\n")

            def compile_probe(profile_name, destination):
                destination.write_text("C probe")

            overrides = {"PHUX_CLIENT_FFI_INCLUDE_DIR": "/wrong/include",
                         "PHUX_CLIENT_FFI_LIB_DIR": "/wrong/archive", "CARGO_TARGET_DIR": "/wrong/target",
                         "CARGO_BUILD_TARGET": "wasm32-wasip1"}
            with mock.patch.object(runner, "ROOT", root), \
                 mock.patch.object(runner, "run", side_effect=build), \
                 mock.patch.object(runner, "compile_probe", side_effect=compile_probe), \
                 mock.patch.object(runner, "exercise"), \
                 mock.patch.object(runner, "source_identity", return_value={"revision": "fixture"}, create=True), \
                 mock.patch.object(runner, "publish_evidence", create=True) as published, \
                 mock.patch.dict(os.environ, overrides), \
                 mock.patch.object(sys, "argv", ["runner", "--scratch-root", temporary,
                                                "--transport", "quic", "--profile", profile_name]):
                runner.main()
            builds = [(argv, env) for argv, env in calls if "everyday-remote-provider" in argv]
            self.assertEqual(len(builds), 1, "existing provider must not bypass the pinned rebuild")
            argv, env = builds[0]
            self.assertIn(f"-Dphux-client-ffi-include-dir={root}/crates/phux-client-ffi/include", argv)
            self.assertIn(f"-Dphux-client-ffi-lib-dir={profile}", argv)
            self.assertIn(f"-Dphux-client-ffi-profile={profile_name}", argv)
            for name in overrides:
                self.assertNotIn(name, env)
            self.assertEqual(provider.read_text(), "fresh provider")
            evidence = published.call_args.args[0]
            self.assertEqual(evidence["profile"], profile_name)
            self.assertEqual(evidence["native_target"], HOST)
            self.assertEqual(evidence["artifacts"]["provider_probe"]["sha256"], runner.file_digest(provider))
            self.assertEqual(evidence["artifacts"]["archive"]["sha256"], runner.file_digest(profile / "libphux_client_ffi.a"))
            self.assertEqual(evidence["artifacts"]["header"]["sha256"], runner.file_digest(header))

    def test_build_overrides_foreign_and_same_host_cargo_target_configuration(self):
        runner = load_runner()
        for configured in ("wasm32-wasip1", HOST):
            with self.subTest(configured=configured), tempfile.TemporaryDirectory(dir="/private/tmp/opencode") as temporary:
                root = Path(temporary)
                (root / ".cargo").mkdir()
                (root / ".cargo/config.toml").write_text(f'[build]\ntarget = "{configured}"\n')
                with mock.patch.object(runner, "ROOT", root), \
                     mock.patch.dict(os.environ, {"CARGO_BUILD_TARGET": configured}), \
                     mock.patch.object(runner, "run", return_value=subprocess.CompletedProcess([], 0, stdout=f"host: {HOST}\n")) as invoke:
                    runner.build_artifacts("ffi-dev", False)
                cargo = [call for call in invoke.call_args_list if call.args[0][0] == "cargo"]
                self.assertEqual(len(cargo), 2, "both archive and CLI need an explicit native-target Cargo build")
                for call in cargo:
                    argv, _, env = call.args
                    self.assertEqual(argv[argv.index("--target") + 1], HOST)
                    self.assertEqual(argv[argv.index("--target-dir") + 1], root / "target")
                    self.assertNotIn("CARGO_BUILD_TARGET", env)

    def test_missing_native_host_refuses_build(self):
        runner = load_runner()
        with mock.patch.object(runner, "run", return_value=subprocess.CompletedProcess([], 0, stdout="rustc without host\n")) as invoke:
            with self.assertRaisesRegex(RuntimeError, "native host target"):
                runner.build_artifacts("ffi-dev", False)
        self.assertEqual(invoke.call_count, 1)

    def test_wrong_single_secondary_execution_is_rejected(self):
        runner = load_runner()
        with tempfile.TemporaryDirectory(dir="/private/tmp/opencode") as temporary:
            directory = Path(temporary)
            (directory / "config/phux").mkdir(parents=True)
            (directory / "executed").write_text(
                "first-input\nafter-reconnect\nafter-link-stall\nafter-expired-history\n")
            (directory / "second-executed").write_text("WRONG-PAYLOAD\n")

            def result(argv, *args, **kwargs):
                data = {"token": "fixture", "cert_fingerprint": "cd" * 32,
                        "credential_id": "fixture", "auth_route": "bearer-quic", "sessions": []}
                return subprocess.CompletedProcess(argv, 0, stdout=json.dumps(data))

            with mock.patch.object(runner, "isolated_env", return_value={}), \
                 mock.patch.object(runner, "fixture_server", side_effect=lambda *args: nullcontext(mock.Mock())), \
                 mock.patch.object(runner, "run", side_effect=result), \
                 mock.patch.object(runner, "start_workload"), \
                 mock.patch.object(runner, "exercise_stall"), \
                 mock.patch.object(runner, "stop"):
                with self.assertRaisesRegex(AssertionError, "secondary"):
                    runner.exercise(Path("phux"), Path("probe"), None, directory, "quic")

    def test_termination_during_server_stop_resumes_and_reaps_owned_children(self):
        for signum in (signal.SIGTERM, signal.SIGHUP):
            with self.subTest(signal=signum), tempfile.TemporaryDirectory(dir="/private/tmp/opencode") as temporary:
                self.check_interruption(Path(temporary), signum)

    def check_interruption(self, directory, signum):
        common = "import os, pathlib, signal, sys, time\n"
        refusal = "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n" if signum == signal.SIGHUP else ""
        executable(directory / "phux", common +
                   "if 'server' not in sys.argv: sys.exit(0)\n"
                   + refusal +
                   "pathlib.Path('server.pid').write_text(str(os.getpid()))\n"
                   "pathlib.Path('s').touch()\nwhile True: time.sleep(1)\n")
        executable(directory / "probe", common +
                   "pathlib.Path('probe.pid').write_text(str(os.getpid()))\n"
                   "pathlib.Path('stall-ready').touch()\nwhile True: time.sleep(1)\n")
        wrapper = ("import importlib.util, os, sys\nsys.dont_write_bytecode=True\nfrom pathlib import Path\nfrom contextlib import nullcontext\n"
                   f"spec=importlib.util.spec_from_file_location('runner', {str(RUNNER)!r})\n"
                   "m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)\n"
                   "d=Path.cwd()\n"
                   "original_stop=m.stop\n"
                   "def tracked_stop(child):\n"
                   " original_stop(child)\n"
                   " with open('reaped', 'a') as record: record.write(f'{child.pid} {child.returncode}\\n')\n"
                   "m.stop=tracked_stop\n"
                   "with getattr(m, 'termination_scope', nullcontext)():\n"
                   " with m.fixture_server(d/'phux', d, dict(os.environ), 12345, 'quic') as server:\n"
                   "  m.exercise_stall(d/'probe', d/'unused-config', d, dict(os.environ), server)\n")
        pids = []
        with (directory / "runner.log").open("w") as log:
            child = subprocess.Popen([sys.executable, "-c", wrapper], cwd=directory, stdout=log, stderr=log)
            try:
                wait_until(lambda: (directory / "probe.pid").exists(), "probe never started")
                pids = [int((directory / name).read_text()) for name in ("server.pid", "probe.pid")]
                wait_until(lambda: "T" in process_state(pids[0]), "fixture server was never stopped")
                child.send_signal(signum)
                child.wait(timeout=15)
                wait_until(lambda: not any(process_state(pid) for pid in pids),
                           "signal left a stopped server or an unreaped probe")
                self.assertEqual(child.returncode, 128 + signum)
                reaped = dict(map(int, line.split()) for line in (directory / "reaped").read_text().splitlines())
                expected_server = -signal.SIGKILL if signum == signal.SIGHUP else -signal.SIGTERM
                self.assertEqual(reaped[pids[0]], expected_server)
                self.assertEqual(reaped[pids[1]], -signal.SIGTERM)
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait()
                for pid in pids:
                    stop_orphan(pid, directory)


if __name__ == "__main__":
    unittest.main()
