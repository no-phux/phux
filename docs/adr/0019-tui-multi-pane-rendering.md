---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0019 — Multi-pane TUI rendering: layout persistence, wire shape, and chrome

**TL;DR.** The reference TUI's layout tree persists as a CBOR L3 metadata blob keyed `phux.tui.layout/v1`, scoped to a Collection. Layout edits are computed client-side and pushed via `SET_METADATA`; other clients subscribe to `METADATA_CHANGED` and re-render. Borders are plain Unicode box-drawing between panes, resize is proportional, and focus is per-client and not persisted. No wire substrate change.

Status: Accepted
Date: 2026-05-27

## Context

The reference TUI presently renders a single Terminal per attached
client: `crates/phux-client/src/attach/driver.rs` holds one
`libghostty_vt::Terminal` mirror, one `TerminalRenderer`, and routes all
stdin to one `TerminalId`. The user-visible result is "tmux without the
multi-pane part." The substrate is ready for more: `phux-byc.2` shipped
the binary split tree (`phux-core::window::Window` with `split_h/v`,
`kill_pane`, `focus_direction`, `pane_rects`); `phux-nz4.3` shipped the
keybind resolver (`KeyChord` → `ResolvedAction`); and the `phux-q0e`
state-sync epic shipped per-consumer `RenderState` lifecycle so the
server is already prepared to drive N renders for N Terminals.

What remains is the rendering surface itself: tile multiple Terminals
into one outer viewport, draw borders, route input to focused, wire
actions through to layout ops, and reflow on SIGWINCH. This ADR closes
the design questions blocking that epic (`phux-4li`) so the work can
fan out into parallelizable child tickets.

The five questions this ADR answers are flagged in the epic body. None
of them touches the wire substrate per [ADR-0017](./0017-tui-not-protocol-privileged.md) —
they are entirely reference-TUI conventions. But they need a single
authoritative decision so child agents don't fork on interpretation.

## Decision

### 1. Layout persistence: L3 metadata, server-authoritative, client-fallback

The TUI's layout tree is persisted **server-side as an L3 metadata blob
keyed `phux.tui.layout/v1`, scoped to the Collection** the session
maps to (per [ADR-0015](./0015-protocol-layering.md) and DESIGN.md
§"How the TUI's user model maps to the substrate"). The blob is the
serialized binary split tree (same shape as `phux-core::LayoutNode`),
plus a per-window focus pointer.

Concrete read/write path:

- **Wire**: piggybacks on the L3 metadata frames already reserved in
  SPEC §7.4 (`GET_METADATA`, `SET_METADATA`, `SUBSCRIBE_METADATA`,
  `METADATA_CHANGED`). No new frame kinds. The discriminant bytes are
  TBD per SPEC §7.4; **this ADR does not allocate them.** The
  child ticket that lights up L3 (`phux-4li.2`) allocates discriminants
  in the same commit that ships the encode/decode and the server-side
  K/V store.
- **Encoding**: CBOR per SPEC §7.4 recommendation. The CBOR schema is a
  newtype envelope `{version: 1, root: LayoutNode, focus: TerminalId}`
  whose `LayoutNode` mirrors `phux-protocol::wire::info::LayoutNode`
  byte-for-byte semantics (binary split, `ratio: f32` in `(0, 1)`,
  `dir: SplitDir`, leaf carries a `TerminalId`). The wire-encoder helper
  for `LayoutNode` is reused; only the envelope is CBOR.
- **Authority**: server is authoritative *when the key is present*. On
  attach, the client `GET_METADATA(scope=Collection, key="phux.tui.layout/v1")`.
  If present, the client reconstructs the tree and renders multi-pane;
  if absent (fresh Collection or single-pane consumers), the client
  falls back to "one Terminal, no borders" exactly as today.
- **Coordination across clients**: the client subscribes to
  `METADATA_CHANGED` for the key and re-reads on notification. Two
  TUIs splitting concurrently is a last-write-wins race, mitigated by
  the versioned key and (later) optimistic compare-and-set in a v2
  schema. Same trade as the metadata-blob coordination story in
  ADR-0017.

**Rejected: client-only per-attach layout (option (b) in the epic).**
Pair-programming and "two windows on the same session see the same
tile arrangement" are real product features even if uncommon; metadata
costs us nothing once L3 lands. Rejecting also rejects "layout is lost
on detach with no client memory," which is a worse default than
"layout is preserved by the server but can be discarded by the client."

**Rejected: both with client overriding server.** Conflict resolution
becomes a TUI-private problem the consumer has to specify. Single
authority (server) with optional client override (the client may
`DELETE_METADATA` to reset) is simpler and reversible.

### 2. Layout-change wire shape: client computes, sends `SET_METADATA`

When the user hits `prefix + |` (split vertical):

1. The client resolves the chord to action `split-pane direction=vertical`.
2. The client issues a `SPAWN { collection_id }` to obtain a new
   `TerminalId` for the new pane (L1 substrate; same path the existing
   single-pane code already uses).
3. The client computes the new layout tree locally using the same
   `LayoutNode` shape as `phux-core::Window::split` (cloned into the
   client-side mirror — see decision 3).
4. The client issues `SET_METADATA { scope: Collection, key:
   "phux.tui.layout/v1", value: <cbor envelope> }`.
5. The server stores, broadcasts `METADATA_CHANGED` to subscribers,
   and other attached clients re-`GET_METADATA` + re-render.

This is option (a) from the epic. Selected because it is the option
most consistent with ADR-0017: the server **never executes layout
logic**. The server stores a blob it does not interpret; layout is a
TUI thing. Other consumers (a future GUI; an agent) never see this
blob and never see layout events. They see `SPAWN` and
`TERMINAL_CLOSED` on the substrate layer; that's all the substrate
owes them.

**Rejected: option (b), server-side `TUI_ACTION` frames.** Would
re-introduce TUI vocabulary into the substrate — exactly the failure
mode ADR-0017 forbids. Killed.

**Rejected: option (c), pure client-side / never shared.** Same
reasoning as decision 1: shared layout across attached clients is a
load-bearing TUI feature and metadata gives it to us free.

### 3. Client-side layout mirror

`crates/phux-client/src/layout.rs` (new module) holds a client-side
mirror of the binary split tree. **Shape**: the same `LayoutNode`
enum the server uses (cloned, not re-derived), plus a focus
`TerminalId` and a per-pane `Rect` cache recomputed on resize.

We do **not** depend on `phux-core::Window` from the client. The
client crate already avoids depending on `phux-core` for ADR-0011
reasons (protocol/core independence). The layout-tree shape lives in
`phux-protocol::wire::info::LayoutNode` (per ADR-0012's note on
duplication); the client imports that type, not the core version.
Operations (`split`, `kill_pane`, `focus_direction`, `pane_rects`)
are re-implemented client-side as free functions over the wire type.
Yes, that's a third copy of the layout-op algorithms. Yes, it's worth
it for the crate-boundary discipline. The algorithms are ~100 lines
each and have proptest coverage server-side; the client copy can
mirror those tests.

### 4. Border drawing and focus chrome

- **Style**: plain Unicode box-drawing characters (U+2500..U+257F).
  No configuration in v0.1. The cell budget is one row + one column
  per interior split, drawn **between** panes (not as a frame around
  every pane). A two-pane vertical split in an 80×24 viewport gives
  pane A 39 columns, the divider 1 column, pane B 40 columns. Math:
  `floor((cols - dividers) * ratio)`.
- **Focus indicator**: the divider edges adjacent to the focused pane
  use heavy box-drawing (U+2503 / U+2501); inactive dividers use light
  (U+2502 / U+2500). Also, the focused pane's top-left corner cell
  contains an indexed numeric tag (`▏0▕`-style) — out of scope for
  v0.1; defer to a follow-up ticket.
- **Cell-budget rule**: borders are accounted for **before** `pane_rects`
  runs. The TUI's outer viewport is `(cols, rows)`; the layout-tree
  rectangle computation is given `(cols - h_dividers, rows - v_dividers)`
  where the divider counts are derived from the tree topology. Pane
  rectangles never overlap dividers; the renderer draws dividers in
  the cells the tree explicitly excluded.

**Rejected: borders around every pane (frame-style).** Doubles the
cell cost per split. tmux uses "dividers between," and that's the
shape users expect.

**Rejected: configurable border style in v0.1.** Reversible — adds a
`[ui.borders]` config block in v0.2 when there's demand. Don't ship
config that hasn't been asked for.

### 5. Resize behavior: proportional, ratio-preserving

Match tmux. When the outer viewport resizes (SIGWINCH or
`VIEWPORT_RESIZE`):

1. Recompute divider cell counts from the tree.
2. Pass `(cols - h_dividers, rows - v_dividers)` to `pane_rects`.
3. Each leaf gets a new `Rect`; the per-pane `RESIZE { terminal_id,
   cols, rows }` is emitted **only for panes whose dimensions changed**.

Split ratios are preserved across resize; no leaf "freezes" at minimum
size in v0.1 — if the viewport shrinks below the layout's minimum
viable size, the layout renders garbage in the affected cells and the
TUI logs a warning. The min-size freezing described in DESIGN.md §6.2
is deferred to a future ticket; not load-bearing for the daily-drive
arc.

Manual resize (`prefix + Ctrl-arrow` or configured key →
`resize-pane direction=right amount=5`) modifies the relevant interior
node's `ratio` and re-runs `pane_rects`, then `SET_METADATA`. The
`amount` is "cells of boundary movement"; conversion to a ratio
delta is `amount / total_cells_along_axis`. **No floor on amount in
v0.1**; if the ratio would leave a child below 2 columns the action
is a no-op and beeps. Defer the "amount in proportional units"
debate.

### 6. Active pane / focus persistence

**Focus is per-client, stored in client-local memory only.** Not
shared via metadata; not persisted across detach. On re-attach with
no client-side memory, focus defaults to the first leaf in
left-to-right depth-first traversal order. This is reversible — if
"my focus follows me across reattach" demand surfaces, add a
`phux.tui.focus/v1` per-client key (scoped Global with a client-id
discriminator) in v0.2.

Multi-client focus convergence is **explicitly not a goal**. Two
clients attached to the same session may have different focused
panes; that's tmux's behavior too. Input from each client routes to
that client's focused pane, server-side, via the existing
`INPUT_KEY.terminal_id` field (which the client populates from its
local focus state).

**Rejected: focus-in-metadata (shared focus).** Pair-programming with
shared focus is a different product (a la `tmate` view-only mirror),
not what users want by default. If we want it we add it; defaulting
to it surprises everyone.

**Rejected: server-tracked focus.** Would require a `FOCUS_CHANGED`
wire concept — exactly what ADR-0015 demoted out of the substrate.
Killed.

## Consequences

### Positive

- **No wire change required by this ADR.** L3 metadata frames are
  already reserved in SPEC §7.4; lighting them up is a v0.2 milestone
  that the `phux-4li.2` ticket pulls forward. The wire envelope, the
  layout-tree binary encoding, and the focus model all land without
  touching L1.
- **Substrate stays substrate.** No layout vocabulary leaks into the
  wire (per ADR-0017). An agent or recorder consumer sees `SPAWN` and
  `TERMINAL_CLOSED`; that's it.
- **Reversible defaults.** Border style, focus persistence, resize
  flooring — every one of these is a config knob we can add when
  someone asks. None is locked in.
- **The hard part (layout-tree algorithms) is already shipped.**
  `phux-byc.2`'s `Window` is the reference; the client's mirror
  reimplements the same shape against the wire type. Proptest
  coverage transfers.

### Negative

- **Three copies of the layout algorithms.** `phux-core::Window`
  (server domain), `phux-protocol::wire::info::LayoutNode` (wire
  encode/decode), and the new client-side mirror. Each is small (~100
  LOC) and ADR-0011 makes the boundary necessary. Acceptable; not a
  growing surface.
- **The L3 metadata wire frames are getting allocated for this
  feature.** SPEC §7.4 leaves their discriminant bytes TBD; `phux-4li.2`
  allocates them. That's a wire-stability commitment we're making
  earlier than the original "v0.2 milestone" plan implied. The wire
  shape is small (key, value, scope) and well-precedented across
  K/V protocols; low risk.
- **Last-write-wins on the layout blob** means two clients splitting
  simultaneously can clobber each other. Mitigated by infrequency in
  practice and by the versioned key. A v2 schema with a CAS token is
  the obvious upgrade if it bites.

### Tradeoffs deliberately accepted

- **No min-size freezing in v0.1.** Tmux does this; we punt. The
  layout renders garbage at extreme shrinkage; users won't notice
  unless they're stress-testing.
- **No configurable border style.** Plain Unicode box-drawing only.
  Reversible.
- **No shared focus.** Each client tracks its own focused pane.
  Matches tmux; explicitly not "shared cursor across clients."

## Alternatives considered

- **Server-side layout service** (option (b) from the epic). Would put
  a `TUI_LAYOUT_OP` frame family in the substrate. Killed: violates
  ADR-0017's substrate/consumer split. The whole point of the
  layering work was to avoid this.

- **Layout in client memory only** (option (c) from the epic). Loses
  multi-client agreement; loses persistence across detach. Defensible
  for v0.1 if L3 metadata frames are too painful to allocate yet;
  rejected because the cost of allocating L3 is small (the frames are
  already designed in SPEC §7.4; we just commit the bytes).

- **Layout in a sibling sidecar file** under `$XDG_STATE_HOME/phux/`.
  Server-side, but not through the wire. Considered briefly; rejected
  because then a remote client can't read it, and "remote client sees
  same session as local" is the v0.2+ federation story we're not
  prepared to break before it ships.

- **A "windows are tabs" layer above panes**, with tabs as a separate
  metadata key. Out of scope for this ADR; v0.1 ships with one window
  per session and a layout tree per window. DESIGN.md §"Window" remains
  the design intent for tabs; this ADR doesn't preclude it.

## Open questions punted forward

These do not block `phux-4li` implementation but should be tracked:

1. **Min-size freezing semantics under aggressive viewport shrink.**
   DESIGN.md §6.2 spec'd it; implementation deferred. File a follow-up
   when the daily-drive arc surfaces complaints.
2. **CAS / optimistic locking on metadata writes.** v0.2 schema bump
   if the last-write-wins race bites. Not a v0.1 problem.
3. **Tabbed layouts** (windows-as-tabs above panes). DESIGN.md §"future
   work" + SPEC §10.3's reserved `TABBED` variant. Out of scope.
4. **Cross-client focus convergence** (pair-programming mode). Add an
   opt-in metadata key in v0.2 if demand surfaces.
5. **`resize-pane amount` semantics in proportional units**, e.g.
   "5% of the axis." Defer until users complain that "5 cells" feels
   inconsistent across viewport sizes.

## References

- [ADR-0012](./0012-binary-split-tree-layout.md) — binary split tree
  shape; this ADR adopts it client-side.
- [ADR-0015](./0015-protocol-layering.md) — three-layer protocol; this
  ADR uses L3 for layout persistence.
- [ADR-0016](./0016-terminal-id-as-wire-primary.md) — `TerminalId` is
  the leaf identity in the layout tree.
- [ADR-0017](./0017-tui-not-protocol-privileged.md) — the TUI is one
  consumer among several; layout is a TUI convention, not a wire
  concept. This ADR honors that line.
- [ADR-0018](./0018-lazy-state-synchronization.md) — per-consumer
  `RenderState` lifecycle, which the multi-pane render path consumes
  (one `RenderState` per (Terminal × attached consumer)).
- SPEC.md §7.4 — L3 metadata frames (reserved; allocated by `phux-4li.2`).
- DESIGN.md §6 (Layout) + §"How the TUI's user model maps to the
  substrate" — user-facing semantics this ADR implements.
- `crates/phux-core/src/window.rs` — server-side layout algorithms
  the client-side mirror clones.
- `crates/phux-protocol/src/wire/info.rs` — wire-side `LayoutNode`
  the CBOR envelope wraps.
- `crates/phux-client/src/attach/driver.rs` — current single-pane
  render path the multi-pane work extends.
- `bd` epic `phux-4li` — multi-pane TUI rendering, decomposed into
  child tickets in the commit that lands this ADR.
