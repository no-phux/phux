---
audience: contributors
stability: stable
last-reviewed: 2026-08-02
---

# 0067 — Native engine-state bootstrap and client-owned history

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

### Protocol 0.7 clean cutover

This is a `0.7.0` clean cutover. `TERMINAL_SNAPSHOT = 0x91` is permanently
retired and never reassigned. Protocol `0.6.x` and `0.7.x` peers reject each
other at HELLO; no alias, old discriminant, or implicit same-UID path bypasses
the handshake. `PING` is the only frame permitted before HELLO.

The old snapshot-first ordering in ADR-0013, ADR-0015, ADR-0018, ADR-0025,
ADR-0034, ADR-0043, and ADR-0060 is superseded by this decision. Their raw
live-byte, client-owned projection, StateSync compatibility, image opacity,
federation identity, and one-PTY-geometry conclusions remain in force subject
to the profile and generation rules below. ADR-0061's additive-change default is
intentionally overridden because removing the old attach ordering is wire
breaking.

### Three explicit profiles

HELLO advertises a set and HELLO_OK selects exactly one of three profiles for
the connection:

| Profile | Bootstrap | Live bytes | `FRAME_ACK` | Color rewrite |
|---------|-----------|------------|-------------|---------------|
| `NativeState` | exact libghostty checkpoint | raw PTY | forbidden | forbidden |
| `SynthesizedVtRaw` | synthesized VT v1 | raw compatibility VT | forbidden | permitted by client caps |
| `SynthesizedVtStateSync` | synthesized VT v1 | synthesized per-consumer VT | cumulative | permitted by client caps |

The enum contains these three combinations rather than a free
`codec × OutputMode` product, so `NativeState + StateSync` is unrepresentable.
`OutputMode` remains the compatibility preference only. Native is preferred
when usable; otherwise a synthesized profile is selected only if both peers
advertised that exact combination. There is no silent fallback after
HELLO_OK. No shared usable profile produces fatal
`ERROR { code: CODEC_UNAVAILABLE = 6 }`.

Native codec/version negotiation is orthogonal to profile negotiation.
`NativeState` requires an exact codec intersection, not an inferred range.
Protocol 0.7 allocates `LibghosttyCheckpointV2` and requires the
`CONTINUATION`, `READY_BOUNDARY`, and `HISTORY_PAGES` engine feature bits.
HELLO advertises exact codec and feature sets; HELLO_OK names the exact selected
codec and negotiated feature intersection. Future checkpoint versions receive
new set bits and are never assumed compatible.

Each HELLO also advertises nonzero `max_chunk_bytes` and
`max_history_page_bytes`; HELLO_OK selects the per-axis minimum. Both are hard
capped at 8 MiB, with reference advertisements of 256 KiB and 1 MiB
respectively. Zero or a value over its hard cap is malformed. The negotiated
bound applies before allocation, in addition to the 16 MiB outer frame cap.
Opaque history cursors are capped at 4 KiB.

### Opaque records and identities

`StreamId` is a nonzero `u64` naming one logical terminal subscription on one
connection. `BootstrapId` is a nonzero `u64` naming one replaceable replica
generation within that stream. Zero is reserved; ids are never inferred from a
TerminalId or transport. Every bootstrap, history, live output, and StateSync
ack frame carries `TerminalId + StreamId + BootstrapId`.

Ghostty alone produces, validates, and consumes native checkpoint records.
phux owns only framing, profile/version negotiation, identities, actor cut
sequence, bounds, opaque cursors, and lifecycle. phux never scans checkpoint
magic, record tags, READY, FINISH, Page layout, allocator metadata, pointers,
padding, or native alignment; it never synthesizes native records. Native
`BOOTSTRAP_CHUNK`, `HISTORY_PAGE`, and subsequent raw
`TERMINAL_OUTPUT.bytes` pass byte-for-byte through every transport and relay.
In particular native raw bytes are never SGR/color/image rewritten.

### Inclusive actor cut and READY fence

The Terminal actor owns one checked, non-wrapping, actor-global `u64` stream
sequence stamped before broadcast. A new bootstrap is cut in one actor turn:

1. Drain the subscription receiver, then ask the actor for an inclusive cut.
2. The actor applies all PTY bytes through `base_seq`, increments the replica
   generation, captures authoritative `(cols, rows)`, and starts an immutable
   codec capture covering exactly `seq <= base_seq`.
3. The coordinator discards drained/subscribed duplicates `seq <= base_seq` and
   queues only contiguous `seq > base_seq`, bounded by bytes and age.
4. Send `BOOTSTRAP_BEGIN`, contiguous zero-based `BOOTSTRAP_CHUNK`s, then
   `BOOTSTRAP_READY` only after all engine bytes through the engine READY record
   have been emitted.
5. Reliable transport order is the publication acknowledgement: the client
   incrementally decodes into staging, atomically publishes when it consumes
   protocol `BOOTSTRAP_READY`, and the next frame is raw
   `TERMINAL_OUTPUT { seq: base_seq + 1 }`. There is no client bootstrap ACK and
   no extra RTT gate.

This fifth step refines and supersedes the earlier accepted wording that
required a client checkpoint ACK before releasing raw bytes. The dual engine
READY/protocol `BOOTSTRAP_READY` fence on a reliable ordered stream is
sufficient. `FRAME_ACK` remains cumulative only for
`SynthesizedVtStateSync`, scoped to `(terminal, stream, bootstrap)`, and is sent
after the acknowledged bytes have been applied to the published compatibility
terminal. Raw profiles never send it.

For session attach the server sends `ATTACHED`, then `BOOTSTRAP_BEGIN` for all
panes in stable snapshot traversal order, then emits bounded chunks round-robin
across panes. A pane's `BOOTSTRAP_READY` immediately opens that pane's live
queue; it does not wait for slower panes or history. `ATTACH_READY` echoes
`attach_id` once every pane in that attach is READY or closed. Input may flow
for a pane after its READY; metadata/history work never blocks its live writes.

### History, resume, and tombstones

After READY, retained history is client-pull, newest-to-oldest, and lower
priority than live output. A stream has at most one `HISTORY_REQUEST`
outstanding. Explicit scroll demand outranks prefetch; cancel is accomplished
by replacing/tombstoning the generation. `HISTORY_PAGE.cursor` echoes the
request cursor; `next_cursor` is the next older opaque cursor. Absent
`next_cursor` means the payload contains the selected codec's FINISH and reaches
the beginning of retained history. A page is generation-bound and loading it
must not mutate active screen, parser continuation, cursor, modes, or live
sequence. Cursor bytes and payload validity remain engine-owned.

Reconnect/resume is legal only when authenticated server incarnation,
TerminalId, profile, StreamId mapping, BootstrapId, and last contiguous live
sequence all prove continuity. Otherwise a fresh stream/cut is required.
Stale generation, evicted cursor, codec failure, resize, relay reconnect,
sequence gap, bounded queue overflow, or explicit reattach invalidates the old
generation with `BOOTSTRAP_TOMBSTONE` before a replacement
`BOOTSTRAP_BEGIN`. Reasons are `RawReplayOverflow`, `OutboundGap`, `Resize`,
`RelayReconnect`, `ExplicitReattach`, `CodecFailure`, and `Other`. After a
tombstone, no chunk, page, output, READY, or ACK carrying that BootstrapId is
legal; the client keeps its last published terminal until a replacement
reaches READY.

### Geometry, resource fairness, and federation

One PTY retains one authoritative `(cols, rows)` under ADR-0027 and ADR-0062.
Every bootstrap records that geometry. A viewport change that does not alter
authoritative PTY geometry leaves the generation intact. An authoritative resize
takes a fresh actor cut and tombstones affected generations with `Resize`;
clients keep the old published terminal until the replacement reaches READY.
Client window size, zoom, crop/letterbox, local scroll, selection, search, cached history, and
historical reflow remain local projections and never change child winsize.

Implementations bound capture leases, post-cut raw queues by bytes and age,
outbound frame queues, history work, cursor size, and client history caches.
Live writes outrank bootstrap chunks; READY-prefix chunks outrank history;
explicit history demand outranks prefetch. Multi-pane attach uses bounded
round-robin chunk turns so a large pane cannot head-of-line block a small pane's
READY. Overflow is a tombstone, never silent `try_send` loss.

Federation negotiates one profile per logical stream and maintains a bijection
`(downstream client, downstream StreamId) ↔ (upstream link, upstream StreamId)`.
Native and compatibility profiles use distinct upstream subscriptions. A hub
rewrites only phux TerminalId/StreamId/BootstrapId envelopes, proxies
StateSync ACK and history demand, and preserves order and bounds; checkpoint,
history, cursor, and native raw bytes remain opaque and byte-identical.
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
- Three profiles increase conformance surface but make fidelity loss explicit.
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
