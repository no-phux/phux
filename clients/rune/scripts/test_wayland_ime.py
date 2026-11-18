"""Exercise the real IME driver with fake platform commands, without a display."""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest

DRIVER = Path(__file__).with_name("qualify-wayland-ime.sh")
FAKE = r'''
import json, os
from pathlib import Path
import sys

root = Path(os.environ["STATE"])
name = Path(sys.argv[0]).name
args = sys.argv[1:]
mode = os.environ["MODE"]
with (root / "calls").open("a") as out:
    out.write(json.dumps([name, *args]) + "\n")
if name in ("fcitx5", "rune-probe"):
    (root / (name + ".pid")).write_text(str(os.getpid()))
    if name == "fcitx5" and mode == "dead-ime":
        sys.exit(0)
    if name == "rune-probe":
        if int((root / "bus-count").read_text()) < 3:
            sys.exit("app launched before the input method owned the bus")
        if mode != "no-cursor":
            print("zwp_text_input_v3@20.set_cursor_rectangle(10, 20, 8, 17)", flush=True)
        if mode == "dead-app":
            sys.exit(0)
    os.execv("/bin/sleep", ["sleep", "60"])
elif name == "dbus-send":
    if "string:org.fcitx.Fcitx5" in args:
        count = root / "bus-count"
        n = int(count.read_text()) + 1 if count.exists() else 1
        count.write_text(str(n))
        print("boolean " + ("false" if n < 3 or mode == "no-bus" else "true"))
    else:
        focus = 0 if mode == "no-context" else 1
        print(f"IC program:sh.phux.rune frontend:wayland_v2 cap:52 focus:{focus}")
elif name == "swaymsg":
    pid = root / "rune-probe.pid"
    focused = mode not in ("no-focus", "dead-app")
    print(json.dumps({"pid": int(pid.read_text()) if pid.exists() and mode != "wrong-window" else 0,
                      "app_id": "sh.phux.rune", "focused": focused}))
elif name == "fcitx5-remote":
    if args == ["-s", "pinyin"] and mode == "switch-failed":
        sys.exit(1)
    if args == ["-n"]:
        print("keyboard-us" if mode == "wrong-engine" else "pinyin")
elif name == "wtype":
    if mode == "typing-failed":
        sys.exit(1)
    with (root / "wayland-ime-app.log").open("a") as out:
        if mode != "no-preedit":
            out.write('zwp_text_input_v3@20.preedit_string("ni hao", 0, 6)\n')
        if mode != "no-commit":
            out.write('zwp_text_input_v3@20.commit_string("你好")\n')
elif name == "phux-probe":
    print("│$ " + ("" if mode == "no-echo" else "你好"))
elif name == "grim":
    Path(args[-1]).write_bytes(b"unit-test screenshot placeholder")
elif name == "sleep" and mode == "moving-cursor":
    with (root / "wayland-ime-app.log").open("a") as out:
        out.write("zwp_text_input_v3@20.set_cursor_rectangle(10, 20, 8, 17)\n")
'''


class WaylandIme(unittest.TestCase):
    def run_driver(self, mode="success"):
        with tempfile.TemporaryDirectory(prefix="wayland-ime-") as directory:
            root = Path(directory)
            binary = root / "bin"
            binary.mkdir()
            fake = binary / "fake"
            fake.write_text(f"#!{sys.executable}\n" + FAKE)
            fake.chmod(0o755)
            for name in ("fcitx5", "fcitx5-remote", "dbus-send", "swaymsg",
                         "wtype", "phux-probe", "rune-probe", "grim", "sleep"):
                (binary / name).symlink_to(fake)
            jq = shutil.which("jq")
            self.assertIsNotNone(jq, "qualification tests need jq")
            (binary / "jq").symlink_to(jq)
            env = {"PATH": f"{binary}:/usr/bin:/bin", "HOME": directory,
                   "STATE": directory, "MODE": mode, "LANG": "C.UTF-8"}
            try:
                result = subprocess.run(
                    [shutil.which("bash"), str(DRIVER), directory,
                     str(binary / "rune-probe"), str(binary / "phux-probe"),
                     "owned-socket", "owned-session"],
                    env=env, text=True, capture_output=True, timeout=30,
                )
                calls = [json.loads(line) for line in (root / "calls").read_text().splitlines()]
                for pid_file in root.glob("*.pid"):
                    with self.assertRaises(ProcessLookupError, msg="driver must reap its own processes"):
                        os.kill(int(pid_file.read_text()), 0)
                return result, calls
            finally:
                # Only these fake children belong to this test, including on timeout.
                for pid_file in root.glob("*.pid"):
                    try:
                        os.kill(int(pid_file.read_text()), signal.SIGTERM)
                    except ProcessLookupError:
                        pass

    def test_ascii_keys_require_real_preedit_commit_and_terminal_echo(self):
        result, calls = self.run_driver()
        self.assertEqual(result.returncode, 0, result.stderr)
        typing = [call for call in calls if call[0] == "wtype"]
        self.assertEqual(typing, [["wtype", "-k", "Control_L", "-s", "500",
                                  "nihao", "-s", "800", "1", "-s", "500", "-k", "Return"]])
        self.assertIn(["fcitx5-remote", "-s", "pinyin"], calls)
        self.assertIn(["fcitx5-remote", "-o"], calls)
        self.assertLess(calls.index(["fcitx5-remote", "-n"]), calls.index(typing[0]))
        self.assertIn(["phux-probe", "--socket", "owned-socket", "snapshot", "owned-session"], calls)
        self.assertEqual(sum(call[0] == "dbus-send" and "string:org.fcitx.Fcitx5" in call
                             for call in calls), 3)
        self.assertIn("the server echoed the composed text", result.stdout)

    def test_missing_focus_or_context_never_injects_keys(self):
        for mode in ("no-focus", "wrong-window", "no-context", "no-cursor",
                     "moving-cursor", "wrong-engine", "switch-failed",
                     "dead-app", "dead-ime", "no-bus"):
            with self.subTest(mode=mode):
                result, calls = self.run_driver(mode)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any(call[0] == "wtype" for call in calls))

    def test_failed_keyboard_keeps_diagnostics_and_does_not_replay_keys(self):
        result, calls = self.run_driver("typing-failed")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("the virtual keyboard failed during composition", result.stderr)
        self.assertEqual(sum(call[0] == "wtype" for call in calls), 1)
        self.assertTrue(any(call[0] == "grim" for call in calls))

    def test_commit_alone_does_not_fake_pinyin_and_missing_echo_is_not_a_pass(self):
        for mode, message in (("no-preedit", "Pinyin preedit reached Rune"),
                              ("no-commit", "Pinyin committed 你好 to Rune"),
                              ("no-echo", "the server echoed the composed text")):
            with self.subTest(mode=mode):
                result, calls = self.run_driver(mode)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("timed out waiting for " + message, result.stderr)
                self.assertEqual(sum(call[0] == "wtype" for call in calls), 1,
                                 "failure must not replay the composition")


if __name__ == "__main__":
    unittest.main()
