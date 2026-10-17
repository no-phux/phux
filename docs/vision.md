---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-10-17
---

# Vision

**TL;DR.** phux's direction is a personal distributed computing environment
across devices, with humans and agents as native peers. Resource access,
durable work, and authority are independent foundations aligned by coherent
identity and ownership. Interfaces and coordinators remain interchangeable.
This is the living architectural direction; it describes goals beyond the
current implementation.

## The environment

Start work from a phone, inspect it from a laptop, let a server carry out the
expensive part, and return through whichever interface fits. Bring a person or
agent into the same work with a clear role and bounded authority. Replacing a
worker or closing an interface should leave the work understandable and
available to continue.

This file is the canonical, living statement of that direction. The dated essay
[One Computing Environment, Many Machines](https://phall.io/writing/one-computing-environment/)
expresses the vision as of October 17, 2026. Current capabilities and release
limits belong in [CONCEPTS.md](./CONCEPTS.md); implementation decisions belong
in the [ADRs](./adr/README.md).

## Three aligned foundations

### Resource fabric

Terminals, browsers, sandboxes, files, and services should be addressable across
hosts. A participant should be able to discover a resource, understand where it
lives, and use it through an appropriate interface. Host boundaries remain
visible where they affect capability, latency, cost, or trust.

A resource has its own owner and lifetime. Some resources are long-lived;
others are created for one step and discarded. Access from another device or
handoff to another participant should preserve resource identity and the
distinction between observing, controlling, and owning it.

### Durable work fabric

Computations, conversations, and collaborations should retain an identity,
history, results, and continuation context independently of any one worker or
interface. Work can wait without an active process. Several workers can
contribute to it over time, using different resources and runtimes.

The work's lifetime, a worker's lifetime, and a resource's lifetime are distinct.
Retaining work does not promise that a process survives a crash. Resuming may
require a replacement execution, new resources, or a decision about an unknown
outcome. Resource handoff should carry enough context to continue safely while
keeping those boundaries visible.

### Authority fabric

Participants need explicit answers to who may observe, act, delegate, and take
responsibility. A grant to use a resource does not transfer its ownership.
Delegation should preserve its limits and the record of who authorized and
performed an action. Responsibility should remain legible when participants
leave, workers are replaced, or resources change hands.

Human and agent peerhood means native participation in the same environment.
It does not imply equal permissions, trust, or responsibility.

### Alignment

Each foundation is useful independently. A terminal can exist without a task;
a collaboration can persist without a running worker; an observer can inspect
work without permission to change it. They fit together through coherent actor,
work, and resource identities, with explicit ownership and authority at their
boundaries.

This separation allows each foundation to evolve without making one interface,
agent harness, or coordinator the required entry point to the whole system.

## Coordination and experience

Any authorized actor may coordinate a particular piece of work. A person can
direct it, an agent can organize it, or a specialized workflow can manage it.
Coordination is a role with a scope. It does not confer ownership of every
resource or authority over every participant.

dot is one possible peer and coordinator. No part of this vision requires dot,
or any other assistant, as a central orchestrator. Clients and coordinators
should be replaceable while preserving the work and its authority boundaries.
Changing who coordinates a task must be explicit; it cannot silently create
two competing owners of the same authoritative state.

Phone, terminal, web, and desktop experiences can emphasize different things.
They should connect to the same work and resources within the participant's
permissions. Agents need a direct interface suited to discovery, action, and
verification, alongside the interfaces people use to understand and decide.

## phux's path

The terminal substrate is the starting point. Its separation of execution from
presentation provides a useful basis for shared resources. The direction is to
broaden that resource model and connect it to a common durable-work contract.

Agent runtimes should retain their own scheduling, execution, and conversation
internals. The common contract should provide enough agreement to identify
work, observe progress and outcomes, associate resources, and hand work between
authorized participants. It should not require every runtime to become the
same harness or imply portable execution state where none exists.

[Pi Durable](https://github.com/earendil-works/pi/blob/main/packages/durable/README.md)
illustrates runtime-local durability: its experimental harness can persist
conversations and tasks and resume interrupted work from storage. That is a
useful building block. Cross-runtime continuity and cross-host resource handoff
remain separate architectural goals.

The existing coordinator proposals, [ADR-0092](adr/0092-durable-work-coordinator-authority.md)
and [ADR-0132](adr/0132-swarm-members-are-coordinator-clients.md), explore shared
durable identity and agents as clients. They remain proposed designs. A durable
state authority for a work lineage and the actor coordinating a task have
different responsibilities. The wider environment should support multiple
authority domains and interchangeable coordinators without ambiguous ownership.
Specific changes to those proposals and accepted contracts require their own
review; this vision does not silently ratify a protocol or migration scheme.

Existing scope limits remain in [CONTRIBUTING.md](../CONTRIBUTING.md): no
embedded scripting language, in-process plugin host, tmux-style copy-mode clone,
homegrown crypto, or format-template DSL.

## Keeping the direction current

Update this file when the architectural direction changes. Keep it focused on
the desired environment, the boundaries between its foundations, and the
principles that constrain future choices. Record concrete decisions in ADRs
and current capability claims in their existing authoritative documents.

Keep the dated essay as a historical expression, with its link here pointing
readers to the current direction. Changes to this document should say what
changed in their commit or pull-request explanation. Recheck the status links
when reviewing the vision; a proposed capability becomes current only when its
implementation and supporting evidence establish that claim.

## Status

The complete environment above is a goal. The current implementation supplies
parts of its foundation; the linked documents own the detailed maturity record.

| Area | Current boundary | Direction and source |
|---|---|---|
| Resources and federation | Terminal and AgentSession exist in current source; installed-release support is capability-dependent. Hub-and-spoke routing preserves host ownership and does not merge remote session models. | Wider resource kinds, cross-machine workspaces, and handoff remain future work. [CONCEPTS.md](./CONCEPTS.md#status) tracks the relevant gaps under [ADR-0160](adr/0160-sessions-own-resources-and-window-is-retired.md). |
| Durable work | Persisted organization records and runtime-specific agent continuation do not provide the full common durable-work contract. AgentSession remains tied to its parent Terminal; ACP is the provider seam. | The general work coordinator remains proposed in [ADR-0092](adr/0092-durable-work-coordinator-authority.md) and [ADR-0132](adr/0132-swarm-members-are-coordinator-clients.md). [ADR-0170](adr/0170-acp-is-the-provider-seam.md) owns the ACP boundary. |
| Authority | Scope enforcement, attach roles, approvals, and delegated agent grants provide existing boundaries. | Coherent authority across the broader environment remains a goal. [CONCEPTS.md](./CONCEPTS.md#status) records shipped enforcement and its remaining authentication gap. |
| Terminal state synchronization | Lazy state synchronization is opt-in for custom consumers. Bundled TUI and web clients request raw output. | A federation default remains future direction under [ADR-0018](adr/0018-lazy-state-synchronization.md). |
| Federation networking | Hub routing exists; network reachability does not supply shared work identity or authority by itself. | The [Defguard masterplan](./architecture/defguard-federation.md) separates implemented address selection from outstanding real-network, platform, policy, and production acceptance gates. |
