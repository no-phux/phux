---
audience: contributors
stability: stable
last-reviewed: 2026-07-15
---

# 0048 — Drag-to-resize panes and default outer-terminal mouse capture

**TL;DR.** The client enables its own outer-terminal mouse tracking
(`?1006h` SGR + `?1002h` button-motion) on attach and restores it on
detach, so divider drags work by default. Divider cells grab the split
they control; a press-motion-release machine adjusts that split's ratio
and commits via the existing `SET_METADATA` layout path. Cells inside a
pane still forward to that pane unchanged. A global `mouse` config gate
and the per-pane `set-pane mouse off` escape hatch protect inner TUIs.

Status: Accepted
Date: 2026-06-17

## Context

Drag-to-resize is the documented-but-unbuilt half of
[docs/consumers/tui.md](../consumers/tui.md) §7. The pieces exist.
The split model is ratios (`LayoutNode::Split{dir, ratio, ..}` in
`crates/phux-client-core/src/layout/mod.rs`). `compute_layout_in`
(`crates/phux-client-core/src/multi_pane/layout.rs`) already produces
per-split `DividerSegment`s in `rasterize.rs` — but they are
`pub(super)` and only the rasterized glyph cells survive in
`PaneLayout`; the segment→split identity is discarded. Mouse hit-testing
exists (`route_mouse_event`, `mouse.rs`) but a divider hit returns the
`RouteDecision::DividerNoOp` stub. Keyboard resize already adjusts the
right split ratio (`apply_resize`, `actions.rs`) with `MIN_PANE_CELL`
and `clamp_ratio`, persisting through `SET_METADATA`
(`input_dispatch.rs`).

The one genuinely new behaviour is capture. The client enters the alt
screen (`write_enter_alt_screen`, `driver.rs`) but never emits mouse
DECSET, so today it only sees mouse reports when an *inner* program
turns tracking on. To grab divider drags by default the client must
enable its *own* outer tracking — which then intercepts every mouse
event, including ones inner TUIs (vim, htop) want.

## Decision

1. **Capture by default.** On attach, after the alt-screen enter, the
   client emits `\x1b[?1002h\x1b[?1006h` (button-event tracking + SGR
   extended coordinates). On detach/reset it emits `\x1b[?1006l?1002l`
   ahead of the existing `?1049l`. Capture is gated by the existing
   `mouse` config key (`phux-config` schema, currently declared but
   unconsumed); `mouse = false` skips the DECSET entirely and restores
   the pre-PR pass-through-only behaviour.

2. **Routing keeps inner mouse working.** Routing is unchanged for pane
   interiors: a cell inside a pane `Rect` forwards `INPUT_MOUSE` with
   pane-local coords, exactly as today. Only divider cells change
   meaning. The client does **not** consult the server's per-pane
   tracking mode for routing; it forwards every pane-interior event and
   the server's `PerTerminalMouseEncoder` already produces empty bytes
   when the inner app has no mode enabled (from its exact mode snapshot).
   That keeps routing a pure client-local geometry decision and avoids a
   new mode-mirror frame.

3. **Escape hatch: global gate now, per-pane gate as follow-up.** This
   PR ships the global `mouse = true/false` toggle as the minimum
   release valve. The per-pane `set-pane mouse off` documented in §7 is
   recorded as follow-up work — it needs a `set-pane` verb and per-pane
   client state that this PR does not introduce. Until then a pane that
   wants raw outer bytes is served by `mouse = false`.

4. **Data plumbing.** `PaneLayout` gains a `Vec<DividerHit>` carrying,
   per divider cell, the path to the `Split` node it controls and that
   split's axis. `route_mouse_event` returns a new
   `RouteDecision::Divider{ node_path, axis }` instead of `DividerNoOp`
   when a press lands on a divider cell. The driver runs a drag machine:
   press = record grabbed node + anchor; motion = recompute the split's
   ratio from the pointer's outer-cell position and re-apply; release =
   stop. Each motion calls a node-targeted resize (a generalization of
   `apply_resize` that takes a node path instead of walking up from
   focus) reusing `MIN_PANE_CELL` and `clamp_ratio`, and sets
   `layout_mutated` continuously plus `set_metadata` on commit — the same
   persistence path the keyboard action uses.

## Why

Capture by default is the only way to honour "drag and resize by
default" — without our own DECSET the client is deaf to the pointer over
a divider whenever the inner app has no mouse mode, which is the common
case (a shell). `?1002h` (not `?1003h` any-motion) is the minimum: we
need motion only while a button is held, and any-motion floods the wire
with hover traffic we discard. `?1006h` (SGR) is mandatory for columns
past 223; X10 coordinate encoding cannot address a wide terminal.

Routing by geometry alone, not by mirrored inner mode, keeps the
hit-test pure and synchronous (`route_mouse_event` already is) and
leans on the encoder's existing empty-bytes behaviour rather than
inventing a mode-sync frame. Reusing the keyboard resize math means
drag and keybind resize cannot diverge — same ratio adjustment, same
floor, same `SET_METADATA` envelope, same multi-consumer broadcast.

## Tradeoffs

- **Host text selection.** Enabling outer tracking suppresses the host
  terminal's native click-drag selection inside the phux viewport. We
  rely on the near-universal terminal convention that holding **Shift**
  bypasses application mouse reporting and restores native selection;
  this is documented in §7, not enforced by us. Hosts that do not honour
  Shift-bypass lose easy selection until `mouse = false`.
- **No per-pane granularity yet.** Until `set-pane mouse off` lands, the
  only opt-out is global. A single raw-bytes-hungry pane forces the
  whole session into `mouse = false`.
- **Wider `PaneLayout`.** Carrying node paths grows the struct and the
  tiling proptests must assert the hit map stays consistent with the
  rasterized cells.

## Alternatives

- **Leave capture to inner apps (status quo).** Rejected: divider drag
  would silently not work in a plain shell, contradicting the
  requirement.
- **`?1003h` any-motion tracking.** Rejected: floods the wire with hover
  events we never use; button-motion is sufficient for grab-drag.
- **Consult per-pane tracking mode before forwarding.** Rejected for
  this PR: requires a server→client mode-mirror frame; the encoder's
  empty-bytes-when-no-mode behaviour already yields the same result with
  no new wire surface.
- **A modifier-gated resize (e.g. Alt+drag anywhere).** Rejected:
  divider cells are an unambiguous, discoverable grab target and match
  the documented §7 model; a global modifier collides with inner apps.
