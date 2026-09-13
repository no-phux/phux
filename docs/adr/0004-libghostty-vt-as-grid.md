---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0004 — libghostty-vt is the canonical grid

**TL;DR.** Per-pane terminal state on the server is a `libghostty_vt::Terminal`, not a hand-written grid. We get the most standards-compliant emulator available, upstream bug fixes flow in for free, and phux's scope shrinks: we do not ship a VT parser. The coupling to libghostty's release cadence is managed via pinned commits.

Status: Accepted
Date: 2026-05-24
See [ADR-0013](./0013-libghostty-bytes-on-wire.md) for the use the server now makes of the grid: PTY bytes are forwarded as-is and the grid backs only the attach snapshot.
See [ADR-0016](./0016-terminal-id-as-wire-primary.md) for the rename: PANE_OUTPUT and PANE_SNAPSHOT below are TERMINAL_OUTPUT and TERMINAL_SNAPSHOT on the wire.

## Context

Every pane has a terminal screen state — the grid, plus scrollback,
plus modes, plus cursor. The server must maintain this state because
it is the source of truth that snapshots are synthesized from when a
client attaches, and (originally) the source of truth that wire-side
diffs were computed from (ADR-0002, now superseded by ADR-0013 —
content flows as VT bytes on the wire; the server-side grid still
backs `PANE_SNAPSHOT` synthesis).

The implementation can be:

- A hand-written grid (tmux: `grid.c`, ~1600 LOC, plus `screen-write.c`,
  ~2500 LOC, plus `input.c` for VT parsing).
- `libghostty_vt::Terminal` from the safe Rust crate over libghostty-vt.

## Decision

Per-pane state is a `libghostty_vt::Terminal`. The server feeds PTY
output into it and forwards those same bytes to attached clients via
`PANE_OUTPUT` (ADR-0013); on attach, the server walks the resulting
grid via `RenderState` / `grid_ref()` to synthesize the
`PANE_SNAPSHOT` VT replay bytes that catch a new client up.

## Rationale

- **Correctness.** Terminal emulation is significantly harder than it
  looks: VT sequences, SGR, OSC, DEC private modes, modes within
  modes, sixel and kitty graphics, character sets, scrolling regions,
  unicode width and grapheme clustering. libghostty's terminal core
  is the most standards-compliant implementation available, and it
  has been hammered on by real users running real workloads.
- **Free upgrades.** Bugs fixed upstream become bugs fixed in phux.
  Newly supported VT features arrive automatically.
- **Reduces phux's surface.** We don't ship a VT parser. Our scope is
  multiplexing, layout, IPC, and rendering — not terminal emulation.
  This is the largest single way phux is smaller than tmux.
- **Aligned with the protocol shape.** libghostty-vt's `RenderState`
  exposes a row/cell iterator API that drives `PANE_SNAPSHOT` byte
  synthesis on attach (ADR-0013) and provides local dirty tracking
  on the server's own grid. (Pre-ADR-0013 this same iterator drove
  cell-level diff emission; the iterator is just as well-suited to
  the byte-synthesis path.)

## Tradeoffs

- **Coupling to libghostty's release cadence.** We pin a specific
  commit and upgrade deliberately. libghostty-rs's vendored build
  mode makes the build hermetic per pin.
- **API churn risk.** Mitigated by libghostty-vt's stable C ABI and
  the safe Rust crate's semver discipline.

## Alternatives considered

- **Hand-written grid (tmux's approach).** Reimplements work that
  exists, less standards-compliant in the wild, and ties the project's
  fate to our ability to keep up with terminal emulation as it
  continues to evolve (kitty kbd, OSC 133, image protocols, …).
  Rejected.
- **vte/alacritty/wezterm terminal cores.** All viable alternatives,
  but none are a first-class Rust library with the same combination
  of completeness, standards compliance, and ongoing investment. We
  bet on libghostty.
