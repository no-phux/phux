#!/bin/sh
# This pane's unread mail as the hook's one JSON reply, from
# `phux mail hook-context`. Registered as its own hook beside the lifecycle
# hook, so its timeout is spent on taking the mail alone, never behind the
# lifecycle work. The take happens once, and output that cannot be written
# is released for the next hook. Nothing is typed into the pane. An agent
# idle at its prompt runs no hook, so its mail waits for the next one.
# SessionStart, UserPromptSubmit and the PostToolUse pair carry it as
# context; Stop carries it as a block, so the agent goes on and handles it.
set -u

[ "$#" -eq 1 ] || exit 2
[ "${PHUX_AGENT_REPORTING:-1}" != 0 ] || exit 0
# The CLI validates a native chat's private record and selects its inbox.
# An ordinary address is not pane authority; lifecycle still needs a real pane.
if [ -z "${PHUX_TERMINAL_ID:-}" ]; then
  [ -n "${PHUX_CHAT_RECORD:-}" ] || exit 0
fi

"${PHUX_AGENT_PHUX_BIN:-phux}" mail hook-context --event "$1" 2>/dev/null || :
