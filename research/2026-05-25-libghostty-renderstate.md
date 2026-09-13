---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-05-28
---

# Libghostty RenderState + dirty tracking — how phux uses it on both ends

**TL;DR.** Capability survey of `libghostty-vt`'s `RenderState` read API
and its two-tier (global + per-row) dirty bits. Describes the contract,
the call sequence, and how the phux renderer side — client for displayed
panes, server for snapshot synthesis on attach — drives one
`RenderState` per pane. Reference for Wave 3 work landing ADR-0013.

**Status**: Research artifact. Forward-looking documentation for the Wave 3
server + client refactor that lands ADR-0013. See ADR-0013 for the decision;
SPEC §8 for the wire shape; this note for *how the renderer side of each
process uses libghostty's read API*.

**Source**: `libghostty-vt` at
`~/.cargo/git/checkouts/libghostty-rs-28fee7453bdb2b25/31d1f70/crates/libghostty-vt/src/render.rs`
(the revision pinned in this workspace's `Cargo.lock` as of 2026-05-25).

## 1. What RenderState is

`RenderState` is a stateful render snapshot wrapping a `Terminal`. You
create it once, then refresh it from a terminal whenever you want to
draw a frame. Two layers of dirty bits — global, and per-row — let a
renderer skip clean frames and skip clean rows within a partial frame.

The library is explicit about the contract:

> The key design principle of this API is that it only needs read/write
> access to the terminal instance during the update call. This allows
> the render state to minimally impact terminal IO performance and also
> allows the renderer to be safely multi-threaded (as long as a lock is
> held during the update call to ensure exclusive access to the
> terminal instance).

In phux, the renderer-side process (client for displayed panes; server
for snapshot synthesis on attach) drives a single `RenderState` per
pane against the local `Terminal` for that pane.

## 2. The API surface

The structs and the methods we will actually call. Lifetimes elided
for prose; see `render.rs` for the full annotated signatures.

```rust
// Construction — one per pane on the renderer side.
let mut render_state = RenderState::new()?;
let mut row_iter     = RowIterator::new()?;
let mut cell_iter    = CellIterator::new()?;

// Per-frame refresh.
let snapshot = render_state.update(&terminal)?;     // Result<Snapshot>
```

Read off the snapshot:

```rust
snapshot.dirty()      -> Result<Dirty>                // Clean | Partial | Full
snapshot.cols()       -> Result<u16>
snapshot.rows()       -> Result<u16>
snapshot.colors()     -> Result<Colors>               // bg, fg, cursor?, palette[256]
snapshot.cursor_visible()       -> Result<bool>
snapshot.cursor_blinking()      -> Result<bool>
snapshot.cursor_password_input()-> Result<bool>
snapshot.cursor_visual_style()  -> Result<CursorVisualStyle>  // Bar | Block | Underline | BlockHollow
snapshot.cursor_viewport()      -> Result<Option<CursorViewport>>  // { x, y, at_wide_tail }
snapshot.cursor_color()         -> Result<Option<RgbColor>>
```

Iterate rows and cells:

```rust
let mut row_iteration = row_iter.update(&snapshot)?;
while let Some(row) = row_iteration.next() {
    let dirty = row.dirty()?;
    let _raw  = row.raw_row()?;     // libghostty's `screen::Row` (rarely needed in phux)

    let mut cell_iteration = cell_iter.update(row)?;
    while let Some(cell) = cell_iteration.next() {
        let style       = cell.style()?;             // libghostty::style::Style
        let fg          = cell.fg_color()?;          // Option<RgbColor>, palette resolved
        let bg          = cell.bg_color()?;          // Option<RgbColor>, flattened
        let graphemes   = cell.graphemes()?;         // Vec<char>; empty == no text
        let _raw        = cell.raw_cell()?;          // libghostty's `screen::Cell`
    }
}
```

Dirty mutators (caller is responsible for clearing both layers):

```rust
snapshot.set_dirty(Dirty::Clean)?;   // reset global
row.set_dirty(false)?;               // reset this row
```

`Dirty` is `Clean | Partial | Full`. `Partial` means "some rows changed
— consult per-row bits"; `Full` means "redraw everything"; `Clean`
means "nothing to draw".

## 3. The two-layer dirty contract

The library is explicit: *setting one dirty state doesn't unset the
other.* From `render.rs`:

> An extremely important detail: setting one dirty state doesn't unset
> the other. For example, setting the global dirty state to false does
> not reset the row-level dirty flags. So, the caller of the render
> state API must be careful to manage both layers of dirty state
> correctly.

The pattern we will use on both ends, after rendering:

```rust
match snapshot.dirty()? {
    Dirty::Clean   => { /* nothing drawn; no resets needed */ }
    Dirty::Partial => {
        // Iterate rows, redraw any with row.dirty() == true,
        // call row.set_dirty(false) after each.
        snapshot.set_dirty(Dirty::Clean)?;
    }
    Dirty::Full => {
        // Iterate every row regardless of per-row bit; redraw all.
        // Still walk rows to clear per-row bits.
        snapshot.set_dirty(Dirty::Clean)?;
    }
}
```

Forgetting to clear per-row bits after a `Dirty::Full` frame leaves
the next `Dirty::Partial` frame thinking every row is dirty.

## 4. How phux uses it on the SERVER

The server runs a libghostty `Terminal` per pane. PTY bytes arrive,
the supervisor calls `terminal.vt_write(bytes)`, and the canonical
state advances. The renderer-side use of `RenderState` on the server
is narrow:

- **Snapshot synthesis on attach.** When a client attaches and needs
  a `PANE_SNAPSHOT`, the server refreshes its `RenderState`, iterates
  rows and cells, and emits a VT byte sequence that — when written
  into a fresh `Terminal` on the receiving client — reproduces the
  current grid. See §7 for the synthesis algorithm.
- **Future: server-side rendered overlays.** The `?`-binding popup,
  command-prompt, and any future server-rendered chrome (DESIGN §13)
  is built by drawing into a separate `Terminal` and snapshotting it
  the same way.

**The hot path does not use `RenderState`.** Under ADR-0013, the hot
path is "PTY bytes → forward as `PANE_OUTPUT` to subscribed clients
after per-client capability rewriting." There is no diff compute; the
server's canonical `Terminal` exists to answer queries (snapshot on
attach, capability-driven rewriter context, future hooks) and to
satisfy ADR-0004's "server holds canonical state" invariant — not to
generate per-frame diffs.

## 5. How phux uses it on the CLIENT

The client runs a libghostty `Terminal` per attached pane (this is
new under ADR-0013; the prior shape kept a `DiffMirror` instead).
Frame loop:

```rust
// On each PANE_OUTPUT frame:
terminal.vt_write(&frame.bytes);

// On each render tick (paced by frame timer, not by inbound frames):
let snapshot = render_state.update(&terminal)?;
match snapshot.dirty()? {
    Dirty::Clean   => return,                    // skip; nothing to draw
    Dirty::Partial => draw_dirty_rows(&snapshot, &mut row_iter, &mut cell_iter)?,
    Dirty::Full    => draw_all_rows(&snapshot, &mut row_iter, &mut cell_iter)?,
}
snapshot.set_dirty(Dirty::Clean)?;
```

`draw_dirty_rows` walks the row iterator, skips clean rows, and for
each dirty row emits whatever the client's rendering backend needs —
for the TUI backend, that is a VT-escape sequence positioned to the
row; for a future GUI backend, it is glyph quads. Per-row dirty bits
are reset after the row is drawn. This gives efficient incremental
rendering for free across multi-pane layouts: a busy pane in one
quadrant does not force redraw of three idle panes.

The client never sees `DiffOp`s under ADR-0013; the wire is bytes,
and dirty tracking is a *local* render-side concern on each end.

## 6. What RenderState does NOT solve

- **It is not a wire diff format.** Per-row dirty bits are useful
  for local render perf; they do not change the optimal wire shape.
  The wire shape, per ADR-0013, is bytes for content + structured
  envelopes for everything else.
- **It is viewport-only.** `RenderState` covers the visible grid.
  Scrollback is reached through `Terminal`'s screen API
  (`terminal.grid_ref()` and friends), not through `RenderState`.
  Scrollback transport semantics remain phux's design problem.
- **Out-of-band features live elsewhere on the `Terminal`.** Kitty
  graphics image registry, hyperlink intern table, mode flags, and
  cursor style are reached via `Terminal` accessors, not via the
  `RenderState` `Snapshot`. The snapshot covers cell-level visible
  state plus cursor position and the active palette.

`RenderState` is a tool for *drawing the next frame efficiently
against a `Terminal` that already knows the truth*. It is not a tool
for moving the truth across a wire.

## 7. Snapshot-on-attach: the byte synthesis algorithm

When a client attaches, the server owes it a `PANE_SNAPSHOT` (SPEC §8
under the ADR-0013 rewrite; the bytes field is `vt_replay_bytes`).
The server synthesizes those bytes by walking `RenderState` and
emitting VT that reproduces the grid when written to a fresh
`Terminal`. Informational sketch — implementation lands in a future
server commit:

```text
1. Reset target:
     ESC[!p ESC[2J ESC[H        # DECSTR + ED 2 + CUP home
2. For each row r in 0..rows():
     prev = default
     For each cell c:
       cur = (c.style(), c.fg_color() or colors.fg, c.bg_color() or colors.bg)
       If cur != prev: emit SGR delta; prev = cur
       g = c.graphemes()
       Emit UTF-8(g) if non-empty else ' '
     Emit ESC[0m
     If row is not wrap-flagged and r is not last: emit '\r\n'
3. Cursor position:
     If snapshot.cursor_viewport() == Some({x, y, ..}):
       emit CSI <y+1>;<x+1> H
4. Cursor visibility/style:
     visible:   CSI ?25 h / l
     DECSCUSR:  Block -> 2 (blink 1); Underline -> 4 (blink 3);
                Bar   -> 6 (blink 5); BlockHollow -> 2 (TODO)
5. Mode bits via DECSET — alt-screen, bracketed paste, mouse modes,
   kitty keyboard level. Source is terminal.mode(), not RenderState.
6. Out-of-band registries (OSC 8 hyperlinks, APC G kitty images)
   emit after cells. Deferred; not fully decided.
```

Two structural notes:

- **Wide-cell tails** (`at_wide_tail`; empty `graphemes()` on the
  tail) are skipped — emitting the base grapheme advances across
  both cells.
- **Wrapped rows** are tracked in `Row` flags; the soft wrap
  survives only if `\r\n` is suppressed between the row and its
  continuation.

Cost is `O(cols × rows)` worst-case per attach — fine; attaches are
rare and the grid is small. This is what ADR-0013 means by "mosh-
style snapshot synthesis."

## 8. References

- libghostty-rs source: `crates/libghostty-vt/src/render.rs` at
  rev `31d1f70` in this workspace.
- [ADR-0013](../docs/adr/0013-libghostty-bytes-on-wire.md) — the pivot
  decision; this note is the renderer-side companion.
- [ADR-0008](../docs/adr/0008-use-libghostty-types-directly.md) — input
  and style atoms; still in force on the input direction.
- SPEC §8 (post-ADR-0013 rewrite) — `PANE_OUTPUT` and
  `PANE_SNAPSHOT` wire shape.
- SPEC §13 — attach replay sequence; the snapshot algorithm above
  produces the `vt_replay_bytes` that section references.
- [docs/architecture/crate-graph.md](../docs/architecture/crate-graph.md)
  §"Wire bytes: implementation participation" — the architectural
  framing this note complements.
