---
audience: contributors
stability: stable
last-reviewed: 2026-09-19
---

# 0132 — Swarm members are coordinator clients

**TL;DR.** phux is the OS a bot swarm runs on and the human control plane
over that OS. Swarm members authenticate to the coordinator as first-class
clients. TUI, Cockpit, web, and mobile are projections. A Run may bind no
Terminal. Mux-class product policy stays out of phux.

Status: Proposed
Date: 2026-09-19

## Context

[ADR-0009](./0009-phux-vs-mux-positioning.md) placed phux under Mux-class
products: a multiplexer protocol, not an orchestration app. [ADR-0092](./0092-durable-work-coordinator-authority.md)
then assigned durable Objective, Run, WorkSession, Artifact, Signal, and
evidence to a coordinator, still Proposed, and kept formation policy out.
[ADR-0097](./0097-durable-coordinator-is-a-separate-bounded-endpoint.md)
specified that endpoint as Terminal-referencing. [ADR-0103](./0103-agent-session-resource-and-producer-fed-streams.md)
made `AgentSession` a child of a Terminal.

Cockpit's north star is one person directing hundreds or thousands of
agents without holding each in their head. What shipped is the small
reading of that story: a TUI fleet overlay, CLI/MCP driving a handful of
panes, AgentSession as a pane child. That is a human watching terminals,
not a swarm using phux as its OS.

The coordinator spec's v0.1 quotas (16 connections per principal, 256
server-wide) and `START_RUN`'s `terminal_group` encode the same
assumption: work has a Terminal.

## Decision

1. **Two planes.** The coordinator is the cheap work OS: identity,
   authority, bindings, evidence, Signals. Terminal owners remain the
   expensive execution providers: PTYs, libghostty engines, input leases.
   [ADR-0003](./0003-server-process-model.md) still holds for a terminal
   server (one per user, current-thread). The coordinator must not use
   that runtime as the swarm fan-out.

2. **Swarm members are coordinator clients.** A bot authenticates to
   `phux-coordinator/1` as a peer of Cockpit. It does not enter through
   the TUI, a layout slot, or a pane detector. TUI, Cockpit, web, and
   mobile project coordinator state; they are not the swarm's control
   path.

3. **Actor, Run, optional binding.** Actor is the coordinator principal
   that owns Runs. `AgentSession` remains the L1 harness log
   ([ADR-0103](./0103-agent-session-resource-and-producer-fed-streams.md)).
   Terminal is an optional binding under a WorkSession. A Run may have
   zero bindings. Bindings name a `ResourceId` of any kind the
   coordinator admits, not Terminal-only. Lifting AgentSession's
   Terminal-parent rule is in scope as a follow-up; this ADR does not
   change that L1 facet.

4. **Who mints Runs.** Any authorized principal — human or bot — may
   `START_RUN`. A swarm fans out without a UI round-trip.

5. **Formation is recorded policy.** A formation is a coordinator
   document (roles, capabilities, escalation, concurrency or budget
   numbers) that products author and the coordinator enforces. Phux
   does not own the editor, model picker, prompt, compaction, cost
   chrome, or worktree UX. [ADR-0009](./0009-phux-vs-mux-positioning.md)'s
   Mux-class refusal stands.

6. **Do not ship pane-shaped swarm.** Another client whose fleet object
   is "every pane" is not this decision. Attention is Signals and
   drill-in to a bound resource, when one exists.

7. **Delivery.** Ratify this positioning; amend
   [`docs/spec/coordinator.md`](../spec/coordinator.md) (optional
   bindings, Actor, quotas that survive swarm clients); implement the
   coordinator; then one attention projection. Terminal-only clients
   stay valid.

## Why

Durable identity cannot live in each client
([ADR-0092](./0092-durable-work-coordinator-authority.md)). A pane per
bot cannot be the unit at thousands of actors: most members need work
identity and Signals, not a grid. Human attention is exceptions, not
4,000 replicas. Keeping Mux-class policy out preserves the substrate
for other products, including Mux-class ones, as ADR-0009 required.

## Tradeoffs

Two protocols and two scale regimes. Coordinator implementation is now
on the critical path before more human clients. Formation schemas are
sticky once enforced. AgentSession still requires a Terminal parent
until a follow-up, so today's harness log remains pane-shaped.
[ADR-0003](./0003-server-process-model.md)'s crash radius still applies
to terminals; splitting the coordinator out is what keeps the swarm OS
from inheriting it.

## Alternatives

**Stay ADR-0009-only: Cockpit owns swarm.** Rejected: two authorities
for the same work, which ADR-0092 already refused.

**Swarm as a TUI/Cockpit fleet of panes.** Rejected: the object is
wrong; cost and attention both fail at thousands.

**Become Mux.** Rejected: model selection, prompt strategy, and cost
chrome are product policy. This ADR widens infrastructure, not that
surface.

**Every Actor is a Terminal.** Rejected: the VT engine is the expensive
plane; requiring it makes the cheap plane impossible.
