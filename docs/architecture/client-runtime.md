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

That is the model Cockpit drives `phux-client-ffi` with: a consumer that
owns its socket feeds frames directly and needs nothing below this line.

`connection::run_session` is the driver. It dials the `Target` over its
lane (Unix socket, WebSocket, or QUIC through `phux-dial` and this crate's
`dial` planners), writes what the plane queues, feeds what arrives, and
on a drop walks the `reconnect::Ladder`; `is_fatal_refusal` ends the session
instead. A resync redials at once, a nudge cuts a backoff short and probes
an attached socket, and the never-attached phase is bounded by attempts
and wall clock so a caller can derive its wait from `ConnectOptions`.

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
  many frames land; `take_events` re-arms it).
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
