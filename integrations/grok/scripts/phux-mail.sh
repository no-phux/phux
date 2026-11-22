#!/bin/sh
# This pane's unread mail as the hook's one JSON reply, from
# `phux mail hook-context`. Registered as its own hook beside the lifecycle
# hook, so its timeout is spent on taking the mail alone, never behind the
# lifecycle work. The take happens once, and output that cannot be written
# is released for the next hook. Nothing is typed into the pane. An agent
# idle at its prompt runs no hook, so its mail waits for the next one.
# Grok reads context only after a tool (the PostToolUse pair) and a block at
# Stop, so only those events register it; its other events are passive.
set -u

[ "$#" -eq 1 ] || exit 2
[ "${PHUX_AGENT_REPORTING:-1}" != 0 ] || exit 0
[ -n "${PHUX_TERMINAL_ID:-}" ] || exit 0

"${PHUX_AGENT_PHUX_BIN:-phux}" mail hook-context --event "$1" 2>/dev/null || :
