---
audience: contributors
stability: stable
last-reviewed: 2026-10-06
---

# 0145 — The TUI sizes its panes and casts no viewport vote

**TL;DR.** The TUI attaches with a zero viewport, so it casts no
`window-size` vote, and it sizes every pane only with `RESIZE_TERMINAL`,
which gains an optional cell pixel size (fields 4/5). The wire change is
additive and gated on `features_ext.RESIZE_CELL_PX`. A server without
that bit still gets the old outer-window vote.

Status: Accepted
Date: 2026-10-06

## Context

A TUI pane is a tile: the outer window minus the sidebar, the status bar,
split borders and other panes. The outer window is never a pane's size. The
TUI still sent it as its `ATTACH` viewport and on every outer resize
(`VIEWPORT_RESIZE`). The server sized every pane in the session to that
vote (ADR-0027, ADR-0062) before the TUI's per-pane `RESIZE_TERMINAL` set
the tile. A departing vote also shrank the panes to the headless 80x24. A
sidebar session switch is a detach and an attach, so each switch moved every
pane through three PTY sizes: 80x24, the full window, then the tile.

A full-screen program redraws on each `SIGWINCH`. Claude Code positions
words by absolute column, so a redraw laid out for the full window that
lands after the tile resize is clamped at the right edge. The first letter
of each word piles up in the last column and the rest wraps to the next row,
and the server's canonical grid keeps that damage. Reflowing the grid later
spreads the words across fixed columns. Users saw this as a "wrap at the
edge" rendering bug in their real sessions.

L1 §9 already lets a client attach with a zero viewport and size panes only
through `RESIZE_TERMINAL`. The TUI could not use that path because the vote
was the only way to report cell pixel size. The server needs cell pixel size
for PTY `winsize` pixels, XTWINOPS and mode-2048 size replies, and for turning
`INPUT_MOUSE` pixel positions back into cells.

## Decision

- **Wire.** `RESIZE_TERMINAL` gains optional `cell_width_px: u16` (field 4)
  and `cell_height_px: u16` (field 5). They are sent as a pair or not at all.
  A frame carrying only one, or a zero on either axis, has no cell size.
  When the pair is present the server applies it to the pane exactly as it
  applies a cell size derived from a viewport vote. When it is absent the
  pane keeps its last cell size, which is today's behavior. A new
  extended-word bit, `features_ext.RESIZE_CELL_PX = 0x00000002`, says the
  server applies the pair. `PROTOCOL_VERSION` stays `0.9.0`.
- **TUI.** When the bit is advertised, the TUI attaches with a zero viewport
  (initial attach, re-attach, session switch and rebootstrap). It sends no
  `VIEWPORT_RESIZE` on an outer resize. Its existing per-pane reflow sends
  each pane's tile with the cell size the host reports. If the host reports
  no pixel metrics, the TUI omits the pair and keeps its 8x16 mouse fallback,
  which matches the server default. Without the bit the TUI behaves as
  before.
- **Unusable panes still reset.** A vote-free detach re-resolves nothing,
  except for a Terminal it leaves with no subscriber and smaller than
  `MIN_USABLE_TERMINAL_DIMS = (10, 3)` on either axis (not under `manual`).
  That Terminal returns to the headless 80x24. The check lives in the
  server's one detach re-resolve.
- **Amends ADR-0027 and ADR-0062 for the TUI.** The `window-size` policy
  still governs any client that votes: agents, the desktop app, mobile and
  old TUIs. It no longer governs panes sized only by vote-free TUIs. Between
  two such TUIs on one pane, the last `RESIZE_TERMINAL` wins.

## Why

The vote was wrong in shape, not just in timing: one number for a whole
session can never be the size of a tile. Reordering the frames or debouncing
the resizes would still pass through a size that no pane has. Dropping the
vote removes the wrong size entirely, and the zero-viewport contract is
already specified, tested and used by the headless composite.

Cell size had to reach the server some other way. The pane-sizing frame is
the natural carrier because the cell size changes exactly when the TUI
re-sizes its panes, and a new frame or command would duplicate it. Appending
fields to an existing frame is the additive shape proto.md §6.3 requires: an
old server skips them by length. The feature bit lets the TUI tell an old
server, which would ignore the pixels, from a new one, so it falls back to
voting instead of leaving that server at the 8x16 default.

## Tradeoffs

- **No `smallest` arbitration for TUIs.** Two TUIs viewing one pane at
  different tile sizes now take turns; the last resize wins and the other
  view letterboxes or crops. Before, the vote picked the smallest outer
  window, which was never a tile size either. A voting client mixed with a
  vote-free TUI can still resize the pane when the voting client's viewport
  changes. The TUI only reasserts its tiles on its own layout and resize
  events.
- **Cell size only from per-pane resizes.** A pane the TUI has not sized
  since attach (for example, one spawned through `SPAWN_INITIAL_SIZE`
  without a follow-up resize) keeps its previous or default cell size until
  its tile is next sent. Pane creation already behaved this way under the
  vote.
- **Satellites.** A hub forwards the pair on a satellite-routed
  `RESIZE_TERMINAL`. A satellite that predates the fields ignores it, and
  the pane keeps its cell size there.
- **The minimum is a heuristic.** 10x3 is about the smallest grid where a
  shell can show a prompt, a typed command and one output line. A real tile
  is only that small in a degenerate outer window. A deliberately tiny,
  unwatched pane (say, a 16-way split) is reset on detach. A GUI or TUI
  relaunch never trips the reset, because normal tiles are far larger. It
  is a constant, not config: nobody tunes a floor for unusable panes.
- **Old servers.** A new TUI against an old server votes as before, so it
  keeps the old flap until the server is upgraded.

## Alternatives

**A zero-cell viewport that carries pixels.** This reads a cell size out of
an `ATTACH` or `VIEWPORT_RESIZE` that casts no vote. It changes the meaning
of bytes already on the wire, because §9.2.1 defines pixels relative to the
same report's cells. That is a minor-version break under §6.3. Rejected.

**Keep the vote, debounce the resizes.** This narrows the window between
the outer-window resize and the tile resize but does not remove it, and it
delays every legitimate resize. Rejected.

**Stop shrinking panes to 80x24 on detach.** This removes one of the three
sizes and leaves the outer-window-then-tile flap that caused the damage.
Rejected as insufficient.

**Accept 8x16 for vote-free TUIs.** Mouse would still be correct if the TUI
also scaled by 8x16, but every image and cell-size query would report a
cell that belongs to no real screen, regressing §9.2.1. Rejected.
