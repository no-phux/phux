---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0013 — Libghostty bytes on the wire; structured input remains

**TL;DR.** phux's wire carries VT bytes for pane content and structured events for input. Both server and client instantiate libghostty `Terminal`s; the client's is a local replica fed by server bytes. Per-client capability downsampling moves from the cell level to a server-side VT byte-stream rewriter. Supersedes ADR-0002 in full.

Status: Accepted
Date: 2026-05-25

This ADR supersedes ADR-0002 in full.

## Context

ADR-0002 chose a structured cell-level diff protocol over VT byte
replay. That decision rested on a cost model: parse VT once at the
server, ship structured diffs to N clients, and never pay parse cost
again. The framing was that "VT byte replay forces N clients × N
parsers, which wastes CPU." A year of building against that model has
made two things clear, and both of them invalidate the bet.

**The cost model was wrong.** libghostty parses VT at microseconds per
kilobyte. A busy interactive session emits well under a megabyte per
second of PTY output; the parse cost is invisible against everything
else the client does (decode, layout, draw). The "N clients × N
parsers" framing also imagined N machines. The actual phux deployment
is N processes on one workstation — the parser runs once per attached
TUI, and "once per attached TUI" is a rounding error on a machine that
can render the grid at 144Hz. The CPU we thought we were saving
doesn't exist.

**The protocol-design cost is perpetual.** Every libghostty feature
that touches a cell — Kitty graphics, the modern key protocol, sixel,
hyperlinks, future image protocols, selection APIs (Ghostty PR
#12794), whatever Ghostty 1.5 ships — has to be re-modeled in phux's
wire format before it can reach a client. Wave 8 of the protocol epic
made this concrete: we shipped cursor-as-wire-field, modes-as-wire-
field, per-cell capability downsampling, a `SmallVec` on `Cell::text`
for grapheme clusters, and a phux-flavored mirror of libghostty's
info-types. Every one of those was structural work whose only job was
to push libghostty's existing model across the wire and back. We were
building **against** libghostty's interface on the receiving end
instead of **through** it.

**The discovery that closed the question.** libghostty's `Terminal`
type is a bytes-in / structure-out box.

- `Terminal::vt_write(&mut self, data: &[u8])` is the **only** way to
  feed grid content into a `Terminal`. There is no `set_cell`, no
  `apply_diff`, no `feed_grid_state`, no `restore_snapshot`. The API
  refuses any path that doesn't go through the VT parser.
- `Terminal::grid_ref()`, `cursor_x/y()`, `mode()`, `cursor_style()`,
  and friends read structured state **out** of the terminal — perfect
  for a renderer.
- `RenderState::update(&terminal)` provides built-in dirty tracking
  (Clean / Partial-with-per-row-bits / Full) on the read side. This
  is render-side dirty tracking, not a wire diff, but it means both
  the server's grid snapshotter and the client's renderer get
  efficient incremental redraw for free, on each side, locally.

A structured-diff wire protocol means synthesizing some other way to
load a `Terminal` — which doesn't exist — or bypassing libghostty on
the client and keeping a parallel grid representation (which is what
the `DiffMirror` plan amounted to). Either path fights the library.
The shape that **uses** libghostty is the shape libghostty's API was
built for: bytes in, structure out.

## Decision

phux's wire protocol carries **VT bytes** for pane content and
**structured events** for input. Both server and client instantiate
libghostty `Terminal` objects; the server's is the canonical source
of state, the client's is a local replica fed by bytes received from
the server. Per-client capability downsampling moves from the cell
level to a server-side VT byte-stream rewriter.

### Server → Client: bytes for content, structured envelopes for everything else

| Frame | Payload | Notes |
|-------|---------|-------|
| `PANE_OUTPUT` | `{ pane_id, bytes }` | hot path; PTY bytes forwarded, downsampled per client caps if needed |
| `PANE_SNAPSHOT` | `{ pane_id, cols, rows, vt_replay_bytes }` | on attach; server synthesizes a byte sequence that, when `vt_write`-en to a fresh `Terminal`, reproduces the current grid (mosh-style: clear + cursor home + SGR runs + write cells, derived from `grid_ref()`) |
| `ATTACHED`, `DETACHED`, `PANE_OPENED`, `PANE_CLOSED`, `WINDOW_OPENED`, `WINDOW_CLOSED`, `SESSION_OPENED`, `SESSION_CLOSED`, `BELL`, `ERROR`, `PONG` | structured | lifecycle frames; the session graph is still phux's vocabulary, not libghostty's |

The split is principled: pane content is a byte stream emitted by a
process; the multiplexer's job there is forwarding, not interpreting.
Session/window/pane lifecycle is phux's invention — libghostty knows
nothing about it — so those frames stay structured.

### Client → Server: structured input (libghostty atoms re-exported per ADR-0008)

| Frame | Payload |
|-------|---------|
| `INPUT_KEY` | `{ pane_id, KeyEvent }` — composing libghostty's `key::Action` + `key::Key` + `key::Mods` + text |
| `INPUT_MOUSE` | `{ pane_id, MouseEvent }` — composing libghostty's `mouse::Action` + `mouse::Button` + `key::Mods` + position |
| `INPUT_FOCUS`, `INPUT_PASTE` | structured |
| `HELLO`, `ATTACH`, `DETACH`, `PING`, `VIEWPORT_RESIZE`, `COMMAND` | structured |

Input remains structured because byte encoding for the PTY is
mode-dependent: a single Up-arrow press becomes `\e[A` under normal
mode but `\eOA` under `APP_CURSOR_KEYS`, and the equivalent decisions
exist for mouse-protocol variants, paste bracketing, focus reporting,
and the modern key protocol. **Only the server knows the pane's
current mode state.** Sending pre-encoded bytes from the client would
either require the client to track every libghostty mode bit
(re-introducing the structured-mirror problem on the input side), or
guess wrong. Structured events ship raw semantics to the server,
which uses libghostty's encoders + the pane's own `mode()` to produce
the correct bytes for the PTY.

This asymmetry — bytes one way, structure the other — mirrors the
asymmetry in the underlying physics. Output is a stream of bytes a
process is producing. Input is keystrokes from a human, which become
bytes only in the context of a terminal mode.

### Per-client capability downsampling: server-side byte rewriter

A v1 client advertises `ColorSupport::TrueColor`; a legacy client
advertises `Color256` or `Color16`. The server runs a byte-stream
rewriter that translates `\e[38;2;R;G;Bm` truecolor → `\e[38;5;Nm`
256-color → `\e[3Nm` 16-color on the way out, per client, on
`PANE_OUTPUT` and `PANE_SNAPSHOT`. tmux already does this for `Tc` /
`RGB` clients — well-trodden territory. The cell-level downsampling
that landed in `cc30ab5` is replaced by stream-level rewriting; the
*concept* survives, the *layer at which it lives* moves from the
emitter to the wire.

## Rationale

### The cost-model correction

The thing ADR-0002 was buying — saved parse cycles across clients —
is not on the budget at modern libghostty speeds and at the actual
deployment scale (one machine, a handful of clients). The thing it
was selling us — a perpetually evolving wire format that has to chase
libghostty's feature surface — has a real, recurring cost. We paid
that cost through 8 waves of the protocol epic. The trade is
inverted.

### Build with libghostty's API, not against it

libghostty exposes `vt_write` as the **only** way to populate a
`Terminal`. Any wire protocol that doesn't ship bytes is, on the
receiving end, either synthesizing bytes from structured data (parse
once and re-emit, which loses information) or maintaining a separate
grid representation that bypasses libghostty (`DiffMirror`). The
first path is wasteful; the second is the structural-mirror problem
ADR-0008 talked about, moved one layer up. Bytes on the wire match
the shape of the API on both ends — server reads bytes out of the
PTY and forwards them; client receives bytes and `vt_write`s them
into its local `Terminal`.

This is the same insight as ADR-0008 applied at the protocol layer
instead of the type layer. ADR-0008 stopped mirroring libghostty's
input/style enums; ADR-0013 stops mirroring libghostty's grid model.

### RenderState dirty tracking is free on both ends

The thing we thought we were getting from a wire-diff — "the client
only redraws what changed" — `RenderState::update(&terminal)` already
provides, locally, on each end. Server uses it to drive snapshot
synthesis efficiently; client uses it to skip clean rows on redraw.
Neither needs the wire to know about dirtiness. The wire ships
whatever bytes the PTY produced; the rendering on each side is
incremental because libghostty's `RenderState` says so.

### Predictive echo gets simpler, not harder

ADR-0007 named predictive local echo as a client-side feature
operating against the diff mirror. Under bytes-on-the-wire, the
mirror **is** a local libghostty `Terminal`, and the prediction
strategy becomes: speculatively `vt_write` the user's keystrokes
into a shadow `Terminal` (or directly into the rendered one, with a
predicted-cells overlay), reconcile against authoritative bytes from
the server when they arrive. This is closer to how Mosh actually does
it than the diff-mirror sketch was.

## Consequences

### Positive

- **Dead-simple wire.** A `PANE_OUTPUT` frame is two fields:
  `pane_id` and `bytes`. There is no per-cell encoding to specify, no
  cursor-on-wire to keep in sync, no mode-on-wire to bikeshed. SPEC
  §8 shrinks substantially.
- **Automatic libghostty feature parity.** Every wire-affecting
  libghostty improvement — Kitty graphics, modern key protocol,
  sixel, hyperlinks, selection APIs, anything Ghostty merges next —
  lands on phux clients on `cargo update`, with zero protocol work.
  This is the ADR-0008 dividend extended to grid content.
- **Snapshot-on-attach via byte synthesis.** Mosh ships this pattern:
  walk the grid, emit a CSI/SGR/text sequence that reproduces it on a
  fresh terminal. `Terminal::grid_ref()` is exactly what that walker
  reads from. Client receives the snapshot bytes, `vt_write`s them,
  and is caught up — same code path as live output.
- **Predictive echo is structurally simpler.** Speculatively
  `vt_write` user keystrokes (encoded via libghostty's encoders + the
  client's best guess at mode) into a shadow `Terminal`; render with
  an overlay marking predicted cells; reconcile when the
  authoritative `PANE_OUTPUT` arrives. The reconciliation logic is
  diff-of-grids, which libghostty already supports via `grid_ref()`.
- **Non-libghostty clients still work.** A recording client writes
  the byte stream to disk; an inspection client greps it; a replay
  client tee's it into a `Terminal` later. The wire being a byte
  stream is *more* friendly to non-libghostty consumers than a
  phux-defined structured grid would be.

### Negative

- **Per-client byte-stream rewriting is real work.** For each
  downsampling client, the server walks the bytes of every
  `PANE_OUTPUT` looking for SGR sequences and rewriting truecolor →
  256 → 16. tmux does this; it is correctness-sensitive (cursor
  state mid-sequence, multi-parameter SGR, `\e[38;2;...:...:...m`
  ITU-style separators); it has to be fast on the hot path. This is
  the protocol's largest implementation cost under shape C, and it
  replaces a cleaner per-cell `Cell::downsample` that ran once per
  emitted diff.
- **Non-libghostty clients pay parse cost.** A recording or
  inspection client that wants structured cells has to parse VT
  itself, or embed libghostty, or operate at the byte-stream level.
  This is a regression from a structured-diff world where the
  structure was on the wire. We accept the regression because (a) no
  such consumer exists yet, (b) embedding libghostty in a Rust
  consumer is one dependency line, and (c) `vte` and similar
  byte-level parsers are mature and ubiquitous.
- **Client wire is partially shaped by libghostty.** Input atoms
  re-exported per ADR-0008 mean a libghostty major-version bump
  ripples into `phux-protocol`'s input layer. ADR-0008 already
  accepted this trade; ADR-0013 leaves it intact on the input side
  and removes the equivalent coupling on the output side (since
  output is now bytes, not structured cells composed of libghostty
  atoms).

### Tradeoffs we are deliberately accepting

- **The wire is "less ours" on the output side.** Anyone who can
  produce VT bytes can drive a phux client; we don't get to add
  semantic frames on the output side without extending the wire
  envelope (which we still control). Acceptable: pane content was
  never the right place for phux semantics anyway; that's what
  lifecycle frames are for.
- **Server holds the canonical `Terminal`.** This was true under
  ADR-0002 too. Under ADR-0013 the client also holds a `Terminal`,
  which slightly increases per-attached-client memory (one
  libghostty grid per pane per attached client instead of one
  diff-mirror). The cost is small (libghostty grids are compact) and
  the architectural win — local `RenderState`, local predictive
  `vt_write` — is large.

## Alternatives considered

- **Shape A — structured cell-level diffs (ADR-0002, now superseded).**
  The original bet. Rejected per the cost-model correction above:
  the parse cost it was avoiding is invisible at modern libghostty
  speeds, and the protocol-design cost it imposes is perpetual.
- **Shape D — row-diffs riding `RenderState`'s per-row dirty bits.**
  Use `RenderState` on the server to identify dirty rows, ship those
  rows as structured cells. Rejected: still requires structured
  per-cell serialization — every out-of-band libghostty feature
  (hyperlinks, Kitty graphics, future image protocols) needs
  protocol coordination to appear in a row. The wire savings vs.
  bytes are marginal once the framing is compressed (PTY bytes are
  already terse; SGR runs out to long stretches of repeated state).
  The protocol-design treadmill survives.
- **Shape E — full grid snapshot per frame.** Ship the entire grid
  every render tick. Rejected: roughly 62KB per 80×24 frame at one
  byte per Cell field including styling, multiplied by frame rate
  and client count. Bandwidth-prohibitive over anything but a Unix
  socket, and not even attractive there.
- **Hybrid — bytes for content + diffs for cursor/mode/etc.**
  Keep some structured state on the wire because "the client needs
  to know the cursor moved." Rejected: this is exactly what
  `vt_write` already does. The cursor moves because the VT stream
  contains `\e[H` or equivalent. There is nothing for the wire to
  add on top of the bytes.

## What this supersedes, and what it leaves intact

### Superseded

- **ADR-0002 in full.** The diff-based wire protocol is replaced by
  bytes-on-the-wire for content. ADR-0002 is marked superseded at
  the top of its file; its content is preserved as historical
  context. Do not implement against it.

### Still valid, sometimes more so

- **ADR-0006 / ADR-0008 — input atoms re-exported from libghostty.**
  More justified under ADR-0013, not less. Input stays structured
  because mode-dependent byte encoding requires server-side
  knowledge; libghostty's encoders are the right tool; re-exporting
  the input atoms means `cargo update` carries every new key /
  mouse / focus extension through.
- **ADR-0010 — frontend-agnostic server.** Easier to defend now. Any
  libghostty consumer is a valid phux client, and a non-libghostty
  client can still operate on the byte stream (recording,
  inspection, replay). The "frontend-agnostic" claim is structurally
  stronger when the wire is a byte stream than when it carries
  phux-defined cell semantics.
- **ADR-0011 — protocol/core independence + IdBridge.** Unaffected.
  The IDs and the bridge are independent of whether the wire ships
  bytes or diffs.
- **ADR-0007 — Mosh-class transport + satellites.** Forward-compat
  invariants survive. The hub-and-spoke relay is bytes-friendly:
  satellites forward `PANE_OUTPUT` byte frames as opaque payloads;
  the hub never needs to re-encode VT. If anything, satellite
  relaying is simpler under ADR-0013 because the hub doesn't have
  to understand cell structure.
- **ADR-0003 / ADR-0004 — single server, libghostty-vt as grid.**
  Unaffected. The server is still authoritative; libghostty-vt is
  still the grid source; the only change is what the server *ships*
  from that grid.

### What's no longer needed in the implementation

Pure-docs commit; the refactors land in subsequent waves. For the
record, the following constructs become dead under ADR-0013:

- `DiffOp` enum + diff codec + diff compute + diff apply
- `Cell`, `CellFlags`, `Color`, `Underline` as **wire** types (still
  fine as libghostty foreign re-exports for non-wire purposes if
  anything needs them; not on the wire)
- `CursorState`, `PaneModes`, `CursorShape` on the wire (libghostty
  tracks these inside `Terminal`)
- `DiffMirror` on the client (replaced by a libghostty `Terminal` +
  `RenderState`)
- `SmallVec` on `Cell::text` (Cell is no longer on the wire)
- `phux-server/src/grid.rs`'s `Color`-to-cell capture path (server
  forwards bytes directly; only needs `RenderState` for snapshot
  synthesis on attach)

### What stays valuable

- `HELLO` / `ATTACH` / `ATTACHED` / `DETACH` / `INPUT_*` / `PING` /
  `BELL` frame skeletons
- `IdBridge`, `Registry::sessions`, the protocol/core independence
  story
- The published-crate stance and `#[non_exhaustive]` enums (more
  important now — wire envelopes evolve, content bytes don't)
- Session / Window / Pane info types in `SessionSnapshot` (the
  session graph is phux's vocabulary; not affected)
- Capability negotiation as a concept (now applied to byte-stream
  rewriting instead of per-cell downsampling)
- Every ADR in this directory except 0002

## References

- `libghostty_vt::Terminal::vt_write`, `grid_ref`, `cursor_x/y`,
  `mode`, `cursor_style` — the bytes-in / structure-out API surface
  that this ADR is built around. Source lives at
  `~/.cargo/git/checkouts/libghostty-rs-*` (pinned rev in
  `Cargo.toml`).
- `libghostty_vt::RenderState::update` — the local dirty-tracking
  primitive that makes per-side incremental rendering free.
- ADR-0002 — diff-based protocol (**superseded by this ADR**).
  Preserved for historical context.
- ADR-0006 — input mirrors libghostty (still valid; reinforced).
- ADR-0008 — re-export libghostty's input/style atoms (still valid;
  this ADR extends the same logic to the wire-protocol layer).
- ADR-0010 — frontend-agnostic server (still valid; easier to
  defend under bytes-on-the-wire).
- ADR-0011 — protocol/core independence + IdBridge (unaffected).
- SPEC §8 — pane state synchronization (substantial rewrite owned by
  a parallel agent; do not edit from this ADR).
- `research/2026-05-25-libghostty-renderstate.md` — research note on
  `RenderState` and the `vt_write`-only input channel (owned by a
  parallel agent).
- tmux's `tty.c` — prior art for SGR truecolor-to-256-to-16
  downsampling at the byte-stream level, which the server-side
  rewriter will need to match in correctness.
- Mosh — prior art for snapshot-on-attach via synthesized VT byte
  sequences derived from a server-side grid representation.
