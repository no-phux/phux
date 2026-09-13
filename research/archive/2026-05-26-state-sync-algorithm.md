---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-05-28
---

> **Archived 2026-05-28.** Ratified by [ADR-0018](../../docs/adr/0018-lazy-state-synchronization.md).
> Preserved here for the research record; not the authoritative source.
> For current behavior see the ADR.

# State synchronization for libghostty Terminals: algorithm composition

**Date:** 2026-05-26 (substantially revised 2026-05-26; see "Revision
history" at end)
**Status:** Research note. Captures the algorithm we expect to land for
phux's long-arc wire semantics. Ratified by ADR-0018. The original
framing claimed implementation was gated on a missing libghostty
primitive; that turned out to be wrong on the cache side — `RenderState`
fills that role and is already in use in-tree. See "Dependencies"
(revised) for the current picture.

---

## Problem

phux's wire (ADR-0013) carries VT bytes between two `libghostty_vt::Terminal`s
— canonical on the server, mirror on the consumer. Today the server
forwards the PTY's byte stream directly. That works for local UDS and
fast SSH stdio. It scales poorly on lossy or high-latency transports, on
slow clients that wedge the server's outbound queue, and on cross-host
federation where every byte of every PTY's history has to traverse the
hub. The class of problem ADR-0013 left open is the *long-arc network
behavior* of the wire.

The well-known solution to this class of problem is **state
synchronization**: ship the *minimum delta from a known reference state
to the current state*, on a tick, per consumer, with the wire content
being self-correcting against packet loss. Mosh built this for
terminals in 2012. ncurses solved a smaller-bore version of the
"diff two screens, emit minimum VT" subproblem in the 1980s. The
algorithms exist. The engineering question is the **composition**:
which pieces of which algorithm go where, given that libghostty is
our state model.

---

## Mosh's State Synchronization Protocol, refresher

Mosh's algorithm (open source at https://github.com/mobile-shell/mosh,
state sync in `src/network/`, terminal display in
`src/terminal/terminaldisplay.cc`):

1. **State as the wire's unit.** The wire doesn't carry "what
   happened." It carries "what the new state is, expressed as a delta
   against a state you already have." Every datagram is self-contained
   against a known reference.

2. **Per-client cached reference state.** Server tracks, for each
   client, the last state that client acknowledged. To send, the
   server diffs *the current state against the client's last-acked
   reference* and ships the diff.

3. **Tick-based emission.** Server emits diffs at a periodic tick
   (~50ms baseline, RTT-adaptive). Between ticks, PTY output is
   absorbed into the canonical state; only the *result* is shipped,
   not every intermediate byte. Coalescence is structural, not a
   rate cap on the original stream.

4. **Loss tolerance is inherent.** Mosh runs over UDP. If a datagram
   is lost, no retransmit machinery is needed — the next tick will
   produce a *larger* diff (against the same older reference) and
   ship that. The receiver applies whichever packet it gets; the
   wire heals itself.

5. **Acks drive cache eviction.** When the client confirms receiving
   sequence N, the server can discard cached state references older
   than every client's `last_acked_seq`. Bounded memory.

The wire format Mosh actually uses is *serialized terminal state with
binary delta compression against the reference serialization*. The
server serializes its full Terminal `Complete` struct (framebuffer +
cursor + modes + title), runs the new serialization through a
delta-compression encoder against the client's reference
serialization, and ships the compressed delta. The client decompresses
to "the new Complete," replaces its cached state, renders.

This last bit — *the client installs a state object directly* — is the
one Mosh-shaped move that doesn't survive contact with libghostty.

---

## The libghostty constraint

`libghostty_vt::Terminal` exposes:

- **Bytes-in via `vt_write(&[u8])`.** The only path that loads grid
  state. No `set_grid`, no `apply_state`, no `restore_from_serialized`.
- **Structured readout via `grid_ref()`, `cursor_*()`, `mode()`,
  `RenderState`.** Read the current state in structured form.

This shape was a deliberate choice on libghostty's part — "bytes-in,
structure-out" — and is the foundation ADR-0013 was built on.

It also means we cannot port Mosh's wire literally. The receiver-side
"install this state object" step has no API. Whatever we ship has to
be VT bytes that, when fed through `vt_write`, *produce* the target
state from the receiver's current state.

So the algorithm has to substitute: keep Mosh's **framework** (tick,
per-client cache, diff-against-reference, ack-driven eviction, loss
tolerance) but swap the **diff encoding** for *minimum-VT-to-transition*
synthesis instead of *serialized-state-with-delta-compression*.

---

## The diff-encoding algorithm: screen-diff to VT

The replacement subproblem — "given two terminal screen states, emit
the shortest VT byte sequence that transitions one into the other" —
has reference implementations going back decades:

- **ncurses** (`lib/lib_doupdate.c`, function `_nc_doupdate` and its
  helpers, descended from 4.4BSD curses' Ken Arnold / Pavel Curtis
  algorithm). The canonical, most-optimized reference. Uses
  dynamic-programming row alignment, line-insertion / line-deletion
  ops (`IL` / `DL`), character-insertion / -deletion ops (`ICH` /
  `DCH`), and per-cell rewrites with SGR pen tracking and cursor-
  positioning optimization. Optimizes for serial-line byte counts.

- **Mosh's `Display::new_frame()`** (in `src/terminal/terminaldisplay.cc`).
  Simpler than ncurses: row-by-row diff with per-row cursor positioning,
  SGR coalescing (track current pen, emit attribute changes only when
  the pen changes), and per-cell rewrites. Doesn't bother with line-
  insert/delete optimization. Good enough for Mosh's traffic profile
  and several hundred lines of code rather than several thousand.

- **tmux** (`screen-write.c` and friends). Operates in-place via an
  output buffer; less directly applicable because tmux's model is
  operation-based rather than state-diff-based.

**Mosh's shape is the right reference for phux.** ncurses-class
optimality is overkill for a packet-network context where we care more
about per-tick CPU cost than about saving 30 bytes per frame.
Algorithmically the work splits cleanly:

1. **Row alignment.** Walk old grid and new grid in parallel. Where
   the rows are identical, skip. Where they differ, mark the row dirty.
   libghostty's `RenderState::update()` already produces per-row dirty
   bits on the read side — we can use it as the starting point even if
   we then have to walk dirty rows manually.

2. **Per-row emission.** For each dirty row, position the cursor at
   the leftmost changed column (`CSI Pl ; Pc H` or `CSI Pc G`), then
   walk left-to-right:
   - Maintain a *pen* (current SGR attributes). When the next cell's
     attributes differ from the pen, emit the minimal SGR sequence to
     update the pen, then update the local pen.
   - Emit the grapheme cluster for the cell.
   - Skip runs of unchanged cells by jumping the cursor (`CHA n`)
     rather than overwriting them.

3. **Cursor restore.** After all dirty rows are flushed, position the
   cursor at the canonical state's cursor location. Set cursor style
   (`DECSCUSR`) and visibility if they changed.

4. **Modes.** DEC modes (altscreen, bracketed paste, cursor key
   application, mouse protocol, etc.) that differ between old and new
   require `DECSET` / `DECRST` emission. The set of relevant modes is
   small; a flat comparison suffices.

5. **Scrollback.** Diffing scrollback is expensive and rarely useful
   incrementally. Ship scrollback only on the initial attach
   (`TERMINAL_SNAPSHOT.scrollback_bytes`); incremental diffs apply
   to the visible grid only.

Implementation size estimate: ~500–1000 lines of Rust, with the bulk
in (2). Reference shape: Mosh's `new_frame()`. Not novel research.

---

## Per-client cache management

The real engineering hard part is *what state the server holds, per
client, for diff purposes*.

A snapshot needs the grid (`cols × rows × cell_size`), the cursor
position/style/visibility, and the relevant modes. Rough bytes:

- Grid at 200 × 50 × 16 bytes/cell = 160 KiB
- Cursor + modes ≈ 100 bytes

So **the per-client cache is dominated by the grid**, at ~160 KiB for a
typical-sized terminal. Multiple clients × multiple terminals × this
cache is the memory budget.

Mosh's mitigations, adapted:

1. **One reference snapshot per client, not a ring.** Loss tolerance
   doesn't require keeping a history — every diff is against the
   single most-recent ack. If a packet is lost, the next diff is
   against the same reference and is larger.

2. **Don't cache scrollback in the diff base.** As noted above; the
   grid is the diff target. Scrollback is initial-attach only.

3. **Re-snap on diff blowup.** When the diff bytes exceed the cost of
   a full snapshot from empty, emit a snapshot and reset the client's
   reference to the new current state. Bound the per-tick worst case
   at one full-snapshot's worth of VT.

4. **Copy-on-write between live Terminal and the cached snapshot.**
   This is where the libghostty primitive matters most. If libghostty
   can hand us a *cheap snapshot* of its current grid (a structural
   handle that shares cell-page storage with the live grid, with
   page-COW on writes), the cache cost drops from "full grid copy"
   to "page-table overhead." Without this, every per-client cache is
   a full grid copy.

Without COW, with one client and a typical-sized terminal: 160 KiB *
N terminals = trivial. With ten clients × ten terminals = 16 MiB. Not
disastrous. The COW optimization is wanted but not load-bearing for
v0.2; load-bearing for federation hubs that might fan out one terminal
to many clients across satellites.

---

## The tick scheduler

The other Mosh piece worth taking literally: **RTT-adaptive tick
interval**. Static 60 Hz is wasteful on a slow link (you're shipping
state changes nobody can ack in time) and laggy on a fast one (you
could be shipping at 250 Hz over a LAN).

Mosh's approach: measure RTT via timestamped acks. Pick a tick interval
roughly `RTT / 2` clamped to `[20ms, 200ms]`. Adjust slowly.

First-cut for phux: fixed 33 Hz (30ms). Adapt later. Don't tick if
nothing changed (server tracks per-Terminal `dirty` since last tick).

---

## The composition, end-to-end

```
SERVER, per Terminal:
    libghostty Terminal      (canonical, always current)
    seq                      (monotonic, ++ when state changes)
    dirty: bool              (set on every vt_write into canonical)
    per attached client:
        last_acked_seq
        cached_grid          (the grid at last_acked_seq;
                              ideally a libghostty COW snapshot)

    on PTY bytes received:
        canonical.vt_write(bytes)
        seq += 1
        dirty = true

    on tick:
        if not dirty: return
        for each client subscribed:
            transition_bytes = synthesize_diff(client.cached_grid,
                                               canonical)
            if len(transition_bytes) > full_snapshot_threshold:
                # Diff blowup; re-snap instead.
                transition_bytes = full_snapshot_vt(canonical)
                client.cached_grid = snapshot(canonical)  # the new ref
            send TERMINAL_OUTPUT { terminal_id, seq, bytes: transition_bytes }
        dirty = false

    on FRAME_ACK { terminal_id, seq } from client:
        if seq > client.last_acked_seq:
            client.last_acked_seq = seq
            client.cached_grid = snapshot(canonical at seq)
                (or whatever's closest available;
                 in practice we keep just one snapshot per client,
                 the most recent ack)
        evict any older cached_grids that no client references

CLIENT, per Terminal:
    libghostty Terminal      (mirror)
    last_received_seq

    on TERMINAL_OUTPUT { terminal_id, seq, bytes }:
        if seq > last_received_seq:
            mirror.vt_write(bytes)
            last_received_seq = seq
            send FRAME_ACK { terminal_id, seq }
        # else: older or duplicate packet, ignore

    render from mirror at its own pace
        (RenderState dirty tracking, when libghostty's Dirty
         FFI bug is sorted; phux-l0t)
```

The wire shape is **identical to ADR-0013's**. Same
`TERMINAL_OUTPUT { terminal_id, seq, bytes }`. Same
`FRAME_ACK { terminal_id, seq }`. Same `TERMINAL_SNAPSHOT` (which is
now just the special case `synthesize_diff(empty, canonical)`).

What changes is the *content* of those bytes. Today they happen to be
the PTY's byte stream. Under state sync they're synthesized to be the
minimum transition. The protocol is silent about which (a server is
permitted to do either, as long as the bytes produce the correct
target state when applied to the receiver's current state).

---

## Dependencies (revised 2026-05-26)

The original version of this section claimed the algorithm gated on a
new libghostty primitive (`Terminal::snapshot_grid()` +
`Terminal::diff_into()`). Re-reading upstream's C headers and the
in-tree usage shows that's wrong on the cache half. Updated picture:

### 1. Cache primitive — already exists as `RenderState`

`libghostty_vt::RenderState` (C: `GhosttyRenderState`) is documented in
`include/ghostty/vt/render.h:21–34` as:

> Represents the state required to render a visible screen (a viewport)
> of a terminal instance. This is stateful and optimized for repeated
> updates from a single terminal instance and only updating dirty
> regions of the screen … only needs read/write access to the terminal
> instance during the update call … safely multi-threaded as long as a
> lock is held during the update call to ensure exclusive access to
> the terminal instance.

And critically (render.h, "Dirty Tracking"): *"The `update` call does
not unset dirty state, it only updates it."* Dirty bits are
caller-managed via `Snapshot::set_dirty(Dirty::Clean)` and
`Row::set_dirty(false)`. That means each consumer can hold its own
RenderState whose dirty bits track "what's changed since this
consumer's last-acked seq" — exactly the per-consumer cached reference
state Mosh's framework requires. N RenderStates per Terminal is the
supported pattern, not a workaround.

phux already does this in two places:

- Server: `crates/phux-server/src/grid.rs::SnapshotSynthesizer` owns a
  `RenderState<'alloc>`, calls `update(terminal)` on demand, walks
  rows+cells, emits VT.
- Client: `crates/phux-client/src/attach/render.rs::PaneRenderer` does
  the same on the mirror side for terminal output.

The state-sync server work is **lifecycle**, not a new primitive:
generalize SnapshotSynthesizer to hold one RenderState per attached
consumer (today: one per Terminal), and drive per-consumer dirty
resets from FRAME_ACK instead of from synthesis completion.

The original "naive `GridSnapshot` = full grid copy ~160 KiB per
consumer" budget still applies — that's just RenderState's per-instance
working set. The COW optimization called out in the original draft is
still wanted at federation scale (many consumers × one Terminal), and
that *is* an upstream ask, but it is not a v0.2 blocker: a hub with
~10 consumers × ~10 terminals × ~160 KiB ≈ 16 MiB, which is fine
without COW.

### 2. Synthesis primitive — still phux's work, smaller than estimated

The screen-diff-to-VT emitter does not exist in libghostty. It also
does not need to. The "from empty" case is already implemented in
`SnapshotSynthesizer::synthesize` (DECSTR + ED 2 + CUP home + per-row
SGR-deltas + cursor restore + mode replay). The state-sync extension
is the *incremental* case:

1. Skip the reset header. Consult `Snapshot::dirty()` to decide whether
   to emit anything at all.
2. If Partial, walk rows; skip rows with `Row::dirty() == false`.
3. For each dirty row, do exactly what `synthesize` does today — CUP
   to row, emit per-cell SGR-delta + grapheme.
4. Diff cursor and mode bits flat against the consumer's last-acked
   cursor/mode set (small, server tracks these per consumer).
5. After emission for consumer C, **do not** clear C's dirty bits
   until C's FRAME_ACK arrives. Loss tolerance falls out: a lost
   packet means the bits stay set, next tick emits a larger diff
   against the same older reference.

Because we get the dirty row set directly from RenderState, we do not
need the full ncurses-class row alignment / line-insert-delete
optimization. The synthesizer is per-dirty-row pen-tracking,
identical to what SnapshotSynthesizer already does. Estimate revised
from ~500–1000 LOC to ~200–400 LOC of new code, most of it shared
with the existing synthesizer.

### 3. RTT measurement

Either round-trip timing on `PING`/`PONG` (we have these) or
piggybacked timestamps on `FRAME_ACK`. Small.

### 4. Load-bearing prerequisites

- **`Snapshot::dirty()` reliability on subsequent updates.** The
  deferred test in `phux-l0t` saw `Error::InvalidValue` (FFI returned
  a `Dirty` value outside `{Clean, Partial, Full}`) when re-`update()`
  was called without an intervening `set_dirty(Clean)`. Production
  paths reset between updates and work fine; state-sync depends on
  dirty consultation per tick, so re-prioritize phux-l0t and either
  characterize the FFI behavior or document the required reset
  invariant.
- **Per-consumer RenderState ownership.** Lives in the per-Terminal
  actor. Blocks on `phux-28f` (server-side Terminal placement —
  spawn_local vs. actor pattern) settling first.

---

## Open questions

- **Where does the diff algorithm live?** phux-only first (Option A
  from the design discussion), or upstream into libghostty (Option B),
  or both phases (Option C: ship in phux, upstream when proven). Option
  C is the most likely path; this note doesn't decide between them.

- **Cursor-style and modes diff format.** Straightforward but unstated
  above. Compare flat: emit each changed mode's DECSET/DECRST. Detail
  for the implementer.

- **What happens on grapheme-cluster boundaries.** libghostty's cell
  carries a grapheme cluster, not a codepoint. The wire emission must
  preserve cluster boundaries. Per-cell rewrite handles this trivially;
  cell-skip-and-overwrite optimization needs to respect cluster width.

- **Image protocols (Kitty graphics, sixel).** These don't fit a
  grid-cell diff model. Today they ride on the PTY byte stream as
  opaque escape sequences; the canonical Terminal records what's
  displayed and where. Under state sync, image regions become a
  separate diff axis ("image at (row, col) changed" rather than "this
  cell's grapheme changed"). Out of scope for the first cut; the
  initial state-sync implementation can fall back to bytes-on-wire
  for any terminal that's emitted image content recently.

- **TUI predictive echo composition.** Predictive echo is the
  client-side overlay (docs/architecture/predictive-echo.md). State-sync server-side is
  orthogonal — predictive cells live in the overlay on top of the
  mirror; the mirror tracks server-acked state. They compose cleanly.

---

## References

- Mosh: https://github.com/mobile-shell/mosh
  - State sync in `src/network/` (look at `Connection`, `Transport`).
  - Terminal display synthesis in `src/terminal/terminaldisplay.cc`
    (`Display::new_frame()`).
  - The original Mosh paper: Winstein and Balakrishnan,
    "Mosh: An Interactive Remote Shell for Mobile Clients,"
    USENIX ATC 2012. The State Synchronization Protocol is described
    in §3.
- ncurses: https://github.com/mirror/ncurses
  - `lib/lib_doupdate.c`: `_nc_doupdate` and helpers.
  - Pavel Curtis's algorithm description in early BSD curses comments.
- libghostty-vt: https://github.com/Uzaaft/libghostty-rs (the Rust
  crate we depend on). The Zig source for the underlying terminal is
  in Ghostty itself; `Terminal`, `RenderState`, `grid_ref`, `vt_write`,
  `cursor_*`, `mode` are the load-bearing API surface for this work.

---

## Status of this note

Captures the algorithm shape phux is aiming at for the long-arc wire
behavior. Ratified as ADR-0018. Implementation is not gated on an
upstream libghostty primitive — see Dependencies §1, revised — and
proceeds against the in-tree `RenderState` cache + an extension to
`SnapshotSynthesizer` for the incremental synthesis path.

---

## Revision history

- **2026-05-26 (initial)**: drafted alongside ADR-0018. Framed
  implementation as gated on a missing libghostty primitive
  (`Terminal::snapshot_grid()` + `Terminal::diff_into()`).
- **2026-05-26 (revised)**: corrected the Dependencies framing after
  re-reading upstream C headers (`include/ghostty/vt/render.h`) and
  the existing in-tree `SnapshotSynthesizer` / `PaneRenderer` usage.
  `RenderState` fills the per-consumer cache role; the synthesis half
  is phux-side work that extends `SnapshotSynthesizer`. ADR-0018
  carries the matching addendum. Original "Dependencies" text is
  preserved below the revised section header in git history (commit
  history is the canonical record).
