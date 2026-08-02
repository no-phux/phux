---
audience: contributors
stability: stable
last-reviewed: 2026-08-01
---

# 0066 — Native engine-state bootstrap and client-owned history

**TL;DR.** Native phux clients bootstrap from exact, versioned libghostty state
instead of server-synthesized VT. The server sends state through the engine's
`READY` boundary, then releases queued raw PTY bytes while history continues in
pages. The server retains canonical history; clients own local replicas,
viewports, selection, and historical reflow. One PTY retains one active grid.

Status: Accepted
Date: 2026-08-01

## Context

phux already runs libghostty on both ends. The server owns the PTY and canonical
`Terminal`; clients feed `TERMINAL_OUTPUT` bytes into local `Terminal`s. This
avoids cell diffs and preserves the byte-faithful live hot path
([ADR-0013](./0013-libghostty-bytes-on-wire.md)). Attach and resync are the
exception: the server walks its grid and synthesizes VT whose fidelity is below
the engine state it replaces. Scrollback delays interaction, and parser,
graphics, and history state cannot all be reconstructed from the public grid.

libghostty's binary snapshot grammar has a
`TERMINAL → SCREEN/PAGE → READY → HISTORY/PAGE → FINISH` lifecycle. At `READY`,
a decoded terminal can render and resume parsing raw VT; history is a separable
suffix. That permits visible-first attach without creating a phux state format.

The design must preserve durability while detached, local scroll/search/select,
one real PTY winsize, and engine delegation
([ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md)).

## Decision

### Explicit profiles and versions

HELLO selects exactly one synchronization profile:

- **`NativeState`**: exact libghostty checkpoint bootstrap, raw PTY live output,
  and engine-owned history pages.
- **`SynthesizedVtV1`**: existing synthesized snapshot and StateSync/downsampled
  behavior for legacy, constrained, and temporarily non-native clients.

A server never silently falls back after selecting `NativeState`. If the peers
share no native codec, they explicitly select `SynthesizedVtV1` or fail. The
browser uses compatibility mode until it can run the same decoder through WASM;
WebSocket is transport, not a third state model.

This is a `0.7.0` clean cutover. `TERMINAL_SNAPSHOT = 0x91` is retired and never
reassigned. ADR-0061 normally favors additive capabilities, but the old attach
ordering must be removed, so `0.6.x` and `0.7.x` peers reject each other. Every
stateful connection performs HELLO, including same-UID Unix-socket clients;
`PING` alone may remain stateless.

The phux version and libghostty codec version are independent. HELLO advertises
an engine codec range and feature mask; HELLO_OK selects one exact immutable
codec version and usable feature intersection. Missing required engine features
reject native negotiation.

### Opaque engine bytes

libghostty alone produces and consumes native state. phux frames only identity,
selected codec, generation, live watermark, bounded chunks and integrity,
checkpoint id, opaque history cursors, acknowledgements, limits, and tombstones.
It never parses an engine record, depends on `Page` layout, or synthesizes codec
bytes. Compressed pages may stay compressed; allocator metadata, pointers,
virtual mappings, padding, and native alignment never enter the portable codec.

### READY-fenced attach

A native attach has one actor-owned ordering:

1. After HELLO and `ATTACH_TERMINAL`, the Terminal actor applies all prior PTY
   bytes, increments the generation, records the next live sequence, begins an
   engine checkpoint capture, and queues later bytes for that consumer.
2. The server streams bounded opaque chunks through libghostty's `READY` record.
3. The client incrementally decodes into staging. Nothing partially decoded is
   visible to the frontend.
4. At engine `READY`, the client atomically publishes the live terminal. It may
   render, accept input, and use cached history immediately.
5. The client acknowledges checkpoint, generation, and live watermark. Only
   then does the server release queued `TERMINAL_OUTPUT`; history may continue
   concurrently on its own stream.

The PTY is not paused for network delivery. Only the in-actor cut is
pause-sensitive. Capture holds a bounded immutable lease, pin, or copy-on-write
view so later output/compression cannot mutate bytes in flight. A transport
that subscribes first and requests a snapshot from another task has a gap and
is invalid; UDS, WebSocket, relay, and future QUIC consume the same actor stream.

The post-cut queue is bounded by bytes and age and never silently drops. Queue
overflow, bad integrity, wrong-generation ACK, sequence gap, or stale cursor
produces an explicit tombstone/resync requirement. The client keeps its last
published terminal until a replacement reaches READY. Reconnect resumes only
when server incarnation, TerminalId, checkpoint/generation, and last contiguous
sequence prove continuity; otherwise it takes a fresh cut.

Federation forwards opaque chunks and logical stream identities, and proxies
ACK/history demand to the origin. It never decodes engine state. Native and
compatibility profiles are separate upstream subscriptions.

### Durable server history, client-owned projection

The server retains canonical bounded history so detach, late join, recording,
and recovery work with no connected client. Each client owns a local, initially
incomplete replica plus its viewport, cache, selection, and search state.

After READY, history moves newest-to-oldest in bounded, independently verified
engine pages. Clients request, pause, or cancel by opaque cursor; explicit
scroll demand outranks prefetch. Servers return explicit beginning,
evicted-cursor, or stale-generation results, never a substituted range. Pages
are generation-bound and loading them cannot mutate the active area, cursor,
parser continuation, or live sequence.

Client caches are bounded by bytes. Selection/search use semantic engine anchors,
not viewport row offsets, so page insertion and eviction do not invalidate them.
Unlimited disk retention and deferred soft-wrap reflow remain libghostty work;
phux supplies demand, cancellation, priority, and resource budgets.

### One PTY geometry, many projections

One process and PTY retain one authoritative `(cols, rows)`, chosen by existing
server resize policy ([ADR-0027](./0027-terminal-references-and-l3-links.md),
[ADR-0062](./0062-headless-resize-and-window-size-policy.md)). Every native
client reconstructs that active grid. Clients independently choose physical
window size, zoom, crop/letterbox, layout, local scroll offset, and how much
history to show. When supported by libghostty, clients may reflow finalized
history locally. None of this changes the child process's winsize.

## Why

First render becomes proportional to the active prefix rather than all retained
history, while exact parser and graphics continuation replace an irreducibly
lossy grid replay. Live output remains raw PTY bytes instead of turning every
frame into a state diff.

"Client-owned scrollback" means local interaction with a cached projection; it
does not mean the disconnected client is the only durable copy. The READY fence
then separates invisible staging from an atomically renderable terminal and
prevents live bytes overtaking the state they extend.

## Tradeoffs

- Native mode requires compatible codecs on both ends; updating libghostty is an
  explicit compatibility operation.
- Capture retains or copies pages while output continues. Slow clients may be
  tombstoned rather than allowed to pressure the Terminal actor.
- Server and client duplicate cached history to buy detach durability and local
  interaction.
- Two profiles increase conformance surface but make fidelity loss explicit.
- Independent live grids remain impossible for one PTY; historical reflow does
  not make cursor-addressed TUIs responsive to two logical winsizes.

## Alternatives

**Page synthesized VT only.** Better latency, same fidelity ceiling. Retained as
compatibility, rejected for native clients.

**Structured cell diffs.** Duplicates engine state on the wire and loses
engine-only state. Rejected by ADR-0013 and ADR-0030.

**Ship `Page` memory.** Pointer, allocator, mapping, compression, and layout
state is not portable. Rejected in favor of the engine codec.

**Keep history only on clients.** Breaks detached and late-join recovery.
Rejected.

**Pause the PTY through bootstrap.** Simple ordering, unbounded producer latency.
Rejected in favor of a bounded queue after a short coherent cut.

**One active geometry per client.** One child sees one winsize and emits one
cursor-addressed stream. True independent grids require separate PTYs/processes
or application-semantic output, not one terminal multiplexer. Rejected.

## Related

- ADR-0013 — libghostty bytes on wire.
- ADR-0027 — terminal identity, one geometry, many views.
- ADR-0030 — engine-delegated wire and projections.
- ADR-0032 — server incarnation and graceful upgrade.
- ADR-0061 — capability additions versus version breaks.
- ADR-0062 — explicit resize and window-size policy.
