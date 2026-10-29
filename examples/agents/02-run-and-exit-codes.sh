#!/usr/bin/env bash
#
# 02 — run a command and trust its exit code: `run`.
#
# For "run this and tell me whether it worked," reach for `run` instead of
# send-keys + polling. It submits the command, waits for it to finish, and
# reports {command, exit_code, output, duration_ms, truncated} — and the
# `phux` process EXITS WITH THE COMMAND'S OWN CODE, so it composes like a
# shell (`phux run a && phux run b`).
#
# `run` assumes a POSIX shell (sh/bash/zsh): it brackets the command with
# sentinels to read `$?`. It cannot capture a command that REPLACES the
# shell (`exit`, `exec`) — that kills the pane's shell, and with it the
# session. To observe a non-zero code without killing the shell, run it in
# a subshell: `sh -c 'exit 7'`.
#
# Run it:   bash examples/agents/02-run-and-exit-codes.sh

# shellcheck source=examples/agents/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
phux_start_server

# --- run: human form --------------------------------------------------------
# Prints the captured output, then an `exit=<code> (<ms>)` footer.
section "phux run (human)"
phux run "$PHUX_SESSION" "echo building...; echo done"

# --- run: JSON form ---------------------------------------------------------
# The machine contract. Flags (--json/--timeout) MUST precede the command,
# or they get joined into it.
section "phux run --json"
phux run --json "$PHUX_SESSION" "echo hello world"

# --- exit-code mirroring ----------------------------------------------------
# The `phux` process exits with the command's code. Use it directly in
# shell control flow, exactly like running the command locally.
section "exit-code mirroring"
if phux run "$PHUX_SESSION" "true" >/dev/null; then
    echo "  'true' -> phux exited 0 (success branch)"
fi
# A subshell that exits non-zero: the code is mirrored without killing the
# pane's shell.
rc=0
phux run "$PHUX_SESSION" "sh -c 'exit 7'" >/dev/null || rc=$?
[[ "$rc" -eq 7 ]]
echo "  'sh -c exit 7' -> phux exited $rc"

# --- chaining like a shell --------------------------------------------------
# Because exit codes mirror, `&&` / `||` chain across runs.
section "chaining on success"
phux run "$PHUX_SESSION" "true" >/dev/null \
    && phux run "$PHUX_SESSION" "echo 'second step ran because first succeeded'"

section "short-circuit on failure"
rc=0
phux run "$PHUX_SESSION" "sh -c 'exit 3'" >/dev/null || rc=$?
[[ "$rc" -eq 3 ]]
echo "  first step failed (code $rc); did NOT run the second"

# --- reading the JSON result programmatically ------------------------------
# An agent typically wants exit_code + output as data, not text.
section "parse the JSON result"
phux run --json "$PHUX_SESSION" "echo line1; echo line2" | python3 -c '
import json, sys
r = json.load(sys.stdin)
print("command : " + r["command"])
print("exit    : {}".format(r["exit_code"]))
print("took    : {} ms".format(r["duration_ms"]))
print("trunc   : {}".format(r["truncated"]))
print("output  :")
for line in r["output"].splitlines():
    print("    " + line)
'

# NOTE on `truncated`: `run` output is viewport-bounded. If a command
# prints more than fits on screen, `output` is the visible tail and
# `truncated` is true. Full capture awaits scrollback support.
