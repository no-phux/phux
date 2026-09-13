---
audience: contributors
stability: stable
last-reviewed: 2026-05-31
---

# 0020 — Layered render: ratatui chrome over libghostty pane interiors

**TL;DR.** phux-client uses two disjoint renderers in one process: ratatui for chrome (status bar, dividers, borders, overlays) and libghostty for pane interiors. ratatui marks pane rectangles `skip=true` so they carve out holes; pane bytes layer into those holes from a client-side `Terminal` mirror. SGR resets at every transition, one renderer positions the cursor, and ratatui imports are confined to `render/`.

Status: Accepted
Date: 2026-05-27

## Context

phux-client paints its frames as raw VT bytes to stdout. Today every
piece of UI — pane interiors, dividers, the status bar — is hand-rolled
cell positioning under `crates/phux-client/src/attach/`. The hot path
lives in `paint.rs` (`paint_full_frame`, `paint_focused_pane`),
multi-pane composition lives in `multi_pane.rs`, divider drawing lives
alongside it, and the bottom row is owned by `status_bar.rs`. Two recent
commits made the friction explicit: `34bfc07` collapsed four
near-duplicate paint paths down to `paint_full_frame` +
`paint_focused_pane` (the bespoke geometry was already at the duplication
threshold a year before any overlay shipped); `ed84431` reserved a row
for the status bar after multi-pane output stomped the bar and the
cursor (a regression that only existed because the bar and the panes
were sharing the same renderer's idea of where the cursor was supposed
to live).

What's coming makes the friction worse, not better: a help screen
overlay, a command palette, a session/window picker, eventually a tab
strip — all chrome the substrate now expects the client to grow. Every
one of those, under the current model, is a new hand-rolled cell
budget, new cursor handoff code, and a new place where SGR state can
leak. The cost of "one more piece of chrome" is linear in chrome
features, and we're about to grow chrome features.

This is a **client-internal** decision. The wire is untouched, byte
content is untouched, [ADR-0013](./0013-libghostty-bytes-on-wire.md)
stands; the TUI's design space stays the TUI's per
[ADR-0017](./0017-tui-not-protocol-privileged.md). What changes is how
phux-client composes its own frames.

## Decision

phux-client adopts a **hybrid layered render**. Two renderers, disjoint
regions, layered (not interleaved):

- **Chrome layer — ratatui.** The status bar, dividers between panes,
  pane borders/focus indicators, and all overlays (help screen,
  command palette, session picker, future modals) are drawn by
  ratatui widgets composed against ratatui's `Layout`. ratatui's
  `Buffer` and `crossterm` backend produce the chrome's bytes.

- **Pane interiors — libghostty.** Each pane's cells continue to come
  from a client-side `libghostty_vt::Terminal` mirror fed by server
  `PANE_OUTPUT` bytes (the [ADR-0013](./0013-libghostty-bytes-on-wire.md)
  path). Pane interiors are emitted as VT bytes positioned directly to
  stdout, exactly as `paint.rs` does today; the `RenderState` per-row
  dirty tracking that powers `paint_focused_pane` stays intact.

- **Boundary — ratatui `Cell::skip` carve-outs over pane rectangles.**
  ratatui draws the chrome around the panes; pane rectangles in
  ratatui's `Buffer` are marked `skip = true` so ratatui emits no
  bytes for those cells. Pane interiors are layered into those holes
  by the libghostty side after ratatui flushes the chrome.

The invariants that hold the two renderers together:

1. **Dependency boundary.** `ratatui` and `crossterm` live only in the
   `phux-client` crate (imports confined to its `src/render/`). The
   pane-interior substrate — the pane mirror, predictive echo, layout
   math, and multi-pane composition — lives in `phux-client-core`, which
   has no `ratatui` dependency, so a stray `use ratatui` there fails to
   compile. Originally enforced by `scripts/check-ratatui-boundary.sh`
   (added by `phux-5ke.1`); phux-0fv replaced the grep with the crate
   split so the compiler enforces it. The attach loop stays in
   `phux-client` because it composites chrome over panes and so depends
   on both the chrome and the substrate.

2. **Region disjointness.** In any rendered frame, the set of cells
   ratatui writes to and the set of cells libghostty pane renderers
   write to are disjoint. Chrome over pane interior is **not**
   allowed in the steady state; overlays are a special case handled
   by invariant 5.

3. **SGR reset at every transition.** Before chrome emits its first
   byte and before pane bytes resume after chrome finishes, an
   `\e[0m` reset is emitted. Neither renderer assumes the other's
   active style.

4. **Cursor ownership.** Exactly one renderer positions the cursor
   per frame. The chrome layer parks the cursor at a known sink
   position at end-of-frame; the pane layer (after `paint_focused_pane`
   completes) positions the cursor at the focused pane's logical
   cursor. There is no "shared" cursor state.

5. **Overlay-active state.** While an overlay is up (help screen
   modal, command palette, etc.), pane-stdout flushing pauses —
   the client does not emit VT bytes for pane interiors. Server
   `PANE_OUTPUT` byte consumption into the client-side `Terminal`s
   continues normally; the mirror stays current. On overlay dismiss,
   the client triggers a full repaint (`paint_full_frame`-equivalent
   over the freshly-uncovered region) and pane stdout flushing
   resumes.

## Consequences

### Positive

- **libghostty feature parity stays automatic on the pane hot path.**
  Kitty graphics, sixel, OSC 8 hyperlinks, modern key protocol, and
  whatever Ghostty merges next continue to pass through cell-for-cell
  via `vt_write` on the client `Terminal`. None of it touches ratatui.

- **`RenderState` per-row dirty tracking survives.** The local-side
  incremental redraw that [ADR-0013](./0013-libghostty-bytes-on-wire.md)
  buys us continues to power pane painting; only the chrome region
  is touched by ratatui, and chrome dirty regions are ratatui's
  own problem.

- **Chrome velocity goes up.** A new overlay is a ratatui widget plus
  a region carve-out — minutes, not days. The command palette,
  session picker, and help screen all become composable widgets in
  the same module, not three separate cell-positioning rewrites.

- **The wire does not move.** ADR-0013 bytes-on-the-wire is preserved;
  ADR-0017's "TUI is one consumer among several" stays unbroken; an
  agent SDK consumer or future native GUI sees no change at all.
  Nothing about layered render leaks across the substrate seam.

- **Layout math collapses.** ratatui's `Layout::split` replaces the
  divider arithmetic in `multi_pane.rs`. Pane rectangles are computed
  once, fed both to the chrome layer (as carve-out regions) and to
  the libghostty pane painters (as their target rects).

- **The boundary is mechanically extractable.** If a future headless
  client (recorder, smoke-test driver) or a GUI client ever ships,
  pulling them out is a matter of replacing `render/` with a different
  chrome implementation. Everything outside `render/` already doesn't
  know ratatui exists.

### Negative

- **Two renderers in one process.** Chrome and pane interiors are
  drawn by two different paths emitting to the same stdout. The
  cursor / SGR handoff invariants in the Decision section are real
  invariants — get them wrong and the status bar inherits the
  focused pane's bold red, or the cursor blinks under the divider.
  `ed84431` is the cautionary precedent.

- **ratatui dep weight.** ratatui itself is ~30k LOC; it pulls
  crossterm. Compile time grows; binary size grows. Acceptable: the
  alternative is hand-rolling the equivalent layout, widget, and
  buffer-diff code, and ratatui has years of polish over what we
  would write.

- **Overlay-active state machine.** Invariant 5 introduces a small
  state machine in the attach loop ("are we currently flushing pane
  bytes to stdout?") that the single-renderer model didn't need.
  Mitigated by the consume-but-don't-flush split: byte ingestion
  into the local `Terminal` mirror is decoupled from byte emission
  to stdout, which we already wanted for predictive echo anyway.

- **Two cursor authorities to keep coordinated.** Cursor blinking,
  shape, and visibility are libghostty-pane state; chrome may want
  to hide the cursor outright (modal overlays). Reconciling on
  every frame is mechanical but a new responsibility.

- **A subtle perf cost: the chrome layer's `Buffer::diff` runs
  every frame** even if only one pane scrolled. The cost is bounded
  by the chrome cell budget (typically one status row + dividers <<
  total cells), so well under the dominant pane-redraw cost — but
  it's not free.

## Rejected alternatives

### 1. ratatui-ghostty for pane interiors (lossy widget bridge)

`ratatui-ghostty` exists; it bridges a parsed VT buffer into a
ratatui widget. Using it for pane interiors would unify the renderer
to one layer and remove the boundary invariants entirely. We
rejected it.

The cost is fidelity. The bridge collapses libghostty's cell model
into ratatui `Cell`s — and ratatui `Cell` has no place for Kitty
graphics, sixel, OSC 8 hyperlinks, or the modern key protocol's
keyboard flags. Those features are exactly the ones
[ADR-0013](./0013-libghostty-bytes-on-wire.md) preserved on the wire
so they would automatically appear on the client. Routing pane
content through ratatui-ghostty re-introduces the same translation
treadmill ADR-0013 walked away from, in the renderer instead of the
wire. The bridge is the **right** tool for pane *thumbnails* — a
session picker showing 8×24 previews of every pane — where fidelity
genuinely doesn't matter and what we want is a ratatui-composable
mini-grid. We will use it there. Not on the hot path.

### 2. rmux-style structured cell snapshots on the wire

Some multiplexers ship structured cell snapshots from server to
client and let each client compose its own renderer freely on top.
We rejected this in [ADR-0013](./0013-libghostty-bytes-on-wire.md)
and re-rejecting it here is mechanical: it directly contradicts the
bytes-on-the-wire decision and would re-introduce the
protocol-evolution treadmill where every libghostty feature
(graphics, sixel, hyperlinks, key flags) needs a structural
representation in `phux-protocol` before it can reach a client. The
cost-model argument from ADR-0013 §"The cost-model correction"
applies unchanged. Out of scope for this ADR; preserved here as a
back-pointer for future readers wondering why phux didn't take the
"obvious" path.

### 3. All-ratatui with libghostty as a buffer source

A milder version of (1): keep libghostty parsing VT into a grid
server-side and client-side, but have ratatui pull cells out of the
client `Terminal` via `grid_ref()` and draw the entire frame
through ratatui. One renderer, one cursor authority, no boundary
invariants.

Rejected because the lossy bridge is still the cost — the moment a
cell carrying a Kitty graphics blit or a sixel attachment passes
into ratatui's `Cell`, the blit is gone. The fidelity loss is the
same as alternative 1, just relocated. Worse, we'd also forfeit
`RenderState::update`'s per-row dirty tracking, because ratatui's
`Buffer::diff` is a different incremental model that doesn't know
which libghostty rows are clean. The result would be a slower,
lossier render than the status quo.

### 4. Status quo — hand-rolled cell positioning forever

Keep `paint.rs`, `multi_pane.rs`, `status_bar.rs` as the model.
Build overlays the same way: bespoke cursor math, bespoke SGR
discipline, bespoke region tracking. We rejected this because it
doesn't address the motivating problem. Overlay velocity is the
chrome we want to build over the next two milestones; the status
quo makes each new piece of chrome a fresh exercise in the same
geometry primitives. `34bfc07` already collapsed the four paint
paths once; the next overlay would re-introduce a fifth, and so
on. The cost of bespoke chrome math grows linearly in chrome
features, exactly when we want it to grow sub-linearly.

## Boundary enforcement

The boundary is a crate split (phux-0fv). The pane-interior
substrate — pane mirror, predictive echo, layout math, and
multi-pane composition — lives in `phux-client-core`, which declares
no `ratatui` dependency. A `use ratatui` there cannot resolve, so it
fails to compile; `cargo build`/`lint` enforce the boundary. This
replaced the original `scripts/check-ratatui-boundary.sh` grep guard
(introduced by `phux-5ke.1`). The attach loop and server-frame
handler stay in `phux-client` because they composite chrome over
panes; the pure substrate they delegate to is the portable part.

The reason this matters is not aesthetic. That substrate needs to
remain portable to renderers that aren't ratatui — a future GUI
consumer, a headless smoke-test client, or an inspection tool. If
ratatui types leaked into it, the extraction would stop being
mechanical and become a rewrite. The crate boundary keeps the
extraction price flat as the client grows.

## Implementation epic

Tracked under `bd` epic [`phux-5ke`](../../README.md) — phux-client
layered render. Children:

- **`phux-5ke.1`** — `render/` module scaffolding, workspace
  `ratatui` dependency gated to `phux-client::render`, CI grep
  guard, ARCHITECTURE.md note.
- **`phux-5ke.2`** — Migrate `attach/status_bar.rs` to a ratatui
  widget under `render/chrome/`. First end-to-end exercise of the
  boundary; replaces a self-contained module.
- **`phux-5ke.3`** — Dividers and pane chrome to ratatui;
  introduces the `Cell::skip` carve-out over pane rects. Touches
  `multi_pane.rs` and the paint plumbing; coordinates with any
  in-flight `driver.rs` refactor.
- **`phux-5ke.4`** — Overlay framework under `render/overlay/`;
  first overlay is the help screen. Lights up invariant 5 (the
  overlay-active state machine).
- **`phux-5ke.5`** — This ADR.

## References

- [ADR-0013](./0013-libghostty-bytes-on-wire.md) — libghostty bytes
  on the wire. Preserved unchanged; this ADR is downstream of it
  and changes nothing it owns.
- [ADR-0017](./0017-tui-not-protocol-privileged.md) — the reference
  TUI is one consumer among several with no protocol privileges.
  Honored: chrome composition is a TUI-internal concern; no
  vocabulary leaks into the wire.
- [ADR-0019](./0019-tui-multi-pane-rendering.md) — multi-pane TUI
  rendering. This ADR makes explicit the chrome/pane-interior split
  that ADR-0019 implicitly relied on (its "borders between panes"
  decision presumed a renderer for the borders; this ADR names
  that renderer ratatui).
- `crates/phux-client/src/attach/paint.rs` — current
  `paint_full_frame` / `paint_focused_pane`. The pane-interior side
  of the boundary stays here; the chrome side migrates to
  `crates/phux-client/src/render/` over `phux-5ke.2..4`.
- `crates/phux-client-core/src/multi_pane.rs` — divider compute
  (`PaneLayout`, `DividerCell`); moved to `phux-client-core` under
  phux-0fv. The chrome side rasterizes its cells in
  `crates/phux-client/src/render/chrome/dividers.rs`.
- `crates/phux-client/src/attach/status_bar.rs` — original status
  bar; migrated to `render/chrome/status_bar.rs` under `phux-5ke.2`.
- Commits `34bfc07` (paint-path collapse) and `ed84431` (status-bar
  row reservation fix) — the precedents that motivated this ADR.
- The boundary was originally a `scripts/check-ratatui-boundary.sh`
  CI guard (added by `phux-5ke.1`); phux-0fv replaced it with the
  `phux-client` / `phux-client-core` crate split.
