#!/usr/bin/env python3
"""Exercise the shell presentation contract without downloads or installations."""
import os
from pathlib import Path
import pty
import select
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
HELPER = ROOT / "scripts/lib/output.sh"
SHELLS = list(dict.fromkeys(filter(None, ["/bin/sh", "/bin/bash", shutil.which("dash")])))
STATUS = '. "$1"; phux_error "bad %s \\path"; phux_warn "careful"; phux_info "working"; phux_ok "done"; phux_hint "  try this\n  second line"; printf "data\\n"'
PLAIN = b"phux: error: bad %s \\path\nphux: warn: careful\nphux: info: working\nphux: ok: done\n  hint: try this\n  hint: second line\n"
COMMANDS = {shell: [shell, "-eu", "-c", STATUS, "test", str(HELPER)] for shell in SHELLS}
COMMANDS["bun"] = ["bun", "-e", r'''import { status, hint } from "../tools/src/lib/output.ts";
status("error", String.raw`bad %s \path`);
status("warn", "careful");
status("info", "working");
status("ok", "done");
hint("  try this\n  second line");
process.stdout.write("data\n");''']
COMMANDS["node"] = ["node", "--input-type=module", "-e", r'''import { status, hint } from "./lib/output.mjs";
status("error", String.raw`bad %s \path`);
status("warn", "careful");
status("info", "working");
status("ok", "done");
hint("  try this\n  second line");
process.stdout.write("data\n");''']


def environment(**overrides):
    env = os.environ.copy()
    for name in ("NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE"):
        env.pop(name, None)
    env["TERM"] = "xterm-256color"
    env.update(overrides)
    return env


def run_status(command, **env):
    return subprocess.run(command, cwd=ROOT / "scripts",
                          env=environment(**env), capture_output=True, check=True)


class OutputTests(unittest.TestCase):
    def test_plain_pipe_and_literal_payload(self):
        for name, command in COMMANDS.items():
            with self.subTest(runtime=name):
                result = run_status(command)
                self.assertEqual(result.stdout, b"data\n")
                self.assertEqual(result.stderr, PLAIN)

    def test_color_environment_precedence(self):
        cases = [({}, False), ({"CLICOLOR_FORCE": "1"}, True),
                 ({"CLICOLOR_FORCE": "0"}, False), ({"CLICOLOR_FORCE": ""}, False),
                 ({"CLICOLOR_FORCE": "1", "NO_COLOR": "1"}, False),
                 ({"CLICOLOR_FORCE": "1", "NO_COLOR": ""}, True),
                 ({"CLICOLOR_FORCE": "1", "TERM": "dumb"}, False),
                 ({"CLICOLOR_FORCE": "1", "CLICOLOR": "0"}, True),
                 ({"CLICOLOR": "0"}, False)]
        for name, command in COMMANDS.items():
            for env, colored in cases:
                with self.subTest(runtime=name, env=env):
                    result = run_status(command, **env)
                    self.assertEqual(result.stdout, b"data\n")
                    self.assertEqual(b"\x1b[" in result.stderr, colored)
                    if not colored:
                        self.assertEqual(result.stderr, PLAIN)

    def test_stderr_terminal_colors_but_stdout_terminal_does_not(self):
        for name, command in COMMANDS.items():
            for destination in ("stdout", "stderr"):
                with self.subTest(runtime=name, destination=destination):
                    master, slave = pty.openpty()
                    try:
                        streams = {"stdout": subprocess.PIPE, "stderr": subprocess.PIPE}
                        streams[destination] = slave
                        child = subprocess.Popen(command, cwd=ROOT / "scripts",
                                                 env=environment(), **streams)
                        # Keep our slave open until reading: macOS may discard
                        # buffered PTY bytes when the final slave closes.
                        stdout, stderr = child.communicate(timeout=10)
                        self.assertEqual(child.returncode, 0)
                        self.assertTrue(select.select([master], [], [], 10)[0], "PTY output timed out")
                        captured = os.read(master, 65536)
                        if destination == "stderr":
                            self.assertIn(b"\x1b[1mphux\x1b[0m: \x1b[31merror\x1b[0m:", captured)
                            self.assertEqual(stdout, b"data\n")
                        else:
                            self.assertEqual(stderr, PLAIN)
                            self.assertEqual(captured.replace(b"\r\n", b"\n"), b"data\n")
                    finally:
                        if slave is not None:
                            os.close(slave)
                        os.close(master)

    def test_rendering_does_not_change_caller_variables_or_exit(self):
        code = '. "$1"; phux_label=original; phux_style=original; phux_error test; test "$phux_label/$phux_style" = original/original; exit 7'
        for shell in SHELLS:
            with self.subTest(shell=shell):
                result = subprocess.run([shell, "-eu", "-c", code, "test", str(HELPER)],
                                        env=environment(CLICOLOR_FORCE="1"), capture_output=True)
                self.assertEqual(result.returncode, 7)
                self.assertEqual(result.stdout, b"")

    def test_the_served_installer_stands_alone_and_errors_keep_stdout_empty(self):
        for shell in SHELLS:
            with self.subTest(shell=shell):
                # Copy just the served script, proving there is no repo helper dependency.
                with tempfile.TemporaryDirectory() as directory:
                    script = Path(directory) / "install.sh"
                    shutil.copyfile(ROOT / "scripts/install.sh", script)
                    result = subprocess.run([shell, str(script), "--not-a-flag"],
                                            cwd=directory, env=environment(), capture_output=True)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stdout, b"")
                self.assertTrue(result.stderr.startswith(b"phux: error: "), result.stderr)
                self.assertNotIn(b"\x1b", result.stderr)

if __name__ == "__main__":
    unittest.main()
