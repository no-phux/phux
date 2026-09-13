---
audience: contributors
stability: stable
last-reviewed: 2026-07-25
---

# 0056 — Cross-session Terminal move

**TL;DR.** A new `MOVE_TERMINAL` command re-parents a live Terminal into
another window, including one in a different session, without restarting its
process. Ownership moves on L1; geometry stays a client-written L3 concern, as
with spawn placement. The move is atomic on the server and rolled back by the
caller if the destination layout write fails.

Status: Accepted
Date: 2026-07-25

## Context

`phux move-pane` today edits one session's layout envelope. It cannot move a
pane between windows of different sessions, because nothing on the wire changes
a Terminal's owning window: `Terminal.window` is set at creation in
`phux-core`'s registry and never reassigned. `spatial.rs::same_session()`
enforces this, rejecting the cross-session case with a `cross_session` error.

The workaround is to kill the pane and spawn a replacement, which is not the
same operation — it destroys the process, its scrollback, its agent session,
and anything the user was running. For an agent fleet the loss is total: the
whole point of moving a pane is that the agent inside it keeps working.

ADR-0050 settled ownership for *new* Terminals (`SPAWN_TERMINAL.owner_terminal`)
and deliberately left existing ones alone. This ADR opens that door.

## Decision

Add `MOVE_TERMINAL { request_id, terminal, owner_terminal }` and its typed
reply `TERMINAL_MOVED { request_id, result }`, mirroring the
`SPAWN_TERMINAL` / `TERMINAL_SPAWNED` pair.

`owner_terminal` is an **ownership address**, exactly as in ADR-0050: the
server re-parents `terminal` into the window that currently owns
`owner_terminal`. It conveys no split direction, ratio, or focus. The
destination window may belong to a different session; that is the whole point.

The server performs the re-parent under its state lock: resolve both
Terminals, verify both are local, move the registry entry, and reply. Either
the whole re-parent lands or none of it does. Local-only: a satellite Terminal
on either end is `MoveError::UnsupportedSatelliteRoute`, matching spawn.

**Layout stays with the client.** After a successful reply the caller removes
the leaf from the source session's layout and inserts it into the
destination's, through the same shared `LayoutOps` read/mutate/write path that
placed spawns. Two layout writes, because two sessions have two envelopes.

**Rollback is the caller's.** If the destination layout write fails after the
re-parent succeeded, the caller issues the inverse `MOVE_TERMINAL` to restore
the original owner and reports failure — the same shape as the spawn-placement
rollback in `commands/spawn.rs`, which removes a spawned Terminal when its
layout publication fails.

The pane's process, PTY, scrollback, metadata, and agent record are untouched.
A move is a change of ownership, not of identity: the `TerminalId` is stable
across it, so subscriptions and outstanding `phux wait` calls survive.

## Rationale

Ownership genuinely belongs on L1 — the registry is the only thing that knows
which window holds a Terminal, and it lives on the server. Geometry genuinely
belongs on L3, where every other layout decision already lives (ADR-0019).
Splitting the operation along that seam keeps both halves where their state is,
and reuses the placement machinery spawn already proved.

Making the server do the layout edits instead would require it to understand
the TUI's layout metadata convention, which is precisely the coupling ADR-0019
removed. Making the client do the re-parent is impossible: it does not own the
registry.

Two layout writes rather than one atomic cross-session transaction is a
deliberate acceptance of the last-write-wins model ADR-0019 already chose.
A cross-session layout transaction would be the first compare-and-set on the
wire, for a case where the failure is visible and recoverable.

## Tradeoffs

There is a window between the re-parent and the destination layout write where
the Terminal is owned by the destination window but not yet present in its
layout envelope. A client rendering in that instant shows the pane in neither
session. The window is a single round trip, the state is self-correcting on the
next layout read, and the alternative is a wire transaction — but it is real,
and it is why rollback is specified rather than left to taste.

Rollback is best-effort. If the inverse move also fails, the Terminal is owned
by the destination window with no leaf in either envelope. It is recoverable
(`phux insert-pane` places it) and reported, not silent.

When the moved Terminal was its source window's only pane, reaping leaves no
`owner_terminal` in that window to address an inverse move. This is the
deterministic edge of the same best-effort rule: the caller cannot issue an
inverse, reports that ownership was not restored, and names inventory plus
`insert-pane` as recovery. A session-scoped client observing a source session
reaped by the move is detached; independent per-Terminal subscriptions survive
the stable id.

Moving the last pane out of a window leaves an empty window, which the server
already reaps by its existing rules. No new lifecycle.

## Alternatives considered

**Kill and respawn.** Already available, and already wrong: it destroys the
process, which is the one thing a move must preserve.

**Server-side layout edits.** Rejected — it puts the TUI's layout convention
inside the daemon, undoing ADR-0019.

**Extend `SPAWN_TERMINAL` with an "adopt existing" mode.** Rejected: spawn's
contract is that it returns a *new* `TerminalId`, and overloading it would make
`TerminalSpawned` ambiguous about whether a process was created. A distinct
verb keeps both replies honest.

**Session-scoped move only** (same session, across windows). Rejected as a
half-measure: the registry change is identical, and the cross-session case is
the one agent fleets actually need.
