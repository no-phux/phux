---
name: using-phux
description: Drives persistent phux terminals from Grok. Use for REPLs, debuggers, dev servers, durable shell state, or supervising another terminal-hosted agent. Prefer a one-shot shell for independent commands.
---

# Using phux from Grok

The `phux` MCP server from this plugin is the tool catalog. When
`PHUX_TERMINAL_ID` is set, that pane hosts you. Never send input to it.

1. Run `phux agent list --json` and `phux ls --json` for exact selectors.
2. Read with `phux snapshot --json @N` before acting.
3. Act with `phux run --json --timeout SECS @N "COMMAND"`. Use `send-keys`
   for interactive keys and `paste` for multiline text. Put flags before
   the target.
4. Bound observation with `--timeout`: `wait`, `watch`, `agent wait`, or
   `resource wait`. A quiet pane is not proof that the command finished.
5. Snapshot again.

`phux --skill=quick` prints the same guide from the installed binary. Trust
that copy when it disagrees with this file.

A kill or destructive signal needs the exact target, a snapshot, and
affirmative confirmation. `phux` refuses `--yes` when it has no terminal to
ask. You cannot approve your own grant.
