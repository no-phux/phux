---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-09-19
---

# Vision

**TL;DR.** phux's future direction is a durable work coordinator with swarm
members as first-class clients, plus federation using lazy state
synchronization by default. The coordinator does not ship today.
These are architectural goals, not a delivery schedule or a description of
current capabilities.

---

## Why now

Reusable terminal engines and agent-driven workflows create two opportunities.
libghostty lets server and client run the same parser rather than translating
between different terminal models. Agents need direct control and evidence:
command completion with an exit code, remote process creation, and observation
without screen scraping.

Current capabilities live in [`CONCEPTS.md`](./CONCEPTS.md). The sections below
describe the direction beyond them.

## Lazy state synchronization as the wire's destination

Lazy state synchronization ships as an opt-in output mode for custom consumers
([ADR-0018](adr/0018-lazy-state-synchronization.md)). The server synthesizes the
minimum VT transition from each consumer's last reference state. Bundled TUI
and web clients still request raw output, the lowest-latency path for their
current use cases. Making StateSync the default on federation links is a
future goal.

The research note (now archived) captures the algorithm composition:
[`../research/archive/2026-05-26-state-sync-algorithm.md`](../research/archive/2026-05-26-state-sync-algorithm.md).

## A durable work coordinator, not shipped

The proposed coordinator would own durable work identity and evidence;
clients would own presentation. Swarm members would authenticate as first-class
clients, alongside TUI, Cockpit, web, and mobile projections. A Run could exist
without a Terminal. Objective, Run, WorkSession, Actor, Artifact, and Signal
would have shared definitions rather than being inferred independently by
each client.

This surface does not ship. The proposed contract and delivery order live in
[ADR-0092](adr/0092-durable-work-coordinator-authority.md); swarm-as-client
positioning is in [ADR-0132](adr/0132-swarm-members-are-coordinator-clients.md).

## Milestones

The client surfaces below already ship. The hub entry distinguishes shipped
routing from the remaining federation work:

- **Hub.** Hub-and-spoke routing ships (`host/@N`; the hub does not merge
  remote session or window models). Remaining: lazy state sync as the
  federation default, and richer joins beyond Terminal-scoped relay.
- **Web.** The browser client ships as a carry-your-own-engine consumer
  ([`consumers/web.md`](./consumers/web.md)).
- **Cockpit.** The independently versioned native macOS client ships
  ([`consumers/cockpit.md`](./consumers/cockpit.md)).

## What phux is, on purpose, not

The scope limits remain in force. [CONTRIBUTING.md](../CONTRIBUTING.md) owns
their rationale:

- No embedded scripting language.
- No in-process plugin host. Plugins are external packages declared in
  config, not code loaded into the server.
- No tmux-style copy-mode clone.
- No homegrown crypto.
- No format-template DSL.
