---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0012 — Window layout is a binary split tree, not n-ary

**TL;DR.** The reference TUI's layout is a binary split tree: every interior node is `Split { dir, ratio, left, right }` with exactly two children and `ratio: f32` in the open interval `(0.0, 1.0)`. Tree operations and wire encoding stay mechanical. A future `TABBED` variant is reserved but deliberately absent. Under ADR-0015 this is a TUI convention, not a wire concept.

Status: Accepted
Date: 2026-05-25
Superseded in part by [ADR-0015](./0015-protocol-layering.md): the split tree is the reference TUI's layout schema under an L3 metadata key, not a wire concept.
See [ADR-0016](./0016-terminal-id-as-wire-primary.md) for the rename: Leaf(PaneId) below is Leaf(TerminalId).

## Context

A multiplexer must commit to a layout model before it ships its first
session, because the model leaks: into the wire protocol (snapshots
have to round-trip), into persistent state (a session reattached after a
restart had better come back recognisable), and into every operation
that touches panes (split, kill, resize, directional focus). Picking
this wrong now is not a "small refactor later" kind of mistake.

There are basically three shapes in the wild:

- **N-ary split tree.** Tmux uses this: an interior node has a direction
  and `N ≥ 2` children. Splits append children to the parent rather
  than nesting.
- **Binary split tree.** Each interior node has exactly two children,
  a direction, and a single split ratio. Three-way splits are
  represented as nested binary splits.
- **Flat list of rectangles** with absolute coordinates (i3-style).

The decision was made in `phux-byc.2` and shipped in
`crates/phux-core/src/window.rs` as:

```rust
pub enum LayoutNode {
    Leaf(PaneId),
    Split {
        dir: SplitDir,
        ratio: f32,
        left: Box<LayoutNode>,
        right: Box<LayoutNode>,
    },
}
pub enum SplitDir { Horizontal, Vertical }
```

The wire-side mirror in `crates/phux-protocol/src/wire/info.rs` has the
same shape; the *why-is-it-duplicated* question is answered by
ADR-0011 (`phux-protocol` and `phux-core` are independent crates by
design). This ADR is about the shape itself.

## Decision

**phux layouts are a binary split tree.** Every interior node is
`Split { dir, ratio, left, right }` with exactly two children. A
third variant `TABBED` is reserved on the wire (SPEC §10.3) for a
future version but is *intentionally* not present in
`phux_core::LayoutNode` today — the module doc spells this out so the
omission doesn't look accidental.

`ratio: f32` lives in the half-open range `(0.0, 1.0)`. Constructors
validate this (`LayoutError::InvalidRatio` for NaN or out-of-range);
the wire decoder validates it (`DecodeError::MalformedLayoutRatio` for
non-finite or out-of-`[0.0, 1.0]` values). The wire is slightly more
permissive than the in-memory constructor (boundaries included on the
wire, excluded in-process) because the wire has to accept everything a
peer might legitimately send and *then* normalize — but the canonical
in-process invariant is the strict open interval.

## Rationale

### Why binary, not n-ary

- **Recursion is mechanical.** Every operation on the tree —
  `split_at`, `kill_pane` / `collapse`, `pane_rects` / `fill_rects`,
  `focus_direction` — is a function of `(LayoutNode, LayoutNode)`
  rather than `(LayoutNode, Vec<LayoutNode>)`. Pattern-match on
  `Split { left, right, .. }`, recurse, done. There's no "what does
  this operation mean across N siblings" gap to design around.
- **Wire encoding is fixed-fanout.** `encode_layout_node` writes a tag
  byte, the dir, the ratio, then recurses into exactly two subtrees
  (`crates/phux-protocol/src/wire/info.rs` `encode_layout_node` /
  `decode_layout_node`). Proptest round-trip is trivially structural.
  An n-ary encoding would need a length-prefixed list and a
  fan-out-N invariant the decoder has to police.
- **The user-facing model is unchanged.** A user can still split the
  same direction N times; the result is nested binary splits whose
  rectangle tiling is identical to what an n-ary tree would produce.
  The user-visible difference is zero. Resize and focus operations
  reflect intent regardless of whether the tree is nested or wide,
  provided those operations are written correctly (and they are; see
  `focus_direction` in `window.rs`).
- **tmux's n-ary tree is implementation history, not principled
  design.** Early tmux had a pure binary tree; n-ary fan-out was
  grown into `layout-custom`/`layout_*.c` over years to support
  imported layout strings and certain resize gestures. We do not
  inherit that history and have no reason to recreate it.

### Why `ratio: f32`, not absolute cells or pixels

- **Resize-on-viewport-change becomes proportional.** When the
  client's viewport changes (or the server-aggregated viewport
  changes across multi-attach clients), `pane_rects` re-flows the
  whole subtree under the new bounds. No "lost cells" or stranded
  empty rows.
- **Persistence across viewports works.** A session attached from
  80x24, detached, then reattached from 200x60, comes back with a
  layout that still makes sense. Absolute coordinates would either
  truncate or stretch ugly.
- **The cost is f32 weirdness.** NaN, infinity, signed zero, and
  precision drift on repeated edits. We mitigate with explicit
  validation at every boundary that constructs or accepts a ratio
  (`validate_ratio` in `phux-core`, `MalformedLayoutRatio` at the
  wire). Within-process the invariant is upheld by constructors —
  there is no public field write that bypasses `Window::split`.

### Why no `TABBED` yet

The user-facing UX for "tabs vs splits" is genuinely undecided. The
shape we'd most likely want — a window has a list of top-level
"tabs," each holding its own binary split tree — is a layer *above*
`LayoutNode`, not a third sibling variant. Other shapes (tabs as a
LayoutNode variant whose children are sibling subtrees) are still on
the table.

Either way, we don't lose by deferring. The wire reserves a third
tag byte (SPEC §10.3 names the `TABBED` variant; the protocol's
tag-byte enum is non-exhaustive). When we land tabs, the wire grows
one tag and the in-memory enum grows one variant. No layout
re-versioning, no migration of stored sessions.

## Tradeoffs

- **Deep nesting if a user splits the same pane many times.** A
  six-way split is a six-deep tree, not a flat six-child node.
  Operationally this matters for two things: `focus_direction`
  traversal depth (proportional to tree depth, still O(panes) in
  the worst case), and snapshot encoding size (one extra tag byte
  per nesting level). Tmux has the same property at the actual
  rectangle level — its n-ary tree saves a byte per sibling but
  loses it back in every other invariant. Net: a wash.
- **The wire accepts more than the in-memory model.** Specifically,
  `ratio = 0.0` and `ratio = 1.0` round-trip on the wire but cannot
  be constructed in `phux-core`. This is intentional: peer
  decoders must be permissive at the framing layer, and a
  degenerate-but-valid ratio is better turned into a clean error at
  a higher layer (or normalized) than rejected at decode time. The
  asymmetry is documented at both sites.
- **f32 is not Hash/Eq.** We use `PartialEq` only and never key by
  `LayoutNode`. If we ever need a stable hash of a layout (cache
  key, dedupe), we'll quantize the ratio to a fixed-point u16
  scratch representation at the hashing boundary. Out of scope
  for v0.1.

## Alternatives considered

- **N-ary split tree (tmux's model).** Rejected per above: more
  code, more invariants, no user-visible benefit. The only
  meaningful argument *for* n-ary is parity with tmux's
  `layout-custom` string format — and we are not committing to
  consuming tmux layout strings as input.
- **Flat list of rectangles with absolute coordinates (i3-style).**
  Rejected: doesn't survive resize cleanly; loses user intent
  ("split *this* pane" becomes "draw a rectangle here"); doesn't
  serialize compactly. Workable for tiling window managers where
  the user is the layout algorithm; bad for a multiplexer where
  the layout is derived from semantic operations.
- **Constraint-based layout (CSS Grid, GTK Box).** Rejected:
  overkill for terminal cells, no user-visible UX win, and a much
  larger surface area to specify on the wire. The "container with
  flexible children" model is a poor fit for "the user split this
  pane in half then again in thirds."
- **Binary tree with `weight: u16` instead of `ratio: f32`.** This
  is what SPEC §10.3's prose currently says (LEAF carries a
  `weight: u16`). Rejected for the in-memory model because integer
  weights don't compose under nested splits without re-normalizing,
  and re-normalizing integer weights to preserve a target rectangle
  is more arithmetic than just storing the ratio. The SPEC prose
  predates the byc.2 implementation; reconciling it is tracked
  separately and does not block this ADR.

## Consequences

- Positive: simple recursive algorithms; structural round-trip on
  the wire (proptest covers); persists across resize without
  bespoke fixup logic.
- Positive: adding `TABBED` later is purely additive — one new
  variant, one new tag byte, no migration.
- Negative: a user who splits the same direction many times in a
  row gets a deep tree. Acceptable; matches what tmux does at the
  rectangle level.
- Negative: SPEC.md §10.3 prose ("LEAF { pane_id, weight: u16 }")
  no longer matches the implementation. Reconciling SPEC is
  out of scope for this ADR; tracked as documentation drift.

## Related

- `crates/phux-core/src/window.rs` — `LayoutNode`, `SplitDir`,
  `Window::split`, `Window::kill_pane`, `Window::focus_direction`,
  `Window::pane_rects`, `validate_ratio`.
- `crates/phux-protocol/src/wire/info.rs` — wire-side mirror of
  `LayoutNode`/`SplitDir`, `encode_layout_node`/`decode_layout_node`,
  `MalformedLayoutRatio` enforcement.
- ADR-0011 — `phux-protocol`/`phux-core` independence (the reason
  `LayoutNode` exists in both crates).
- SPEC §10.3 — wire-level layout types, including the reserved
  `TABBED` variant.
- ADR-0013 — libghostty bytes on the wire (supersedes ADR-0002).
  Layout snapshots flow through the same `ATTACHED` path as
  `PANE_SNAPSHOT` byte frames. The lifecycle/layout shape on the wire
  is unchanged by ADR-0013 — only the pane-content payload moved from
  structured cell-diffs to VT replay bytes.
- `bd` ticket `phux-byc.2` — implementation that shipped the
  binary-tree model.
