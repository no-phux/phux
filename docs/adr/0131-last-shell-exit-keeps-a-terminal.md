---
audience: contributors
stability: stable
last-reviewed: 2026-09-19
---

# 0131 — Last-shell exit keeps a terminal

**TL;DR.** Natural process exit of a session's only live Terminal respawns
a default shell in that same Terminal. Clients keep the id, layout slot,
and attach. Explicit kill and Close Tab still close the pane.

Status: Accepted
Date: 2026-09-19

## Context

[ADR-0105](./0105-sessions-can-outlive-their-last-window.md) left default
sessions on the tmux cascade: the last pane's process exit reaped the
session, and after a client had been served the server self-exited
([ADR-0063](./0063-ephemeral-server-lifetime.md)). Keep-empty sessions
survived as an empty state. Cockpit and the TUI then either detached or
showed Empty session.

That is wrong for a GUI terminal and for a TUI that should feel like
one: typing `exit` in the last shell should leave a prompt, not drop
the user out of the session. Implementing the replacement in each
client would fork policy. The server already owns PTY EOF.

## Decision

1. **Natural last-shell exit replaces the child in place.** If the
   exiting Terminal is the session's only live Terminal and nobody
   recorded `Killed` / `ParentClosed` / `ServerShutdown`, the engine
   resets the grid, execs the configured default shell, and resyncs
   subscribers. The wire id does not change. No `RESOURCE_CLOSED`.
2. **Kill and Close Tab still close.** `KILL_RESOURCES` and
   `CLOSE_TAB_RESOURCES` mark the pane closing, so replacement does
   not run. Keep-empty Close Tab still leaves an empty session.
3. **Server self-exit still follows an empty session after a served
   client.** A last-session kill can still stop the daemon. `--exit-after-idle`
   is unchanged.

## Why

One server-side decision keeps Cockpit, TUI, and headless observers
honest without each inventing a placeholder pane. In-place replacement
avoids a last-pane detach race: clients never see an empty layout.

## Tradeoffs

- `exit` in the last TUI pane no longer ends the attach. Detach and
  kill-session remain the way out.
- Harnesses that expected last-pane `exit 0` to reap the server must
  kill the pane instead.
- Retain-on-exit of that last natural exit is skipped; a live shell
  outranks a retained corpse.

## Alternatives

**Spawn a new Terminal id.** Clients ignore unsolicited
`RESOURCE_SPAWNED` and detach on last-pane `RESOURCE_CLOSED`.

**Keep-empty every session.** Empty state is not a prompt, and it
changes CLI session lifetime.

**Client-side respawn.** Policy would drift across Cockpit, TUI, and
agents.
