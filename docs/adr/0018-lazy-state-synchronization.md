---
audience: contributors
stability: stable
last-reviewed: 2026-05-29
---

# 0018 — Lazy state synchronization is the wire's long-arc shape

**TL;DR.** phux's wire is lazy state synchronization of libghostty Terminal state between server and consumer mirrors: `TERMINAL_OUTPUT.bytes` carries the minimum VT to move the consumer's last-acked state to the canonical state, computed per-consumer per-tick. ADR-0013's pass-through bytes is the degenerate case. The wire shape does not change; only the server's emission strategy generalizes.

Status: Accepted
Date: 2026-05-26

The 2026-05-26 addendum at the end of this ADR revises the
implementation gate (the cache primitive is solved by `RenderState`;
the synthesis primitive is smaller than originally estimated and not
load-bearing on upstream libghostty).

## Context

[ADR-0013](./0013-libghostty-bytes-on-wire.md) settled the wire format
as VT bytes parsed by `libghostty_vt::Terminal` on both ends. The
decision named *how* terminal state crosses the wire (bytes; libghostty
parses); it did not name *what kind of bytes* the server emits.

In tree today the server emits **the PTY's bytes after a canonical
parse and per-client capability rewrite** — effectively a pass-through.
That works for the v0.1 deployment targets (local UDS, fast SSH stdio)
because the round-trip is cheap, the transport is reliable, and the
worst-case backlog of pending output per pane is bounded by what the
shell produces between attaches.

It breaks down on the deployments [`docs/vision.md`](../vision.md) commits
to longer-term:

- **Lossy or high-latency transports.** A future QUIC datagram channel
  for resilient client attach, or a hub-to-satellite link with packet
  loss, makes "every byte must arrive in order" expensive. Mosh's
  insight, two decades old now, is that *state* survives losses better
  than *event sequences* do.
- **Slow consumers.** A wedged client today causes per-client queue
  growth on the server (SPEC §12); the server kicks the client at
  threshold. A consumer whose render loop runs at 5 Hz over a 60 Hz
  PTY stream shouldn't force the server to choose between dropping
  it or buffering megabytes per pane.
- **Cross-host fanout.** A hub federating multiple satellites
  (ADR-0007) cannot afford to forward every byte of every PTY to
  every observer. The cost grows with `satellites × terminals ×
  observers × bytes/sec`. State sync grows with `observers × bytes
  per tick`.
- **`yes` flood semantics.** Even today on a reliable transport, a
  shell command that emits 10 MB of output whose *visible result* is
  one prompt and one line of text ships 10 MB. The state at the end
  is small. Pass-through cannot benefit; state sync structurally can.

Mosh solved this class of problem for terminals in 2012 with its State
Synchronization Protocol (SSP). The algorithm is well-understood. The
ncurses screen-diff algorithm covers the subproblem ("emit minimum VT
to transition between two screen states") and predates Mosh by two
decades. The engineering work is *composing them with libghostty as
the state model.*

See [`research/archive/2026-05-26-state-sync-algorithm.md`](../../research/archive/2026-05-26-state-sync-algorithm.md)
for the full algorithm derivation, the libghostty primitive request,
and references.

## Decision

phux's wire is **lazy state synchronization** of libghostty Terminal
state between the canonical server-side Terminal and the consumer-
side mirror Terminal. `TERMINAL_OUTPUT.bytes` carries *the minimum VT
to transition the consumer's last-acked state to the canonical's
current state*, computed per-consumer per-tick. ADR-0013's
"pass-through PTY bytes" is the **degenerate case** of this scheme:
the simplest valid transition for an ack-current consumer is exactly
the PTY bytes the canonical Terminal just consumed.

**The wire shape does not change.** Same `TERMINAL_OUTPUT
{ terminal_id, seq, bytes }`. Same `FRAME_ACK { terminal_id, seq }`.
Same `TERMINAL_SNAPSHOT`, which under state-sync is just the
transition `(empty → current)`. SPEC's normative statement about
`TERMINAL_OUTPUT.bytes` shifts from "VT bytes the canonical Terminal
emitted" to "VT bytes that, applied to the consumer's last-acked
state via `vt_write`, produce the canonical's current state." A
server is permitted to emit either pass-through bytes or synthesized
diffs as long as the resulting state on the consumer matches.

**The framework is Mosh's.** Server-side cached reference state per
consumer; tick-based emission (RTT-adaptive); ack-driven cache
eviction; loss-tolerance by re-diffing against an older reference
when packets are lost; per-consumer pacing.

**The diff encoding is curses/Mosh-class screen-diff-to-VT.** Given
two libghostty grid states, walk row-by-row; for each dirty row,
position the cursor, track an SGR pen, emit minimal attribute updates
and grapheme rewrites. Cursor and mode state are diffed flat.
Scrollback is shipped on initial attach only, never diffed
incrementally.

**Implementation is gated on a libghostty state-snapshot primitive.**
What's needed: a `Terminal::snapshot_grid()` returning a handle to the
current grid (ideally COW-shared with the live Terminal), and a
`Terminal::diff_into(&base, &mut Vec<u8>)` that emits the transition
VT. The naive version (full grid clone, walk both grids) is workable;
the optimized COW version is wanted but not load-bearing. Either way
the primitive belongs in libghostty conceptually because libghostty
owns the state model. phux may ship a wrapper that does this
externally against libghostty's existing readout APIs in advance of
the upstream primitive landing.

## Rationale

- **Loss tolerance is inherent.** Every wire packet is a complete
  transition against a known reference state. Drop a packet, the next
  tick produces a larger diff against the same older reference. No
  retransmit machinery; no head-of-line blocking; no parser-state
  divergence when a byte is lost.
- **Coalescence is structural.** A 10 MB `yes` flood between ticks
  produces one diff per consumer per tick, sized by the visible state
  change (small), not by the byte count (huge).
- **Per-consumer pacing falls out.** Each consumer ticks against its
  own RTT and ack rate. Slow consumers receive less frequent, larger
  diffs; fast consumers receive more frequent, smaller diffs. The
  server doesn't queue.
- **Backpressure dissolves.** SPEC §12's per-client queue + kick-on-
  threshold flow control becomes unnecessary. The server holds one
  reference snapshot per consumer; the worst-case per-tick emission is
  bounded by the cost of a full snapshot from empty. No outbound
  queue accumulation.
- **Federation cost stays linear in *observed* state, not in *historical*
  bytes.** A hub fanning a terminal out to N observers ships N
  per-consumer diffs per tick, each sized by what that consumer has
  yet to catch up on. Not N × bytes-since-history-began.
- **The wire stays libghostty-native.** Receivers feed bytes to
  `vt_write` as before; the only API constraint libghostty imposes is
  unchanged. No "install state" path required.

## Tradeoffs

### Where this costs us

- **Implementation complexity at the server.** Per-consumer state
  caches, tick scheduler, screen-diff algorithm. The research note
  estimates ~500–1000 lines of new code plus the cache management.
  Real work, not trivial.
- **Server memory.** Each consumer holds at least one reference
  snapshot (~160 KiB for a typical-sized grid). Multiple consumers
  × multiple terminals; tolerable, but worth budgeting. COW backing
  from libghostty would mitigate.
- **Dependency on libghostty.** The synthesis primitive is the
  load-bearing piece. We can ship our own in phux first; that's a
  duplication if/when libghostty grows the upstream version.
- **A class of pathological grids that don't compress.** Animations,
  busy progress bars at high refresh rate, fullscreen TUIs that
  repaint everything — these have a large state delta per tick. State
  sync degrades gracefully (the diff just gets bigger), but the win
  over pass-through is small here. Pass-through bytes is sometimes
  the right diff; the algorithm should naturally pick it.
- **Image protocols don't fit a cell-diff model.** Kitty graphics,
  sixel, iTerm2 inline images are non-cell content. The initial
  implementation falls back to pass-through for terminals with active
  image content. A proper solution composes image-region diffs
  alongside cell diffs; out of scope for the first cut.

### Where this is free

- **No wire change.** Existing consumers keep working. The shift is
  in the *semantic* of `TERMINAL_OUTPUT.bytes` and in the server's
  emission strategy, not in the bytes-on-the-wire shape.
- **No new ADR-0013 supersession.** ADR-0013 stays accepted; it
  named the wire format and the parse model. This ADR layers state-
  sync semantics on top.
- **Predictive echo composes.** The client overlay (ARCHITECTURE.md)
  is orthogonal — predictive cells live on top of the mirror; the
  mirror tracks server-acked state. No interaction.
- **Future transports (QUIC datagrams, satellite forwarding) get the
  Mosh property "for free"** once the algorithm is in place.

## Alternatives considered

- **Stay on pass-through forever.** The simplest path. Works fine for
  local UDS. Fails the long-arc deployments (lossy transports, slow
  consumers, fanout federation, `yes` floods). Concedes the v0.2+
  story to whoever builds state sync next.

- **Port Mosh's wire literally** (serialized state with binary delta
  compression). Rejected because libghostty has no `set_state` path;
  the receiver has to apply bytes via `vt_write`. We can take Mosh's
  framework but not its wire format.

- **Per-cell wire diff ops** (`CELL_RUN`, `CLEAR`, `REPEAT`, etc.).
  Rejected by ADR-0013 for the cost-of-modeling reasons listed there.
  Same rejection holds; this ADR does not revisit.

- **Defer the algorithm to v0.3+, ship pass-through through v0.2.**
  Defensible if the libghostty primitive is far off. The cost is
  pushing federation and the QUIC story behind state sync's
  dependency. Worth revisiting if the primitive looks far away when
  v0.2 planning starts.

## Consequences

- **SPEC.md** gains a small section under §8 (currently "Pane state
  synchronization — the hot path") restating `TERMINAL_OUTPUT.bytes`'s
  meaning under state-sync semantics, with the caveat that v0.1
  implementations are permitted to emit pass-through bytes (the
  degenerate case). The restructure that lands ADR-0015/0016 names is
  the right time to make this update.

- **SPEC.md §12 (flow control)** simplifies materially when state-sync
  ships. The per-client queue + kick model becomes a sentence:
  consumers that fall behind receive larger diffs against an older
  reference; the server does not queue. Until state-sync ships, the
  current §12 model holds.

- **ARCHITECTURE.md** gains a section under or near "Predictive local
  echo" sketching the per-Terminal state-sync state machine. Pointer
  to the research note for algorithm details.

- **A libghostty issue / advocacy** for the synthesis primitive. This
  is the upstream work that unblocks the optimized implementation.
  Filing this is a near-term action; the research note has the API
  sketch.

- **An implementation ticket** (when ready) blocked on the libghostty
  primitive. Until that lands, phux can ship its own externally-
  computed diff against libghostty's readout APIs; the ticket
  describes both the local-first and upstream paths.

## Addendum — 2026-05-26: implementation gate revised

The original Decision section above describes implementation as "gated
on a libghostty state-snapshot primitive" and sketches a
`Terminal::snapshot_grid()` + `Terminal::diff_into()` API that does not
exist upstream. Re-reading the upstream C headers
(`include/ghostty/vt/render.h`) and the in-tree Rust usage shows that
framing is too pessimistic. The gate has two pieces; one is already
solved.

**Cache primitive: solved by `RenderState`.** `GhosttyRenderState` /
`libghostty_vt::RenderState` is explicitly designed for "repeated
updates from a single terminal instance and only updating dirty regions
of the screen" (render.h:21–34). It supports N instances per Terminal
(threading note: "safely multi-threaded as long as a lock is held
during the update call"), and `Snapshot::set_dirty(Dirty::Clean)` is
caller-driven, so each consumer's RenderState can hold its
"dirty-since-last-acked-seq" state independently. That is exactly the
per-consumer cached reference state Mosh's framework needs. We already
hold a RenderState per renderer in two places: the server's
`SnapshotSynthesizer` (`crates/phux-server/src/grid.rs`) and the
client's `PaneRenderer` (`crates/phux-client/src/attach/render.rs`).
Generalizing this to one RenderState per (terminal × attached consumer)
is a server-side lifecycle change, not a new upstream primitive.

**Synthesis primitive: still phux's work, but smaller than estimated
and not load-bearing on upstream.** The diff-emit half — "given dirty
rows + current cells, emit minimum VT to make a blank-or-stale row
match" — does not exist in libghostty and is correctly described in
the Tradeoffs section above. But the bulk is already in
`SnapshotSynthesizer::synthesize` today as the *from-empty* case
(DECSTR + ED 2 + CUP home + paint everything). The incremental case
generalizes that walk to consult `Snapshot::dirty()` + `Row::dirty()`
instead of resetting, and to manage per-consumer dirty resets driven
by FRAME_ACK. The full row-alignment / line-insert-delete optimization
the original Decision section names as "ncurses-class" is not needed
in v0.2 — RenderState's per-row dirty bits give us the row set
directly, so the synthesizer is per-dirty-row pen-tracking only.
Estimate revised from ~500–1000 LOC to ~200–400 LOC of new code,
plus per-consumer lifecycle in `TerminalActor`.

**Upstream contribution is still worth doing, but is decoupled.** A
COW-backed snapshot handle in upstream libghostty would help federation
hubs where one Terminal fans out to many consumers (the per-consumer
RenderState pattern is O(N × grid) per tick; COW pages would amortize
the cost across consumers). That's a v0.3+ optimization for federation
scale, not a v0.2 blocker. Filing an upstream issue still has value;
it stops blocking phux.

**What's actually load-bearing:**

1. `RenderState::Snapshot::dirty()` returning a value in
   `{Clean, Partial, Full}` reliably on second-and-subsequent updates.
   The deferred regression case in `phux-l0t` (FFI returned a Dirty
   value outside that set) becomes a real blocker once state-sync
   reads dirty per-tick. Re-prioritize.
2. Per-`TerminalActor` ownership of N RenderStates (one per attached
   consumer), with lifecycle tied to ATTACH/DETACH. Blocked on
   `phux-28f` settling actor placement.
3. The screen-diff-to-VT synthesizer extension to SnapshotSynthesizer.
4. Tick scheduler + FRAME_ACK-driven dirty reset.

**No change to Decision, Rationale, Tradeoffs, or Alternatives.** The
wire shape, the framework choice, and the algorithm are unchanged.
This addendum only revises *what's blocking implementation* and
*where the work lives*. The "Consequences" bullet about
"implementation ticket blocked on the libghostty primitive" is
superseded by the new tickets filed under the state-sync epic.

## Addendum — 2026-05-29: lifecycle live, emission gated (phux-0q8)

The per-consumer machinery (steps 2–4 above) is implemented AND wired
into the runtime: the ATTACH path sends `ConsumerAttachRequest` so the
actor allocates and primes a per-consumer `RenderState` cache, the
DETACH / disconnect / EOF paths send `ConsumerDetachRequest` to free
it, and inbound `FRAME_ACK` clears that consumer's dirty cache via
`mark_synced`. `consumer_states` is now populated at attach and drained
at detach.

**What is deliberately NOT live in v0.1: tick *emission*.** The actor's
`tick_emit` is gated by an internal `consumer_tick_emits` flag,
defaulted off. The live server→consumer emitter remains the
pass-through broadcast pump (the degenerate case named in *Decision*):
the PTY's bytes after canonical parse + capability rewrite. Two reasons
the tick must not also emit while the broadcast pump is the live path:

1. **Double-paint.** Both paths write `TERMINAL_OUTPUT` to the same
   consumer mailbox with independent `seq` counters; an emitting tick
   would deliver the same content twice and break the
   monotonic-`seq`-per-consumer invariant.
2. **No client ack loop yet.** The v0.1 client does not send
   `FRAME_ACK`, so `mark_synced` never fires from acks; an emitting
   tick would re-diff an ever-larger unacked delta forever.

Flipping `consumer_tick_emits` to `true` is the production switch. It
is gated on (a) the client driving the `FRAME_ACK` loop and (b) the
broadcast pump being suppressed per tick-managed consumer so exactly
one emitter serves each consumer. Until then the lifecycle runs live
(allocating, priming, ack-evicting, and freeing the per-consumer cache)
while emission stays pass-through — the two coexist without
double-painting.

## Addendum — 2026-05-30: emission live; per-consumer reference grid, emit-once (phux-ia4)

`consumer_tick_emits` is now `true`: the state-sync tick is the live
server→consumer emitter. Two corrections to the 2026-05-26 addendum's
implementation plan, both forced by empirical reading of the pinned
libghostty (`acc4b87`):

**The per-consumer cache is NOT `RenderState` dirty bits.** The
2026-05-26 addendum claimed each consumer's `RenderState` could hold its
own "dirty-since-last-ack" state independently. That is wrong.
`RenderState::update` *consumes* the shared `Terminal`'s dirty state: it
clears `t.flags.dirty`, the active screen's dirty flags, and the
per-page / per-row dirty bits (`render.zig` `update`, ~lines 440-461 and
647-648). A `RenderState`'s own `Snapshot::dirty()` / `Row::dirty()` are
only *populated* from those shared bits during `update`. So with N
consumers on one pane, the first consumer's `update` consumes the shared
dirty bits and every other consumer that tick sees `Dirty::Clean` —
starving all-but-one. The header sentence "the update call does not unset
dirty state" refers to the `RenderState`'s *own* `self.dirty`, which the
caller resets via `set_dirty`; it does not describe the shared `Terminal`
bits, which `update` does clear. The cache is therefore a per-consumer
**reference grid** (`grid.rs::ConsumerReference`: the last-synced rendered
body of each viewport row + cursor/mode), diffed by
`SnapshotSynthesizer::synthesize_against_reference`, which never reads the
shared dirty bits — full per-consumer isolation regardless of attach/ack
divergence.

**Reference advances on emit (emit-once), not on ack.** v0.1 transports
(UDS, SSH stdio, WebSocket) are reliable and ordered. The reference is
committed to the just-rendered state *before* the frame ships, so a
change is delivered exactly once and an unchanged terminal produces no
re-emission. This matches proto.md §8's emit-once model and the prior
broadcast pump's behavior, and keeps a non-acking consumer from
re-receiving the same diff every tick. The loss-tolerance "re-diff
against an older reference on a dropped frame" property (this ADR's
*Rationale*) is inherent to the reference-grid model but is only wired
when a lossy transport (QUIC datagrams, satellite forwarding) lands;
`FRAME_ACK` stays wired for backpressure accounting (proto.md §8.2).

No change to Decision, Rationale, Tradeoffs, or Alternatives. The wire
shape and framework are unchanged; this revises only the cache primitive
and the reference-eviction trigger.
