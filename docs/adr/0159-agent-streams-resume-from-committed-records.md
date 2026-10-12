---
audience: contributors
stability: stable
last-reviewed: 2026-10-11
---

# 0159 — Agent streams resume from committed records

**TL;DR.** Reconnecting AgentSession subscribers request records after their
last committed sequence. A capability-gated attach cursor and an explicit
bootstrap resume anchor distinguish appending a complete suffix from replacing
the retained ring. Eviction falls back to replacement. The kernel owns the
cursor; both FFI projections carry the existing append-or-replace distinction.

Status: Accepted
Date: 2026-10-11

## Context

[ADR-0103](./0103-agent-session-resource-and-producer-fed-streams.md) gives each
AgentSession a bounded producer-fed record ring and an atomic bootstrap cut.
Workspace subscriptions automatically follow those sessions, but a reconnect
replays the whole ring, up to the default 4 MiB per session. Transcript records
make that repeated transfer expensive even when the subscriber already holds
every record. A byte-budgeted tail would reduce transfer by silently dropping
records; reconnect needs a continuity contract instead.

## Decision

1. **Resume after committed records, not received packets.** The session kernel
   retains its committed log, derived state, and generation watermark across a
   same-incarnation transport loss. Resource close, explicit release, or a new
   server incarnation clears them. The runtime queries this owner; bindings do
   not maintain competing sequence cursors.
2. **Additive, capability-gated wire shape.**
   `features_ext.AGENT_STREAM_RESUME = 0x00000008` permits `ATTACH_RESOURCE`'s
   trailing `agent_after_seq: u64`, after its role byte. A missing role is
   encoded as the canonical default byte when a cursor follows. No-cursor
   commands keep their old bytes. `BOOTSTRAP_BEGIN` field 9 `resume_after_seq`
   marks an accepted cursor, using the existing agent JSONL/raw codec; it is
   forbidden on Terminal profiles. Per
   [ADR-0061](./0061-capabilities-add-versions-break.md), `PROTOCOL_VERSION`
   remains `0.9.0` rather than breaking the fleet for an optional extension.
3. **An atomic suffix or explicit replacement.** At its cut the actor selects
   records newer than the cursor only when the entire suffix is retained.
   Equality with the cut is an empty successful resume. An evicted or future
   cursor gets the ordinary full bootstrap with field 9 absent. Live output
   starts after the cut, and reattach retires the old pump first.
4. **Publish only at READY.** The kernel checks the anchor against its still
   committed watermark and validates contiguous records through the cut before
   appending. A bad generation cannot mutate the old log, state, or cursor.
   Full bootstrap replaces, including an empty ring; resume appends, including
   an empty suffix. C ABI and UniFFI retain their existing record kind shapes:
   retained means replacement, incremental means resumed or live records.
5. **Relays do not manufacture missing prefixes.** A hub forwards a cursor
   only to a supporting satellite and only when the upstream cut is not shared
   with consumers whose anchors are different or unknown. Otherwise it asks
   for a full replacement, safe for every subscriber. AgentSession bootstraps
   stay on the control lane rather than acquiring Terminal QUIC stream state.

## Why

The record counter already defines continuity. The actor's retained ring is a
contiguous suffix, so selection uses its cut and length rather than parsing or
copying an entire ring just to discard a prefix. An echoed anchor makes an
empty resume distinguishable from an empty replacement. Keeping validation and
cursor ownership in the kernel makes socket, workspace, C, and UniFFI consumers
share one reconnect decision.

## Tradeoffs

A subscriber disconnected beyond retention still pays for a full replay and
loses history the server no longer holds. Shared satellite streams may also
fall back to full replay rather than allocating separate retained logs or
upstream subscriptions per consumer. Consumers must preserve their committed
prefix until an explicit replacement or close, not infer replacement from a
new packet or bootstrap id. Old servers and no-cursor clients keep full replay.

## Alternatives

**Byte-budgeted replay:** rejected because a smaller tail cannot prove that
records between the client's watermark and that tail were delivered.

**Binding-owned cursors:** rejected because packet arrival is not atomic
bootstrap commitment, and two FFI lanes would duplicate eviction and
incarnation rules.

**New codec or protocol minor:** rejected because codec bytes do not change;
only bootstrap publication mode does. The feature bit and optional field
express that without a synchronized fleet upgrade. Normative fields and bounds
live in [L1 §4.8](../spec/L1.md#48-agenteventsjsonlv1--the-producer-fed-stream-codec).
