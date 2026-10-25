"""Exercise the installed PowerShell launcher in a real Windows ConPTY."""

from pathlib import Path
import re
import select
import sys
import time

from winpty import PtyProcess

CSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


class Terminal:
    def __init__(self, command: list[str]):
        self.child = PtyProcess.spawn(command, dimensions=(36, 120), backend="0")
        self.output = ""
        print(f"Windows ConPTY child pid: {self.child.pid}", flush=True)

    def expect(self, pattern: str, timeout: float = 30) -> re.Match[str]:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            match = re.search(pattern, CSI.sub("", self.output))
            if match:
                return match
            if select.select([self.child.fileobj], [], [], max(0, min(0.5, deadline - time.monotonic())))[0]:
                try:
                    self.output += self.child.read(65536)
                except EOFError:
                    break
        raise AssertionError(f"ConPTY did not produce {pattern!r}: {self.output[-12000:]!r}")

    def size(self, marker: str) -> tuple[int, int]:
        self.child.write(f"printf '{marker}_%s\\n' \"$(stty size)\"\r")
        match = self.expect(marker + r"_(\d+) (\d+)")
        return int(match[1]), int(match[2])


def qualify(launcher: str, socket: str, session: str, host: str) -> None:
    assert sys.platform == "win32", "Run this test in native Windows Python"
    runner = Path(launcher).with_name("terminal-smoke.ps1")
    runner.write_text(
        "param([string] $Socket, [string] $Session)\n"
        "& (Join-Path $PSScriptRoot 'phux.ps1') --socket $Socket attach $Session\n"
        "if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }\n"
        "Write-Output 'PHUX_CONSOLE_RESTORED'\n",
        encoding="utf-8",
    )
    terminal = Terminal([host, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(runner), socket, session])
    try:
        terminal.expect(re.escape(session))
        # Source echo contains %s, not this joined marker: seeing the latter
        # proves the keyboard reached the shell and its output was rendered.
        before = terminal.size("WIN_BEFORE")
        terminal.child.setwinsize(52, 160)
        after = before
        for attempt in range(10):
            after = terminal.size(f"WIN_AFTER_{attempt}")
            if after[0] > before[0] and after[1] > before[1]:
                break
        assert after[0] > before[0] and after[1] > before[1], (before, after)
        terminal.child.write("\x01d")
        terminal.expect("PHUX_CONSOLE_RESTORED")
        deadline = time.monotonic() + 10
        while terminal.child.isalive() and time.monotonic() < deadline:
            time.sleep(0.05)
        assert not terminal.child.isalive(), "TUI did not exit after detach"
        assert terminal.child.exitstatus == 0, terminal.child.exitstatus
        print(f"Windows ConPTY rendering/input/resize/detach/restoration passed: {before} -> {after}", flush=True)
    finally:
        if terminal.child.isalive():
            terminal.child.terminate(force=True)  # Only our recorded child PID.
        terminal.child.close()
        runner.unlink(missing_ok=True)


if __name__ == "__main__":
    qualify(*sys.argv[1:])
