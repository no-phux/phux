---
audience: contributors
stability: stable
last-reviewed: 2026-09-10
---

# 0097 — Durable coordinator is a separate bounded endpoint

**TL;DR.** Durable work uses an independently versioned, authenticated
`phux-coordinator/1` endpoint with opaque identities, fenced authority epochs,
idempotent operation results, bounded snapshots, and credit-controlled events.
It references Terminals without carrying terminal output or input. Clients that
only speak the terminal protocol remain compatible, and missing or downgraded
coordinator capabilities fail closed rather than falling back to L1 or L3.

Status: Accepted (forward-compat)
Date: 2026-09-10

## Context

[ADR-0092](./0092-durable-work-coordinator-authority.md) assigns durable
Objectives, Runs, WorkSessions, Artifacts, Signals, bindings, event order, and
evidence to one coordinator, but leaves its endpoint and recovery contract open.
ADR-0092 is still Proposed; this ADR accepts the endpoint and recovery contract
that any coordinator admitted under it must satisfy.
[ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md), as
generalized by [ADR-0102](./0102-resources-the-server-serves-kinds.md), closes
terminal synchronization to structured product state; [ADR-0061](./0061-capabilities-add-versions-break.md)
makes `major.minor` mismatch a hard refusal and additive growth capability-gated.
Putting work frames in L1 violates the first rule; spending its version on
coordinator evolution violates the second. The new plane must also survive lost
replies, restarts, stale restored writers, slow subscribers, and retention gaps
without duplicate work, unbounded memory, or split-brain authority.

## Decision

### Endpoint and compatibility

The coordinator is a distinct application endpoint selected before decoding:
service `phux-coordinator`, label `phux-coordinator/1`, and an independent
`0.1.0` version/capability namespace. v0.1 is paired-only, including its
dedicated owner-only Unix socket; QUIC uses the label as ALPN. SSH/stdin and
proofless local modes are refused. Terminal streams never multiplex coordinator
frames. Endpoint absence permits terminal-only operation, never authoritative
work state in L1, L3, `AgentEvent`, or a client.

### Identity and authority

Every durable object/operation ID is opaque nonzero 16 bytes. `CoordinatorId`
survives restart; `IncarnationId` changes each start. Each activation advances a
checked durable epoch and acquires a short-lived certificate from an external,
non-clonable witness that durably enforces the last authority epoch and one
fencing token per CoordinatorId. Its signed certificate is authenticated in the
handshake, and every request guards on it. The store lock excludes only within
one image; a copied/restored store cannot bind or commit without the witness
lease. Lease loss fences the writer; restore waits for revocation or expiry of
the prior lease.

### Authentication and scopes

Every connection uses [ADR-0098](./0098-workload-proof-and-closed-scope-authority.md)'s
endpoint-neutral `phux-workload/v1` profile in
[workload-auth.md](../docs/spec/workload-auth.md), domain-separated by service
`phux-coordinator`. Principal, authority proof, fresh nonce, expiry, and scopes
are connection-bound; terminal pairing or reachability grants nothing. Scopes
are a closed bitset for work read, work command, terminal control, artifact read,
Signal acknowledgement, and administration. Unknown bits are refused. The
server intersects an authenticated unexpired grant and rechecks it before each
mutation or delegated action. No credential or secret enters argv, env,
diagnostics, or logs.

### Commands and results

Every mutation carries `OperationId`, authority guard, and typed revision guards.
Idempotence is keyed only by `(CoordinatorId, OperationId)`; cross-principal
reuse conflicts, as does a changed semantic digest. Admission and result state
are transactional. A
delegated terminal effect also has a durable outbox `EffectAttemptId`; a target
with stable same-payload dedupe returns one result. If target dedupe cannot prove
the outcome after a crash, the operation becomes `INDETERMINATE` and is never
redispatched automatically. Lost replies use lookup and the same ID, never a
blind replacement. Bounded result tombstones outlive state they created.

### Snapshots, events, and bounds

Only a complete whole-authority, scope-filtered snapshot may reserve a v0.1
subscription. Snapshot/replay cursors are opaque, at most 4 KiB, and bound to
principal, authority guard, and a hard-lived lease. One visibility predicate
governs snapshots, events, replay, and lookup. Count/byte credit and
server/principal quotas bound every cut and queue before allocation. GAP pauses
deltas; a digest-verified replay plus ACK resumes the same subscription, or a
fresh snapshot replaces it. Silent eviction is forbidden.

### Terminal binding

A WorkSession binds through immutable `BindingId` to authenticated terminal-owner
authority, incarnation, and exact L1 `ResourceId` of a Terminal-kind resource.
Replacement creates a new binding; `ResourceId`, PID, title, cwd, host text,
layout, `StreamId`, and `BootstrapId` are never durable work identity. Sourced
facts admit exact-next owner sequence, replay only byte-identical duplicates, record explicit gaps, and
refuse after binding end. Terminal output, input, leases, signals, bootstrap,
and history remain on the terminal endpoint. The full contract is
[coordinator.md](../docs/spec/coordinator.md).

## Why

A separate endpoint preserves ADR-0030 exactly where it is load-bearing: a
terminal-only peer never learns work vocabulary and terminal synchronization
never becomes a product event bus. Independent versions and permanent
capabilities apply ADR-0061 without turning every coordinator addition into a
terminal fleet break. Durable operation records make an ambiguous network reply
safe, while epoch fencing and immutable binding history keep restart and restore
from manufacturing false continuity.

Credit on both count and bytes is necessary: either dimension alone admits an
unbounded queue of tiny events or a single oversized event. Complete snapshot
cuts plus explicit GAP make every client state either provably current or
visibly stale; there is no plausible-looking partial state.

## Tradeoffs

- Clients that use both surfaces maintain two authenticated connections and two
  version/capability state machines.
- The store carries bounded operation/outbox tombstones, cuts, and retention
  metadata in addition to work facts.
- Availability depends on a small external fencing witness; it holds activation
  sequence/token only, never work state or coordinator policy.
- Indeterminate non-deduplicated target effects require reconciliation rather
  than unsafe automatic retry.
- Fixed aggregate bounds may refuse legitimate load; callers split records or
  store bulk Artifact content instead of raising local limits.
- Initial federation remains one home lineage plus sourced terminal facts.

## Alternatives

**Add a work tier to the terminal protocol.** Rejected: it contradicts
ADR-0030, makes terminal-only clients parse product state, and couples unrelated
upgrade domains.

**Store work in L3 metadata.** Rejected: last-write-wins metadata cannot provide
transactional operation deduplication, immutable order, authority epochs,
complete snapshots, or evidence gaps.

**Let Cockpit coordinate locally.** Rejected by ADR-0092: it creates a second
writer and loses authority whenever one UI is absent.

**Use unbounded streaming over a reliable transport.** Rejected: reliability
does not bound producer/consumer skew, retained snapshot state, or allocations,
and cannot signal application-level retention gaps.
