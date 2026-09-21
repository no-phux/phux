---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-20
---

# The client runtime

**TL;DR.** `phux-client-runtime` is the one layer between the sans-IO
session kernel (`phux-client-core`) and a language binding (ADR-0133).
It is split along one seam: a synchronous, sans-IO `ControlPlane` that
turns decoded frames into outbound frames and owned events, and an async
`Connection` driver that dials, frames, keeps alive, reconnects, and feeds
it. The libghostty engine lives on its own owner thread and never crosses
one; consumers read the grid through immutable published frames carrying a
generation counter and dirty rows. A binding holds no connected-client state
machine: it calls the synchronous `Client`, drains `take_events`, and acquires
frames.

---

## Two layers, one seam

`control::ControlPlane` is the state machine. It owns the connection
lifecycle (`HELLO` to `HELLO_OK` acceptance through
`phux_client_core::handshake`, `ATTACH`, `ATTACH_READY`, `DETACH`), the
topology, per-terminal attach/detach/spawn/kill/close, raw input and the
acknowledged `APPLY_INPUT` path through `phux_client_core::input_replay`,
`SUBSCRIBE_EVENTS` and the folding of agent events, and error and refusal
reporting. It never touches a socket or a clock it is not handed:

- `feed(FrameKind)` and `feed_bytes(&[u8])` apply one inbound frame;
- `take_outbound()` returns the encoded frames to send, in order;
- `take_events()` returns an owned batch of `Event`s;
- a `ControlError` says how a frame ended the connection: `Protocol`
  (drop and redial), `Refused` (terminal), `Resync` (redial for fresh
  snapshots), `Closed` (the consumer asked).

That is the embedded lane, `Runtime::embedded`: a consumer that owns its
socket feeds frames directly and needs nothing below this line. It is how a
harness drives synthetic frames, and how the C ABI's `phux_client_new`
clients still work.

`connection::run_session` is the driver. It dials the `Target` over its
lane (Unix socket, WebSocket, or QUIC through `phux-dial` and this crate's
`dial` planners), writes what the plane queues, feeds what arrives, and
on a drop walks the `reconnect::Ladder`; `is_fatal_refusal` ends the session
instead. A resync redials at once, a nudge cuts a backoff short and probes
an attached socket, and the never-attached phase is bounded by attempts
and wall clock so a caller can derive its wait from `ConnectOptions`.

## Where a connected binding takes delivery

`ControlOptions::deliver_inbound` decides what the driver does with a frame
it read. `Fed`, the default, applies it to the plane on the driver's thread;
that is phux-mobile's lane, and the consumer observes only events and the
published grid.

`Queued` retains it instead, for `Client::take_inbound` to drain and the
consumer to feed on its own thread. `phux-client-ffi` takes that lane
because its per-frame behavior — retired-close suppression,
bootstrap-profile validation, agent-generation tracking — reads
workspace-subscription state only its owning thread may touch (ADR-0133
decision 6). The binding sheds the dialer, the ladder and the framing
without moving its decode point. The queue is bounded at
`MAX_QUEUED_INBOUND_FRAMES` and `MAX_QUEUED_INBOUND_BYTES`, mirroring the
bounds a socket-owning embedder enforced for itself; overflowing either
fails the connection, and `connection_opened` discards whatever the
consumer never drained, because those frames were built against the
connection that ended.

## Thread model

Three kinds of thread touch a session, and no consumer is told which one
to call from.

- **The owner thread** (`engine::EngineHandle`) hosts
  `SessionKernel<GhosttyAdapter>` and every replica. Ghostty is `!Send`,
  so only owned values cross: `EngineEvent`s in over a channel,
  `EngineOutcome`s (the kernel's declarative effects) back. A single event
  publishes before answering as before; `ControlPlane::apply_engine_events`
  applies an ordered pump batch, accumulates damage, and projects each damaged
  terminal once before the ordered outcomes return to the plane. A fatal
  outcome ends the applied prefix; later queued frames never mutate replicas.
  Generation and dirty-row facts therefore describe the batch's final
  authoritative state without paying one projection per output frame.
- **The runtime thread** runs one current-thread tokio runtime with the
  driver on it. Each read drains at most 256 complete frames already buffered
  by UDS, QUIC, or WebSocket, and the plane batches contiguous engine frames;
  a session/control frame flushes the pending engine batch first, preserving
  wire order. The thread shares the plane with callers through a mutex and
  never calls foreign code while holding it: the `Listener` wake fires after
  the lock is released, edge-triggered (one outstanding wake no matter how
  many frames land; `take_events` re-arms it). Dropping the last `Client` closes
  the session and **joins** this thread. That matters for a binding whose
  listener carries a consumer-owned context: closing alone would let a wake
  reach a context the consumer had already freed. The driver selects on the
  close signal and abandons an in-flight dial rather than running it to
  `dial_timeout`, so the join is bounded by the consumer, never the network.
- **Caller threads** hold a `Client` (an `Arc`; clone to share) and call
  synchronous methods from anywhere. Each takes the lock briefly and
  notifies the driver when frames were queued. Grid frames are acquired
  from the `Publication` table without touching the plane at all.

Without the `engine` feature a bounded `ByteAdapter` replica keeps the
same kernel and the same threads, so the transport and control-plane lanes
build and test with no Zig toolchain (phux-mobile's headless lane).

## The publication contract

`publication::Publication` holds one slot per terminal. The owner thread
projects into a back `GridBuffer` (core's `GridProjector`), swaps it out,
wraps it in an immutable `GridFrame`, and publishes it behind an `Arc`;
the buffer of the frame it replaced is recycled when no consumer still
holds it. A consumer:

- calls `acquire(terminal)` and keeps the frame as long as it likes; a
  later publish never touches it, and there is no "valid until the next
  call" contract anywhere;
- reads `generation`, which increases by one per publish of that terminal,
  and skips work when it has not moved; `TerminalPublication::generation`
  is one atomic load;
- reads `damage` and `dirty_rows()`, taken from libghostty's render state
  (`Snapshot::dirty`, `RowIteration::dirty`) and cleared after each
  projection, so a repaint can be incremental. The first frame of a replica
  generation, and every full projection, marks all rows dirty.

The frame also carries the geometry, cursor, scrollbar, the colors the
cells were resolved against, and the replica identity (stream, bootstrap,
last sequence), so a renderer needs no engine type.

## Extension points

The common surface is what both consumers need today. A later rung adds a
lane without a second state machine: `send_command` correlates any
`COMMAND` and answers it as `Event::CommandResult`; `queue_frame` sends
any frame; every inbound frame the plane does not consume (metadata,
directory listings, moves) surfaces exactly once as `Event::Frame` for the
binding's C-shaped projection; `ServerInfo` reports the negotiated features
a binding gates on. Bindings must not clone and independently dispatch the
same inbound frame around `ControlPlane::feed`.
