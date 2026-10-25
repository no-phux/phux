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
        self.cursor_queries = 0
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
                    # ConPTY/PowerShell asks its terminal host for the cursor
                    # position when returning from WSL. Act as that host rather
                    # than leaving a legitimate DSR request unanswered.
                    queries = self.output.count("\x1b[6n")
                    for _ in range(queries - self.cursor_queries):
                        self.child.write("\x1b[1;1R")
                    self.cursor_queries = queries
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
    runner.write_text(r'''param([string] $Socket, [string] $Session)
$ErrorActionPreference = 'Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class PhuxConsoleModes {
    [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int which);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetConsoleMode(IntPtr handle, out uint mode);
    public static uint Read(int which) {
        uint mode;
        if (!GetConsoleMode(GetStdHandle(which), out mode)) throw new System.ComponentModel.Win32Exception();
        return mode;
    }
}
'@
function Get-ConsoleState {
    return (@([PhuxConsoleModes]::Read(-10), [PhuxConsoleModes]::Read(-11),
        [Console]::InputEncoding.CodePage, [Console]::OutputEncoding.CodePage,
        [Console]::InputEncoding.GetPreamble().Length, [Console]::OutputEncoding.GetPreamble().Length,
        [Console]::CursorVisible) -join ',')
}
$before = Get-ConsoleState
& (Join-Path $PSScriptRoot 'phux.ps1') --socket $Socket attach $Session
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$after = Get-ConsoleState
if ($before -cne $after) { throw "Console state changed: $before -> $after" }
Write-Output 'PHUX_MODES_RESTORED'
$typed = Read-Host 'After detach'
if ($typed -cne 'post-detach-input') { throw 'Post-detach keyboard input was not restored' }
Write-Output 'PHUX_CONSOLE_RESTORED'
''', encoding="utf-8")
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
        terminal.expect("PHUX_MODES_RESTORED")
        assert "\x1b[?1049l" in terminal.output, "Alternate screen was not left on detach"
        terminal.child.write("post-detach-input\r")
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
