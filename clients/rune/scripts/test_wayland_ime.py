"""Executable fake-platform tests; these are not Linux/app qualification."""

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
from unittest.mock import patch

DRIVER = Path(__file__).with_name("qualify_wayland_ime.py")
SPEC = importlib.util.spec_from_file_location("wayland_ime", DRIVER)
ime = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ime)

# Every executable, including the server CLI and app, is a local test fake.
# There is no route to an installed phux, production socket, or real display.
FAKE = r'''
import fcntl, json, os, signal, sys, time
from pathlib import Path
root = Path(os.environ["STATE"])
name = Path(sys.argv[0]).name
args = sys.argv[1:]
mode = os.environ["MODE"]
with (root / "calls").open("a") as out:
    out.write(json.dumps([name, *args]) + "\n")
if name in ("fcitx5", "rune-fake"):
    pidfile = root / (name + ".pid")
    pending = root / (name + ".pid.tmp")
    pending.write_text(str(os.getpid()))
    pending.replace(pidfile)
    assert os.environ["WAYLAND_DEBUG"] == "1"
    if mode == "dead-" + name:
        sys.exit(7)
    if name == "fcitx5":
        assert not args, "fcitx must remain foreground"
    else:
        assert args == ["--socket", "fake.socket", "--session", "fresh-ime"]
        # The app and the fake keyboard both emit fixture protocol records.
        fcntl.fcntl(1, fcntl.F_SETFL, fcntl.fcntl(1, fcntl.F_GETFL) | os.O_APPEND)
        print("[1.0] zwp_text_input_v3@12.enter(wl_surface@4)", flush=True)
        print("[1.1] -> zwp_text_input_v3@12.enable()", flush=True)
    # Continuous cursor traffic must never become a quiet-frame readiness gate.
    while True:
        if name == "rune-fake":
            print("[2.0] -> zwp_text_input_v3@12.set_cursor_rectangle(1, 2, 3, 4)", flush=True)
            print("[2.1] -> zwp_text_input_v3@12.commit()", flush=True)
        time.sleep(0.02)
elif name == "dbus-send":
    method = args[-2] if args[-1].startswith("string:") else args[-1]
    if method.endswith("NameHasOwner"):
        ready = (root / "fcitx5.pid").exists() and mode != "no-bus"
        print("boolean " + ("true" if ready else "false"))
    elif method.endswith("GetNameOwner"):
        print('string ":1.0"')
    elif method.endswith("GetConnectionUnixProcessID"):
        pid = int((root / "fcitx5.pid").read_text())
        print("uint32", pid + (1 if mode == "wrong-bus-pid" else 0))
    else:
        focus = 0 if mode == "no-context" else 1
        frontend = "x11" if mode == "wrong-frontend" else "wayland_v2"
        if mode == "split-context":
            print("program:sh.phux.rune frontend:wayland_v2 cap:52 focus:0")
            print("program:other frontend:wayland_v2 cap:52 focus:1")
        else:
            print(f'  string "IC program:sh.phux.rune frontend:{frontend} cap:52 focus:{focus}"')
elif name == "swaymsg":
    if mode == "tree-command-fails":
        sys.exit(6)
    if mode == "hung-tree":
        time.sleep(20)
    pidfile = root / "rune-fake.pid"
    pid = int(pidfile.read_text()) if pidfile.exists() else 0
    stale = {"pid": pid + 1, "app_id": "sh.phux.rune", "focused": True}
    node = {"pid": pid, "app_id": "sh.phux.rune", "focused": mode != "wrong-focus"}
    if mode == "wrong-focus":
        print(json.dumps({"nodes": [node, stale]}))
    elif mode == "wrong-app-id":
        print(json.dumps({**node, "app_id": "other.app"}))
    else:
        print(json.dumps({"floating_nodes": [node]}))
elif name == "fcitx5-remote":
    if mode == "activation-fails" and args == ["-o"]:
        sys.exit(4)
    if args == ["-n"]:
        print("keyboard-us" if mode == "profile-only" else "pinyin")
    elif not args:
        print("1" if mode == "inactive" else "2")
elif name == "wtype":
    (root / "injected").write_text("once")
    if mode == "typing-fails":
        sys.exit(5)
    if mode == "app-dies-during-composition":
        os.kill(int((root / "rune-fake.pid").read_text()), signal.SIGTERM)
        time.sleep(0.05)
    with (root / "wayland-ime-app.log").open("a") as out:
        # Segmentation is intentionally unlike the historical "ni hao".
        out.write('[3.0] zwp_text_input_v3@12.preedit_string("ni’hao", 0, 6)\n')
        if mode != "no-commit":
            out.write('[3.1] zwp_text_input_v3@12.commit_string("你好")\n')
        out.write('[3.2] zwp_text_input_v3@12.done(47)\n')
elif name == "phux-fake":
    assert args == ["--socket", "fake.socket", "snapshot", "fresh-ime"]
    injected = (root / "injected").exists()
    echo = "你好" if injected or mode == "stale-session" else ""
    if injected:
        echo = {"plain-ascii": "nihao", "wrong-chinese": "你号",
                "no-echo": "", "empty-snapshot": ""}.get(mode, echo)
    if mode != "empty-snapshot" or not injected:
        print("│$ " + echo)
        print("│full snapshot row beyond the old four-row truncation\n" * 12, end="")
elif name == "grim":
    Path(args[-1]).write_bytes(b"fake fixture, not an app screenshot")
'''

class WaylandImeTests(unittest.TestCase):
    def run_driver(self, mode="success"):
        with tempfile.TemporaryDirectory(prefix="wayland-ime-", dir=os.environ.get("TMPDIR")) as directory:
            root = Path(directory)
            fake = root / "fake"
            fake.write_text(f"#!{sys.executable}\n" + FAKE)
            fake.chmod(0o755)
            names = ("fcitx5", "rune-fake", "dbus-send", "swaymsg",
                     "fcitx5-remote", "wtype", "phux-fake", "grim")
            for name in names:
                (root / name).symlink_to(fake)
            env = {"PATH": f"{root}:/usr/bin:/bin", "HOME": directory,
                   "STATE": directory, "MODE": mode, "LANG": "C.UTF-8"}
            started = time.monotonic()
            try:
                result = subprocess.run(
                    [sys.executable, "-B", str(DRIVER), directory,
                     str(root / "rune-fake"), str(root / "phux-fake"),
                     "fake.socket", "fresh-ime"],
                    env=env, text=True, capture_output=True, timeout=20,
                )
                elapsed = time.monotonic() - started
                calls = [json.loads(line) for line in (root / "calls").read_text().splitlines()]
                stages = [json.loads(line) for line in
                          (root / "wayland-ime-stages.log").read_text().splitlines()]
                files = {p.name: p.read_text(errors="replace") for p in root.glob("wayland-*")}
                for event in stages:
                    if event["stage"] == "process-start":
                        with self.assertRaises(ProcessLookupError, msg="owned children must be reaped"):
                            os.kill(event["pid"], 0)
                return result, calls, stages, files, elapsed
            finally:
                # Only fixture-owned PIDs, and only if a failed test left them.
                for pidfile in root.glob("*.pid"):
                    try:
                        os.kill(int(pidfile.read_text()), signal.SIGTERM)
                    except ProcessLookupError:
                        pass

    def test_single_ascii_composition_with_real_echo_and_unrestricted_preedit(self):
        result, calls, stages, files, _ = self.run_driver()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([c for c in calls if c[0] == "wtype"], [ime.KEYS])
        self.assertIn(["fcitx5-remote", "-o"], calls)
        self.assertIn("set_cursor_rectangle", files["wayland-ime-app.log"])
        self.assertIn('done(47)', files["wayland-ime-app.log"])
        self.assertIn("ni’hao", files["wayland-ime-app.log"])
        self.assertIn("full snapshot row", files["wayland-ime-terminal.log"])
        self.assertEqual(sum(s["stage"] == "process-reaped" for s in stages), 2)
        self.assertEqual(sum(s["stage"] == "injection-start" for s in stages), 1)
        self.assertIn("wayland-ime-bus-owner.log", files)
        self.assertIn("wayland-ime-tree-before.json", files)

    def test_wrong_owners_profile_only_and_failed_commands_never_inject(self):
        cases = {
            "wrong-focus": "keyboard focus", "wrong-app-id": "keyboard focus",
            "no-context": "focused Rune Wayland context", "split-context": "focused Rune",
            "wrong-frontend": "focused Rune", "profile-only": "effective Pinyin",
            "inactive": "effective Pinyin", "no-bus": "owns org.fcitx.Fcitx5",
            "wrong-bus-pid": "owner PID", "stale-session": "stale session",
            "activation-fails": "command failed (4)",
            "tree-command-fails": "command failed (6)",
            "dead-rune-fake": "app exited (7)", "dead-fcitx5": "fcitx exited (7)",
            "hung-tree": "restarted Rune PID has keyboard focus",
        }
        for mode, message in cases.items():
            with self.subTest(mode=mode):
                result, calls, stages, files, elapsed = self.run_driver(mode)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertFalse(any(c[0] == "wtype" for c in calls))
                self.assert_failure_evidence(stages, files)
                self.assertLess(elapsed, 13, "one shared startup budget plus bounded failure capture")

    def assert_failure_evidence(self, stages, files):
        self.assertTrue(any(s["stage"] == "failure-liveness" for s in stages))
        for artifact in ("tree-failure.json", "context-failure.log",
                         "engine-failure.log", "state-failure.log", "terminal-failure.log"):
            self.assertIn("wayland-ime-" + artifact, files)
        self.assertIn("full snapshot row", files["wayland-ime-terminal-failure.log"])
        self.assertIn("wayland-ime-failed.png", files)
        self.assertTrue(all("time" in s and "elapsed" in s for s in stages))

    def test_no_replay_or_false_success_after_composition(self):
        for mode in ("typing-fails", "app-dies-during-composition", "no-commit",
                     "plain-ascii", "wrong-chinese", "no-echo", "empty-snapshot"):
            with self.subTest(mode=mode):
                result, calls, stages, files, _ = self.run_driver(mode)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertEqual([c for c in calls if c[0] == "wtype"], [ime.KEYS], result.stderr)
                self.assertTrue(any(s["stage"] == "failure-liveness" for s in stages))
                self.assertIn("wayland-ime-terminal-failure.log", files)
                if mode == "no-commit":
                    self.assertIn("no commit_string", result.stderr)

    def test_context_tokens_cannot_be_joined_across_input_contexts(self):
        self.assertFalse(ime.focused_context(
            "program:sh.phux.rune frontend:wayland_v2 focus:0\n"
            "program:other frontend:wayland_v2 focus:1"))
        self.assertFalse(ime.focused_context("program:sh.phux.rune frontend:wayland_v2 focus:10"))
        self.assertTrue(ime.focused_context(
            'string "IC program:sh.phux.rune frontend:wayland_v2 cap:52 focus:1"'))

    def test_startup_deadline_is_shared_and_exactly_nine_seconds(self):
        self.assertEqual(ime.STARTUP_SECONDS, 9)
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as directory:
            with patch.object(ime.time, "monotonic", return_value=100):
                driver = ime.Diagnostic(directory, "fake-app", "fake-phux", "socket", "session")
            self.assertEqual(driver.deadline, 109)
            with patch.object(ime.time, "monotonic", return_value=108):
                driver.wait_for("first", lambda: True)
            self.assertEqual(driver.deadline, 109)
            with patch.object(ime.time, "monotonic", return_value=109):
                with self.assertRaisesRegex(RuntimeError, "timed out waiting for second"):
                    driver.wait_for("second", lambda: True)
                with self.assertRaisesRegex(RuntimeError, "before injection"):
                    driver.compose()
            self.assertEqual(driver.deadline, 109)

    def test_commands_are_clipped_to_remaining_deadline(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as directory:
            with patch.object(ime.time, "monotonic", return_value=100):
                driver = ime.Diagnostic(directory, "fake-app", "fake-phux", "socket", "session")
            with patch.object(ime.time, "monotonic", return_value=108.75):
                with patch.object(ime.subprocess, "run") as run:
                    run.return_value.returncode = 0
                    driver.command(["fixture-only"], "bounded.log", timeout=20)
                    self.assertEqual(run.call_args.kwargs["timeout"], 0.25)
            with patch.object(ime.time, "monotonic", return_value=109):
                with patch.object(ime.subprocess, "run") as run:
                    with self.assertRaisesRegex(RuntimeError, "deadline expired"):
                        driver.command(["fixture-only"], "never-ran.log")
                    run.assert_not_called()

    def test_stage_and_command_logs_survive_existing_evidence_export(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as directory:
            driver = ime.Diagnostic(directory, "fake-app", "fake-phux", "socket", "session")
            with patch.object(ime.subprocess, "run") as run:
                run.return_value.returncode = 0
                driver.command(["fixture-only"], "wayland-ime-tree-before.json")
            files = {p.name for p in Path(directory).iterdir()}
            self.assertEqual(files, {"wayland-ime-tree-before.json",
                                     "wayland-ime-tree-before.json.stderr.log",
                                     "wayland-ime-stages.log"})
            # Exercise qualify.sh's actual cp globs without changing that caller.
            export = (DRIVER.parent / "qualify.sh").read_text()
            for suffix in ("png", "json", "log"):
                self.assertIn(f'"$scratch"/*.{suffix}', export)

    def test_composition_has_one_shared_two_second_echo_budget(self):
        self.assertEqual(ime.ECHO_SECONDS, 2)
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as directory:
            driver = ime.Diagnostic(directory, "fake-app", "fake-phux", "socket", "session")
            driver.deadline = 209
            (Path(directory) / "wayland-ime-app.log").write_text(
                'zwp_text_input_v3@12.commit_string("你好")\n')
            with patch.object(ime.time, "monotonic", return_value=200):
                with patch.object(driver, "command") as command:
                    with patch.object(driver, "wait_for") as wait:
                        driver.compose()
            command.assert_called_once_with(ime.KEYS, "wayland-ime-wtype.log", timeout=5)
            wait.assert_called_once()
            self.assertEqual(driver.deadline, 202)


if __name__ == "__main__":
    unittest.main()
