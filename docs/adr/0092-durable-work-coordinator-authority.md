---
audience: contributors
stability: stable
last-reviewed: 2026-08-18
---

# 0092 - The coordinator owns durable work

**TL;DR.** Durable Objectives, Runs, WorkSessions, Artifacts, Signals, bindings,
and evidence belong to a Phux coordinator, not to any client. Terminal owners
remain authoritative for processes, PTYs, output order, bootstrap generations,
input leases, and signals. Cockpit, the TUI, web, agents, and recorders are peer
consumers that issue commands and project coordinator state.

Status: Proposed
Date: 2026-08-18

## Context

Phux already owns the live execution substrate:

- one server owns a user's processes and PTYs while clients attach separately
  ([ADR-0003](./0003-server-process-model.md));
- `TerminalId` is the wire primary, independent of client layout
  ([ADR-0016](./0016-terminal-id-as-wire-primary.md));
- layout, focus, and grouping are consumer projections or L3 metadata
  ([ADR-0017](./0017-tui-not-protocol-privileged.md),
  [ADR-0049](./0049-client-local-focus-and-advisory-attention.md));
- the terminal owner arbitrates input and process signals
  ([ADR-0033](./0033-input-authority-and-process-signals.md)); and
- acknowledged input is idempotent only within a named server incarnation and
  bounded retry horizon ([ADR-0053](./0053-acknowledged-idempotent-input.md)).

Those contracts let a terminal survive a client disconnect. They do not yet
provide restart-stable work identity, authoritative event history, completed-run
evidence, or process survival across an unexpected server loss. The current
server keeps session state in memory; its on-disk PTY journal remains design
intent, not shipped behavior.

Phux Cockpit independently prototyped Objective, Run, Session, Artifact, Signal,
provider-binding, event-store, and blob-store contracts. That put coordinator
authority in one UI client and proposed a second local PTY daemon beside Phux.
Two authorities for the same work would make reconnect, input delivery,
completion, retention, and evidence disagree by construction.

This is a deliberate scope change from
[ADR-0009](./0009-phux-vs-mux-positioning.md), which rejected agent-orchestration
product policy from Phux, and a boundary change to
[ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md), which
closed the terminal synchronization wire to structured product surfaces. The
new information is that durable identity and evidence cannot be correct when
each peer consumer independently owns them. Keeping those authorities in
Cockpit would also force Cockpit to recreate Phux's execution seams.

## Decision

If accepted, this ADR **narrowly amends ADR-0009**: Phux gains policy-neutral
durable coordination primitives shared by every client. It still does not own
model selection, worktree UX, prompt strategy, cost dashboards, compaction,
formation policy, or Cockpit's product interaction. Those remain product policy.

Phux will add a logically separate **work coordinator** above its existing
terminal substrate. The coordinator is the sole writer for durable work in its
authority domain. It accepts typed idempotent commands, appends ordered facts,
and derives current projections transactionally. Clients never write
authoritative current rows directly.

The initial coordinator may run inside the existing per-user Phux server. The
logical boundary remains explicit: terminal-only clients need not mount the work
surface, and a future supervised or federated coordinator must preserve the same
identity and authority contracts.

This ADR also **scopes ADR-0030's closed list to terminal synchronization**.
Objective, Run, Artifact, Signal, and event projections do not enter L1, L3
metadata, `AgentEvent`, or terminal HELLO capabilities. The coordinator uses a
separate protocol endpoint with independent version and capability negotiation.
It may reuse Phux transport and authentication implementations, but terminal-only
peers never parse coordinator frames and a coordinator change never forces a
terminal-protocol minor bump. The endpoint's transport binding and bounded wire
format require a follow-up ADR before implementation. It must obey
[ADR-0061](./0061-capabilities-add-versions-break.md)'s additive-capability rule.

## Authority

| Fact | Authority |
|---|---|
| Objective intent and lifecycle | Work coordinator |
| Run identity, attempt ordinal, policy, and outcome | Work coordinator |
| WorkSession identity and immutable binding history | Work coordinator |
| Artifact identity, immutable revisions, manifests, and retention | Work coordinator |
| Durable Signal facts, occurrences, acknowledgement, and resolution | Work coordinator |
| Process, process group, PTY, exit, and authoritative geometry | Terminal owner |
| Terminal output order and engine/bootstrap generation | Terminal owner |
| Input lease, signal delivery, and acknowledged input result | Terminal owner |
| Admission of terminal facts into durable lineage and evidence | Work coordinator |
| Federated execution facts | Owning satellite, with source authority preserved |
| Window, tab, split, focus, viewport, and transient disclosure state | Client |
| Rebuildable caches, search indexes, and rendering projections | Client |

The coordinator records terminal-owner facts with source authority, incarnation,
sequence, trust, and explicit gaps. It cannot rewrite them. A terminal owner
cannot independently mutate durable work lineage.

Start, cancel, pause, and resume are coordinator commands. The coordinator
records command acceptance, delegates an operation to the terminal owner when
needed, and records the typed result as another fact. Interactive key and pointer
input continue through the terminal protocol against the exact live generation.
Every mutating work command carries a client operation ID and expected object
revision. After an unknown result, a client queries that operation ID; it never
blindly retries and creates a second Run.

Signal acknowledgement and resolution are separate. A person may acknowledge
or suppress through an authorized command. Only an authoritative recovery,
completion, or corrective fact resolves an execution or evidence Signal.

## Identity

The work plane introduces opaque, globally unique identifiers for at least:

```text
CoordinatorId  ObjectiveId  RunId  WorkSessionId
BindingId      ArtifactId   SignalId  EventId  OperationId
```

`WorkSessionId` deliberately avoids collision with the reference TUI's existing
`SessionId`, which is grouping metadata. A WorkSession is stable product identity;
a Terminal is one replaceable provider resource bound beneath it.

```text
ObjectiveId -> RunId -> WorkSessionId
                       -> ArtifactId/revision
                       -> SignalId

WorkSessionId -> BindingId -> authority + typed resource locator + generation
```

A retry mints a new `RunId`. Moving a terminal between views, reconnecting a
client, or replacing a bootstrap does not. `TerminalId`, PID, socket, layout
position, host text, and the volatile `HELLO_OK.server_id` incarnation are never
durable product identity.

`CoordinatorId` names one persisted installation authority. Every event and
command result also carries a checked `authority_epoch`. One process holds the
single-writer store lock for an epoch. The initial release forbids live
coordinator transfer and configures exactly one home coordinator for a lineage.
A backup may be restored only with the prior writer offline; activation advances
the epoch transactionally before accepting commands. A future transfer protocol
must fence the prior writer before publishing the new epoch. Copying a store and
starting both copies is corruption, never federation.

## Guarantees

Until this proposal is implemented, Phux makes no durable-work claim. Existing
attach, workspace restore, native agent resume, and recording keep their current
narrow meanings:

- attach reconstructs a replica while the terminal owner still exists;
- workspace restore creates replacement processes and layout;
- native agent resume may continue provider conversation state in a replacement
  process, not resurrect the old PTY or Run; and
- a recording is a consumer artifact, not complete authoritative history
  ([ADR-0060](./0060-self-contained-session-recording.md)).

After implementation, durable metadata still cannot resurrect a process. The UI
may claim continuous execution only when the terminal authority proves the same
live resource and generation. Owner loss ends that execution; replay and resume
are distinct states.

## Trust and bounds

Local coordinator access inherits the owner-only UDS and same-UID trust model.
Remote access requires authenticated `CoordinatorId` plus explicit scopes before
any durable fact or command is accepted. Initial scopes separate at least work
read, work command, terminal control, artifact read, Signal acknowledgement, and
administration. Existing terminal pairing is not implicitly work authorization.
Satellite facts retain their authenticated source authority and cannot become
verified evidence over an unverified transport.

Every coordinator envelope, string, event payload, snapshot, artifact manifest,
queue, replay page, and retained cache is bounded. Subscriptions are credit-based
by count and bytes; control and lifecycle traffic retain reserved capacity.
Overflow produces an explicit gap and resync, never silent eviction. Exact limits
and downgrade behavior belong to the coordinator-protocol ADR and conformance
tests, not client preference.

## Delivery order

1. Ratify the coordinator protocol, authenticated authority identity, scopes,
   capability negotiation, bounds, and operation-ID semantics.
2. Define stable work IDs, authority epochs, typed bindings, and canonical
   bounded codecs.
3. Add the crash-safe single-writer event store and deterministic projections.
4. Bind only newly coordinator-mediated work. Never adopt legacy terminals by
   title, cwd, PID, ordinal, numeric TerminalId, or agent metadata.
5. Admit lifecycle, output, acknowledged input, and attention facts with source
   provenance and explicit gaps.
6. Add immutable completion seals, artifact manifests, replay, and retention.
7. Expose the work surface to peer consumers, including Cockpit.
8. Extend authenticated federation: one home coordinator owns lineage while
   satellites remain authoritative for their execution facts.

Durable local work uses the local Phux coordinator. Cockpit's direct in-process
PTY path may remain an explicitly ephemeral terminal, but it does not grow a
second daemon or authoritative work store.

## Tradeoffs

- The coordinator becomes a critical durable component requiring migrations,
  bounded storage, backpressure, integrity checks, and crash testing.
- Work and terminal protocols become two negotiated surfaces.
- Federated work has split lineage and execution authorities, so provenance and
  conflict rules are mandatory.
- Legacy terminals cannot be silently upgraded into durable work.
- Cockpit cannot ship durable-work features independently of the Phux contract.
- ADR-0009's smaller substrate scope is deliberately widened, increasing the
  policy-neutral infrastructure Phux must keep stable.

## Alternatives considered

**Make Cockpit authoritative.** Rejected: it privileges one consumer, loses
authority when that UI is absent, and duplicates the Phux execution substrate.

**Store work in L3 metadata.** Rejected: metadata cannot provide transactional
lineage, immutable event order, evidence completeness, or single-writer
authority.

**Treat TerminalId as durable Session identity.** Rejected: it names one live
resource under one authority and incarnation, not attempts, replacement
bindings, completed history, or migration.

**Create a second local runner for Cockpit.** Rejected: Phux already owns PTYs,
attach bootstrap, generations, leases, signaling, and reconnect-safe input. A
second implementation would create competing execution authorities.
