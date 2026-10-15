---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-14
---

# The client runtime

**TL;DR.** `phux-client-runtime` connects the sans-IO session kernel to
language bindings (ADR-0133). Its synchronous `ControlPlane` handles decoded
frames; an async `Connection` owns dialing, framing, keepalive, and reconnect.
libghostty stays on an owner thread that publishes immutable grid frames.
Bindings call `Client`, drain events, and acquire frames without owning a
connected-client state machine.

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
- `take_events()` returns an owned batch of `Event`s, and
  `take_observation()` returns the same batch together with the status, the
  last error, the topology and the `connection_epoch` sampled in the same
  step. A polling binding uses the second form, because separate reads can
  straddle a reconnect. Every transport pushes a lossless
  `Event::ConnectionOpened { connection_epoch }` boundary. On overflow the
  queue keeps only the newest boundary and reports `events_dropped`;
- a `ControlError` says how a frame ended the connection: `Protocol`
  (drop and redial), `Refused` (terminal), `Resync` (redial for fresh
  snapshots), `Closed` (the consumer asked).

On an L3 server the plane also subscribes to, then reads, two keys for every
inventoried terminal, and re-reads them after an event gap, an event-queue
overflow or a reconnect. Those keys are `phux.agent/v1` (`Event::AgentMetadata`)
and the server-owned `phux.agent.asked/v1` flag (`Event::AgentAskedState`). A
read issued before a live change is fenced by that change. `AgentAsked`
announces a question. `AgentAskedState { asked: false }` is the level that
retracts it, so a question cleared while a client was not listening is not
shown again.

With `ControlOptions::subscribe_agent_sessions` (phux-mobile's lane), the
plane streams every `AgentSession` resource (ADR-0103) whose parent Terminal
the connection streams. `ATTACHED`, `GET_STATE`, and successful per-terminal
attaches reconcile the listed sessions. New sessions are declared to the
kernel and subscribed with `Command::attach_agent_session` (no resize); a
refused subscription is retried by the next read. Terminal callers use
`Command::attach_terminal`, which cannot carry an agent cursor. Both
constructors preserve the existing `ATTACH_RESOURCE` wire shape.

Core's `session::agent_stream::lifecycle` owns decoding, staging, validation,
folding, committed continuity, and retirement for each agent stream. A commit
produces `KernelEffect::AgentPublication`: resource identity, typed stream and
bootstrap identity, committed sequence watermark, `Replace` or `Append`, and
validated records. Successful READY emits a publication even with no records.
An empty replacement clears the log; an empty append advances generation
metadata without changing the log. Invalid staging emits no agent publication.

The runtime's `control::agent_delivery` owns delivery, not the bindings.
`Event::AgentPublication` carries one `Arc` allocation for the committed record
vector and, for runtime-owned subscriptions, the catalog's parent/provider
identity. Ordinary `take_events` and `take_observation` accept ownership on
drain. `take_events_leased` retains the
allocation until `accept_agent_publication` accepts that exact pointer.
Transport loss or replacement parks unaccepted publications, including those
already leased. Replay waits for current resource admission and attach
readiness, follows the catalog snapshot, and precedes later committed records.
An authoritative catalog omission, close, or release discards that resource's
pending publications and cursor. A new server incarnation discards all of them.
`AgentSessionClosed` carries the last committed stream stamp captured before
the kernel forgets the stream; it is not a `TerminalClosed`.

Same-incarnation reconnects preserve the kernel's committed log, derived state,
and sequence watermark. With `AGENT_STREAM_RESUME`, the plane requests only
the suffix after that watermark; evicted cursors and older servers receive a
full replacement
([ADR-0159](../adr/0159-agent-streams-resume-from-committed-records.md)).
Unpublished staging never advances the cursor. Engine reuse compares the
effective history policy: an explicit binding policy overrides the attach's
default scrollback depth, so an unused default cannot erase a resume anchor.

The C ABI leaves automatic agent subscription off because its owning-thread
workspace catalog selects subscriptions and owns their request IDs. Pending
and confirmed wire requests establish subscription state; replayed stream
metadata does not. The shared FFI projection lowers publications to existing
C effects and UniFFI record events without reconstructing commit policy from
packets. Empty append effects may be suppressed, but their stream stamp is
still adopted. `phux_client_effect_clear` accepts C publications. Ending a
borrow on another mutable call is not acceptance. Transport retirement drops
staged views without accepting their publications.

In `Runtime::embedded`, the consumer owns the socket and feeds frames directly.
Harnesses use this lane for synthetic frames, as do the C ABI's
`phux_client_new` clients.

`connection::run_session` is the driver. It dials the `Target` over its
lane (Unix socket, WebSocket, or QUIC through `phux-dial` and this crate's
`dial` planners), writes what the plane queues, feeds what arrives, and
on a drop walks the `reconnect::Ladder`; `is_fatal_refusal` ends the session
instead. A Unix-socket target left on the interactive default uses the
local-upgrade ladder. A session that was attached for at least a second
redials before that sleep. A resync redials at once, a nudge cuts a backoff
short and probes
an attached socket, and the never-attached phase is bounded by attempts
and wall clock so a caller can derive its wait from `ConnectOptions`.
An attached session with no inbound bytes for 10 seconds sends that same
probe. On a byte stream, a send that cannot place another 16 KiB within 15
seconds is a drop, so a shut window cannot park the driver ahead of the
probe, and a slow link that keeps accepting data stays up. A WebSocket
message gets 15 seconds per 16 KiB, capped at 60 seconds, because that
message is one send.
Every acknowledged submission (`apply_line`, `apply_paste`,
`apply_tab_completion`) that waits on the wire nudges too, so a half-open
link is dropped within `probe_timeout` and its receipt follows the
reconnect, instead of waiting for an idle timeout.

## Where a connected binding takes delivery

`ControlOptions::deliver_inbound` decides what the driver does with a frame
it read. `Fed`, the default, applies it to the plane on the driver's thread;
that is phux-mobile's lane, and the consumer observes only events and the
published grid.

`Queued` retains it instead. `Client::take_inbound` atomically drains
`(connection_epoch, frames)` for the consumer to feed on its owning thread.
The consumer checks `ControlPlane::accepts_inbound(epoch)` under the same
control lock used to feed each frame: transport loss can invalidate a batch
after it was drained, even before a replacement socket opens.
`phux-client-ffi` takes this lane because its per-frame behavior — retired-close
suppression, bootstrap-profile validation, and workspace reconciliation — reads
binding-owned catalog state only its owning thread may touch (ADR-0133
decision 6). It processes lifecycle events before decoding the batch, retiring
pending workspace reads, subscriptions and attach bookkeeping while keeping
the last-good publication available for frozen display, not input authority.

The queue is bounded at `MAX_QUEUED_INBOUND_FRAMES` and
`MAX_QUEUED_INBOUND_BYTES`; overflowing either fails the connection.
Loss, failure and replacement discard undrained frames. Already-drained
frames remain subject to the epoch fence.

Automatic attach intent also respects server incarnation. A numeric session
target survives a reconnect to the same server; a replacement server is
addressed by the previously confirmed session name instead. Without that
name, recovery refuses rather than attaching a recycled number or creating
a session. An attach refusal ends the attempt instead of leaving it waiting
for `ATTACH_READY`.

## Thread model

Three kinds of thread touch a session; callers need no thread affinity.

- **The owner thread** (`engine::EngineHandle`) hosts
  `SessionKernel<GhosttyAdapter>` and every replica. Ghostty is `!Send`,
  so only owned values cross: `EngineEvent`s in over a channel,
  `EngineOutcome`s (the kernel's declarative effects) back. A single event
  publishes before answering; `ControlPlane::apply_engine_events`
  applies an ordered pump batch, accumulates damage, and projects each damaged
  presentation a consumer has caught up with once before the ordered outcomes
  return to the plane (see [the publication contract](#the-publication-contract)
  for the ones nobody has read yet). A fatal
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
  from the `Publication` table without touching the plane at all; an
  acquire that must catch up waits on one owner-thread round trip, so no
  owner-thread code acquires.

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
  generation, and every full projection, marks all rows dirty, as does the
  first projection of a presentation after another presentation of the same
  terminal was projected in between.

Publication is paced by reads, with no timer. After a batch the owner
projects a damaged presentation (a terminal's default one or a view) only
when a consumer has acquired its current frame; otherwise the frame it
would replace is unread, so the owner defers and marks the slot stale. The
next `acquire` of a stale slot asks the owner to project the latest state
and returns that frame, one owner round trip. So:

- the owner never projects faster than consumers read (an output flood read
  at display rate publishes at display rate, and a presentation nobody reads
  is projected once), and a consumer never receives a frame older than the
  replica was when it asked;
- a caught-up consumer's next change, a keystroke's echo included, publishes
  in the batch that applies it;
- `generation` only holds still while the current generation is unread, so
  a consumer that polls it still sees every change it has not painted, and
  the caught-up frame's dirty rows cover every deferred batch.

`runtime.publish_deferred`, `runtime.acquire`, and `runtime.catch_up` count
the three sides of that exchange.

The frame also carries the geometry, cursor, scrollbar, the colors the
cells were resolved against, and the replica identity (stream, bootstrap,
last sequence), so a renderer needs no engine type.

## Geometry intent and authoritative readback

Native hosts derive desired cells from the same measured cell metrics their
renderer uses. The runtime retains a full viewport report, including optional
pixels, across reconnect. Changes made during session attachment coalesce until
`ATTACH_READY`; an explicit same-size report reasserts intent rather than treating
an earlier submission as proof of application. C and UniFFI viewport entry points
use this control-plane path.

A session viewport vote affects its subscribed session terminals, including
nonactive panes, under the server's window-size policy. Foreign resource
subscriptions remain separate: the runtime issues exact per-terminal requests
only for ordinary subscriptions, not preserving or viewer subscriptions. Exact C
resizes use the runtime's current-subscription and bootstrap-readiness checks.
Acceptance means retained or queued intent, never an optimistic local grid update.

The server coalesces pending geometry in one latest-value slot per terminal,
separate from its bounded targeted-recovery queue. Queue pressure cannot discard
the final size, pixel donor, or owed live resync. The actor applies geometry before
new captures and publishes replacement bootstrap generations only when geometry
actually changes. Clients consume that authoritative readback; they do not retry
until their own preferred size wins over another client's policy vote.

The TUI retains QUIC's required per-terminal resize stream. Before following a
control-stream viewport vote with exact pane sizes, its connection waits for an
ordered `PING`/`PONG` barrier, retaining intervening control frames for the normal
receive loop. This prevents independent QUIC streams from reversing the vote and
the chrome-inset layout without changing the negotiated wire contract.

## What a consumer touches when the binding crate changes

A consumer of the mobile artifact re-pins with a `PHUX_REV` bump
(ADR-0135). When the binding crate's name, features, or examples change,
the consumer-visible scope also covers:

- **Artifact and module names.** `PhuxMobileFFI-*` assets, the `PhuxFFI`
  Swift module, the module map, and the provenance `format` keys are the
  consumer's contract; the library file names inside
  (`libphux_client_ffi.*`, `phux_client_ffi.kt`) follow the crate.
- **License inventory.** The consumer's notices are generated from the
  crate graph of the lane it ships: `phux-client-ffi` with
  `--no-default-features --features uniffi`, not the default C lane.
- **Fixtures and rigs.** Anything seeded from a crate example
  (`rig_seed`) or gated on a Cargo feature name in a script or a privacy
  gate names the binding crate and its features.

Such a change belongs in the PR body.

## Extension points

The runtime exposes extension points without requiring a second state machine:
`send_command` correlates any `COMMAND` and returns `Event::CommandResult`;
`queue_frame` sends any frame. Every inbound frame the plane does not consume
(metadata, directory listings, moves) surfaces exactly once as `Event::Frame`
for the binding's C-shaped projection. `ServerInfo` reports the negotiated
features a binding gates on. Bindings must not clone and independently
dispatch the same inbound frame around `ControlPlane::feed`.
