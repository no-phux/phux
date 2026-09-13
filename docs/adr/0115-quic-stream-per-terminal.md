---
audience: contributors
stability: stable
last-reviewed: 2026-09-12
---

# 0113 — One QUIC stream per Terminal plus a control stream

**TL;DR.** Over QUIC, a connection stops being one bidi stream carrying
every frame and becomes one **control stream** (HELLO, COMMAND, attach,
lifecycle, keepalive) plus one **bidi stream per attached Terminal**
carrying that Terminal's output, bootstrap, history, and input. UDS,
ssh-stdio, and WebSocket keep the single-stream shape. Gated on a
`ServerFeature` bit per ADR-0061, so old peers keep working unchanged.
This collects the QUIC prize ADR-0007 named: per-pane flow control from
the transport, no cross-pane head-of-line blocking, and migration that
keeps every Terminal stream alive.

Status: Accepted
Date: 2026-09-12

## Context

Every transport carries identical framing over exactly one reliable
ordered byte stream (proto.md §4–§5). QUIC was adopted per ADR-0007 for
migration, 0-RTT, and mandatory TLS 1.3 — and one-stream-per-connection
uses only the third while wasting what QUIC sells: a flooding pane
shares the single ordered flow with every other pane, every bootstrap,
and every control reply, and the application rebuilt fairness above the
transport (bounded queues, round-robin chunking, gap resync) that QUIC
streams would provide for free. Meanwhile `QuicWriter` already tracks a
per-stream congestion window (`transport/quic.rs`) — the backpressure
machinery exists, multiplied by one stream.

Single-stream was the right v0: one code path, one sequence space. Its
cost is invisible for one-phone attaches and permanent for a remote TUI
with many panes, which is the normal case.

## Decision

**Stream 0 — control.** Client-opened first, exactly as today (token
preamble, then frames). Carries everything that is not Terminal-content:
`HELLO`/`HELLO_OK`, `COMMAND`/`COMMAND_RESULT`, `ATTACH`/`ATTACHED`/
`ATTACH_READY`, `DETACH`/`DETACHED`, `PING`/`PONG`, `ERROR`,
session-scoped lifecycle (`RESOURCE_SPAWNED/CLOSED/MOVED`, `BELL`,
`EVENT`). Request/response correlation stays connection-wide via
`request_id`; the command envelope does not change.

**One bidi stream per attached Terminal.** Carries that Terminal's
`RESOURCE_OUTPUT`, `BOOTSTRAP_*`, `HISTORY_*`, `FRAME_ACK`, and its
`INPUT_*` frames. Input and output share the stream deliberately:
per-pane causal order (keystroke before its echo) falls out of stream
order instead of being reconstructed across two channels.

**The client opens every Terminal stream** (`open_bi`), including for
panes learned from an `ATTACHED` snapshot; the server never pushes.
First bytes are a `STREAM_BIND { terminal_id, stream_id }` header (a
transport-establishment detail like the token preamble, not a phux
frame); the server answers with that generation's `BOOTSTRAP_BEGIN` on
the same stream, or an uncorrelated `ERROR` on control plus a stream
reset for an unknown id. Stream close is a detach signal, not an error.

**Generation mapping.** ADR-0070's `(terminal, stream_id, bootstrap_id,
seq)` already names one logical subscription with replaceable
generations. The app-level `StreamId` stays — it is *not* the QUIC
stream id, which encodes initiator/type bits and is unstable across
reconnects. Rebinding a `StreamId` to a fresh QUIC stream after
migration is the reconnect path §4.6 already specifies.

**Gating (ADR-0061).** New `ServerFeature` bit `QUIC_STREAMS`. No bit,
no second stream; old peers never notice. UDS, ssh-stdio, WebSocket,
WebTransport keep the single-stream shape permanently — a local socket
has no head-of-line problem worth a second stream.

**Relay.** Its "never parses frames" contract (proto.md §4.1) survives
and simplifies: stream 0 loses its special status. The relay accepts
each consumer-opened stream and splices it to a fresh `tunnel.open_bi()`
— what `bridge_consumer` does today for the first one
(`crates/phux-relay/src/runtime.rs`). Per-connection stream count is
capped on both legs; over cap closes the connection, still with no phux
frames emitted.

**Migration.** QUIC migration keeps all streams: the path changes, the
connection and every Terminal stream survive, output resumes with at
most a gap the tombstone machinery already heals. No new wire needed —
this is the property we finally collect by using streams at all. 0-RTT
stays future work.

## Why

- Per-pane transport flow control replaces the coarsest layer of
  application fairness with the mechanism QUIC implements, at the layer
  where congestion information lives. Gap resync stays as the
  slow-consumer answer, per stream.
- Per-pane causal input→echo ordering becomes structural.
- The generation/tombstone machinery (ADR-0070) already names the
  subscription abstraction; this gives it a transport realization
  instead of inventing a second one.

## Tradeoffs

- Each Terminal stream is a `QuicWriter` plus reader tasks on each end;
  setup does N opens. Bounded by panes per attach; QUIC streams are
  cheap by design.
- Single-stream stays as the fallback, so the server keeps both QUIC
  shapes until the floor moves. Contained at the
  `FrameReader`/`FrameWriter` seam: multi-stream presents one control
  reader/writer plus a stream table, and the per-connection mailbox
  becomes a demux.
- `STREAM_BIND` needs `ADMISSION_DEADLINE` discipline so a half-opened
  stream cannot park the accept loop; unknown/unauthorized binds are
  refused on control, never silently dropped.
- Control frames still share stream 0 — a giant `GET_STATE` can stall a
  `DETACH` behind it. Accepted: control traffic is small, and
  per-Terminal bulk (the actual problem) leaves stream 0 entirely.

## Alternatives

- **Stay single-stream.** Rejected: leaves QUIC's headline features
  uncollected while we maintain a hand-rolled copy of one of them.
- **Server opens Terminal streams on ATTACH.** Rejected: the server
  cannot know which panes the client will watch
  (`ATTACH_RESOURCE`-only observers watch one pane); push opens streams
  nobody reads.
- **Unidirectional streams, input on control.** Rejected: splits
  per-pane causal order and puts latency-sensitive input back on the
  shared stream.
- **One stream per session.** Most of the implementation cost, a
  fraction of the isolation. Rejected.

## Related

- ADR-0007 — the mosh-class prize this collects; transport-as-trait
  contains the second QUIC shape.
- ADR-0070 — the generation identities this maps onto the transport.
- ADR-0061 — the capability-bit gating discipline.
- ADR-0051/0057 — the relay contract that survives (stream-pair
  forwarding, zero frame parsing).
- `docs/spec/proto.md` §4, §8; `docs/spec/L1.md` §4 — normative updates
  follow in the spec bead.
