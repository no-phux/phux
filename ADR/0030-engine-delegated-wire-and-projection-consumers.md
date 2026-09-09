---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-06-06
---

# 0030 — Engine-delegated wire and projection consumers

**TL;DR.** Both ends of the wire run the same terminal engine (libghostty),
so the wire carries only identity, lifecycle, transport, opaque terminal
bytes, and L3 metadata; every structured surface — screen state, semantic
events, panes, layouts, "run and wait" — is a consumer-side projection of
that shared engine, never a wire tier. This rejects the gRPC/cells-on-wire
agent design, dissolves the L2 collection tier into L3 grouping plus a
single atomic L1 batch operation (`KILL_TERMINALS`), names phux-web as the
reference projection pattern, and reaffirms ADR-0017 as the constraint that
keeps the reference TUI a pure consumer.

Status: Superseded by ADR-0102
Date: 2026-06-06
Superseded by [ADR-0102](./0102-resources-the-server-serves-kinds.md): the engine-delegation principle survives inside it, generalized from Terminal to every kind.
Amended by [ADR-0104](./0104-parent-bindings-are-l1-lifecycle.md): the one-irreducible-op rule becomes two, atomic teardown and the parent-close cascade.

## Context

A docs-wide audit surfaced a cluster of contradictions that all reduce to
one unresolved question: **what is allowed to be a wire tier, and what must
be a consumer-side projection of the shared engine?** The symptoms:

- Session/collection lifecycle verbs (`CREATE_SESSION`, `KILL_COLLECTION`,
  `RENAME_SESSION`) ship today as L1 commands in `phux-protocol`'s `Command`
  enum, while [ADR-0017](./0017-tui-not-protocol-privileged.md) says session
  vocabulary is banished from the wire and [ADR-0015](./0015-protocol-layering.md)
  reserves the collection lifecycle for L2.
- Two agent surfaces disagree. The live wire realizes agent needs as L1
  commands (`GET_SCREEN`, `ROUTE_INPUT`, `GET_TERMINAL_STATE`) plus an
  `AgentEvent` push frame; the unbuilt `docs/spec/L2_AGENT_PROTOCOL.md`
  (marked `stability: scratch`) prescribes a gRPC+JSON transport carrying
  structured cells and a different event taxonomy.
- Two SDK docs disagree: a hand-rolled L1 codec versus a gRPC/tonic
  structured-state service.
- The encoding spec prescribes field-tagged TLV; every message body is
  positional. This is acknowledged divergence, not a settled choice.

These are not four bugs. They are four places where structure that belongs
to a *consumer's view* of the terminal has either leaked onto the wire or
been proposed for it. Without a stated principle, each gets re-litigated in
isolation and the wire accretes product opinions one verb at a time — the
failure mode [ADR-0015](./0015-protocol-layering.md) and
[ADR-0017](./0017-tui-not-protocol-privileged.md) already named for sessions
and layout, now recurring for agent state.

[ADR-0013](./0013-libghostty-bytes-on-wire.md) established that both ends run
libghostty and the wire carries VT bytes. [ADR-0018](./0018-lazy-state-synchronization.md)
generalized that to lazy state synchronization of engine state.
[ADR-0022](./0022-tool-for-agents.md) framed every consumer as a different
*projection* of one source-of-truth `Terminal`. This ADR makes the shared
premise of all three normative and uses it to settle the audit.

## Decision

### 1. The engine is delegated; structure is always a projection

phux does not own terminal semantics. libghostty owns the mapping from bytes
to a grid of cells and from input atoms to bytes, and both ends of the wire
run that same engine ([ADR-0013](./0013-libghostty-bytes-on-wire.md)). phux
never re-encodes terminal state into a second representation on the wire.

It follows that **any structured view of a terminal — a cell grid, a
semantic command-boundary stream, a pane tree, a layout, a "run a command
and collect its output" result — is computed by a consumer from the engine
it already runs.** Structure is a projection, not a transmission. Putting a
structured terminal representation on the wire would re-create the tmux
re-parse liability one layer up: a second model that can drift from the
engine and degrade under capability mismatch, which is precisely the cost
[ADR-0013](./0013-libghostty-bytes-on-wire.md) paid to remove.

### 2. The wire's job, stated as a closed list

The wire carries exactly: terminal **identity**
([ADR-0016](./0016-terminal-id-as-wire-primary.md)); terminal
**lifecycle**, including an atomic multi-terminal batch operation;
**transport** framing and capability negotiation; **opaque
terminal bytes** (output, snapshot, input atoms per
[ADR-0024](./0024-wire-owns-input-atoms.md)); and **L3 metadata** the server
stores without interpreting. It does not carry structured screen state,
semantic event taxonomies as a normative type system, panes, layouts, or
command-runner results. Those are consumer projections.

The existing L1 agent affordances are read in this light. `GET_SCREEN` and
`GET_TERMINAL_STATE` return engine-derived snapshots a consumer could also
compute locally; they are a convenience for consumers that have not yet
adopted the carry-your-own-engine pattern (point 4), not a license to make
structured state a normative wire contract. New structured surfaces SHALL NOT
be added to the wire; they belong in the projection.

### 3. The gRPC/cells-on-wire agent design is rejected

`docs/spec/L2_AGENT_PROTOCOL.md` proposes a separate gRPC+JSON transport that
puts structured cells and a parallel event enum on the wire. It is rejected
and superseded by this ADR. It violates point 1 (structure on the wire), it
forks the codec and event taxonomy from the live L1 surface, and it
contradicts [ADR-0022](./0022-tool-for-agents.md)'s "agents are a projection,
the CLI + JSON schema is the contract." The structured agent surface
(cells, command results, semantic events) is a *local projection* over the
shared engine, exposed to agents through the CLI and its versioned JSON
schema, not a wire service.

### 4. Consumers are peers; phux-web is the reference projection

The TUI, the browser client, and the agent surface are peers — none is
protocol-privileged ([ADR-0017](./0017-tui-not-protocol-privileged.md)). The
reference pattern for a consumer is **carry your own engine and project
locally**: `phux-web` ([ADR-0025](./0025-browser-web-client.md)) runs
`ghostty-vt.wasm`, speaks the exact wire codec ([ADR-0024](./0024-wire-owns-input-atoms.md)),
and computes its rendered view from engine state it owns. The agent SDK
SHOULD follow this pattern — run the engine, project to structured state
locally — rather than become a gRPC structured-state service. The wire stays
identical across all three; only the projection differs.

### 5. Group lifecycle is L3 metadata plus an atomic L1 batch op; no L2 tier

The one thing a consumer-side projection genuinely *cannot* do is an atomic
group operation — kill a bundle of terminals such that no observer sees a
partial state. That, and only that, is irreducible — and it is a single L1
operation, not a tier. The wire gains `KILL_TERMINALS { ids: [TerminalId] }`,
applied atomically under the server's existing single
`Mutex<ServerState>` lock (one lock acquisition, all-or-nothing for a local
server; cross-host atomicity is out of scope). There is **no L2 collection
tier**: group membership and names are L3 metadata plus client logic, and
sessions, windows, panes, and layouts remain L3 conventions plus client
logic ([ADR-0017](./0017-tui-not-protocol-privileged.md),
[ADR-0019](./0019-tui-multi-pane-rendering.md),
[ADR-0027](./0027-terminal-references-and-l3-links.md)).

The current code is wrong here: `CREATE_SESSION`, `KILL_COLLECTION`, and
`RENAME_SESSION` ride as L1 commands, putting session vocabulary and
collection lifecycle in the substrate tier. They decompose and are removed:
create is `SPAWN_TERMINAL`(s) plus L3 metadata, rename is an L3 metadata SET
on a name key, and kill-group is the new `KILL_TERMINALS { ids }`. The
`CollectionId` plumbing is retired where it existed only to serve those
verbs; where it is entangled with `SPAWN_TERMINAL` or L3 metadata scoping it
may remain as a documented opaque grouping key (not a lifecycle tier) with
the remnant noted for a follow-up bead. The migration is tracked as a code
task; until it lands the docs state the target and flag the divergence
inline.

### 6. The reference TUI is the wedge, held pure by ADR-0017

The reference TUI is the daily-driver adoption surface that bootstraps a
population of terminals-on-the-wire, and it is worth heavy product
investment. Its differentiator is the wire itself — attach/detach, remoting,
and humans and their agents sharing the same live terminals — not local
splits, so it is not merely a second local multiplexer. The constraint that
keeps the wedge from corrupting the platform is
[ADR-0017](./0017-tui-not-protocol-privileged.md): the TUI gets no
protocol-level standing, and its needs land as L3 conventions and client
logic, never as new wire surface. Investing in the TUI as a product and
holding it as a pure consumer are not in tension; the leaked L1 session verbs
(point 5) are the current breach of that line, and the thin or unpublished
wire/agent/web docs are the current gap.

## Rationale

The delegation principle is what gives phux its central property: because the
engine is shared and never re-encoded, the wire cannot introduce a second
terminal model that drifts or degrades. Every time structure has been
proposed for the wire — layout in an early draft of
[ADR-0015](./0015-protocol-layering.md), session verbs in
[ADR-0021](./0021-control-plane-commands.md), cells-on-wire in the agent
spec — the same argument retires it: the structure is recoverable from the
shared engine, so transmitting it adds a drift surface and a conformance tax
on consumers that do not want it, while buying nothing the projection lacks.

Group lifecycle is the lone exception because atomicity is not recoverable
from a projection: a client tearing down N terminals one at a time exposes
intermediate states and races a concurrent observer. A server-side atomic
operation is the cheapest correct answer — and the cheapest *form* of that
answer is a single L1 verb (`KILL_TERMINALS { ids }`) under the existing
server lock, not a whole tier. Atomicity earns one op; it does not earn L2.

Naming phux-web as the reference pattern turns an abstract principle into a
copyable shape: a consumer that wants structure runs the engine and reads it,
exactly as the browser client already does in shipping code.

## Tradeoffs

- **A structured agent surface costs each agent consumer an engine.** Running
  libghostty to project structured state is more work than reading cells off
  a gRPC stream. We accept it: it is the same cost the browser client already
  pays, and it removes the drift surface a structured wire would add.
- **Removing the leaked verbs is a wire-affecting change** that today's code
  has already shipped at L1. Decomposing them into `SPAWN_TERMINAL` + L3
  metadata + `KILL_TERMINALS` (and retiring the `CollectionId` plumbing that
  only served them) is real migration work, not a doc edit, and bumps
  `PROTOCOL_VERSION` 0.2.0 → 0.3.0; the docs carry an inline divergence marker
  until it lands.
- **The encoding question is left open here.** Whether message bodies migrate
  from positional to field-tagged TLV is orthogonal to tiering and is not
  decided by this ADR; it remains tracked separately. Naming it avoids the
  reader inferring that "engine-delegated wire" settled the codec shape.
- **Heavy TUI investment under a no-privilege constraint** means TUI features
  must be expressible as L3 conventions and client logic. That is a real
  design discipline, occasionally more work than a bespoke wire message would
  be, and it is the price of keeping the substrate product-agnostic.

## Alternatives

**(B) Dissolve L2 entirely into L3 metadata plus an L1 batch-kill — ADOPTED.**
Drop the Collection tier; represent grouping as L3 metadata (a set of
`TerminalId`s under a well-known key) and provide atomicity through a single
L1 batch operation (`KILL_TERMINALS { ids }`) that the server applies as one
unit under its existing lock. Adopted because it is the most faithful
expression of this ADR's own principle — atomicity is the lone exception, so
it earns exactly one op, not a tier — and because it is better for
federation: there is a single federated identity, `TerminalId`, with no
second `CollectionId` to federate and reconcile. The honest cost is that it
pushes membership consistency into client-maintained metadata (no
server-enforced membership view) and turns "kill this group" into a
client-assembled id list rather than a named server-side entity. We accept
that: grouping is presentation, and the one correctness need (atomic
teardown) is fully served by the batch op.

**(A) Minimal lifecycle-only L2 collection tier — considered, not chosen.**
Keep L2 as a narrow tier whose entire job is atomic create/kill/membership of
a Collection, with sessions/windows/panes/layout as L3 plus client logic.
Honest description: it isolates the one operation a projection cannot perform
atomically behind a server surface, makes grouping a named server-consistent
entity, and leaves [ADR-0015](./0015-protocol-layering.md)'s three-tier model
intact. Not chosen because it adds wire surface (a whole tier) for what is one
op of irreducible need, and it introduces a second federated identity
(`CollectionId`) to carry, reconcile, and version alongside `TerminalId` —
weight that option B avoids while still delivering atomic teardown.

**(C) Adopt the gRPC/cells-on-wire agent protocol.** Build the agent surface
as the separate structured-state service `L2_AGENT_PROTOCOL.md` describes.
Rejected per Decision point 3: it puts structure on the wire, forks the codec
and event taxonomy, and contradicts the projection thesis. Recorded here only
to mark it as considered and closed.
