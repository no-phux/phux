---
audience: contributors, agents
stability: stable
last-reviewed: 2026-10-15
---

# 0173 — Mission control is a client composition layer with an interim work store

**TL;DR.** Mission control is a client composition in the same sense as
`phux worktree`: the server learns nothing new. Tasks live in an interim
store shaped like the coordinator until that coordinator ships. One layer
owns them. Merge is only Patrick's explicit TTY confirmation at the
reviewed SHA.

Status: Proposed
Date: 2026-10-15

## Context

[ADR-0009](./0009-phux-vs-mux-positioning.md) keeps phux a terminal
substrate and leaves agent-runner and workspace-management product policy
out. [ADR-0092](./0092-durable-work-coordinator-authority.md) assigns
durable Objectives, Runs, bindings, Signals, and events to a coordinator,
and still refuses worktree UX, prompts, and model choice.
[ADR-0132](./0132-swarm-members-are-coordinator-clients.md) repeats that
refusal. The coordinator is spec-only; bead phux-ajxq is the implementation
this store migrates into. The proposed parent bead is phux-w7z2
(agent-to-agent orchestration, herdr parity).

[ADR-0054](./0054-worktree-bound-sessions.md) is the precedent that fits:
`phux worktree` composes git with existing session verbs, and the server
learns nothing about git. [ADR-0168](./0168-project-identity-is-a-session-attribute.md)
already stamps project identity at session create, client-side.
[ADR-0123](./0123-events-are-journaled.md) journals events in a bounded
in-memory ring that a restart empties; it is a wake-up, not a task history.
[ADR-0163](./0163-actors-and-delegated-agent-grants.md) stage 5 designs
delegated agent grants. This tree's status table still says agents act
with their connection's grant. Bead phux-3q58 is absent from the beads
export, so that stage's built status is unverified here.

## Decision

1. **Client composition.** Mission control is a client composition, as
   `phux worktree` is. For tasks it may own the worktree, the brief, and
   the agent launch, by composing existing client verbs. The server and
   the wire learn nothing new. This amends the
   [ADR-0009](./0009-phux-vs-mux-positioning.md) /
   [ADR-0092](./0092-durable-work-coordinator-authority.md) /
   [ADR-0132](./0132-swarm-members-are-coordinator-clients.md) boundary
   for that task composition only. Model pickers, cost chrome, and
   compaction stay out of phux.

2. **Interim store.** Until phux-ajxq ships the coordinator, one SQLite
   store in a `phux-mission` crate holds tasks. Its nouns are the
   coordinator's: Objective, Run, binding, Signal, and event. When the
   coordinator ships, the rows migrate in and this store becomes a
   client of it. The store is coordinator-shaped:
   - ids are opaque 16-byte values; a short per-repo label such as
     `phux/12` is a display alias only;
   - every mutation carries an OperationId;
   - every structural edit carries a revision;
   - exactly one writer holds the store, via a lock and an atomic write.
   A herdr-shaped tree keyed by the short label is outside this schema.
   Task history lives in this store. The [ADR-0123](./0123-events-are-journaled.md)
   ring stays bounded and in memory.

3. **One layer.** The coordinator owns tasks. Until phux-ajxq, the interim
   store is that coordinator. The ops hub is its first read-and-command
   client and keeps no task state. Blackbird stays frozen and optional
   ([ADR-0095](./0095-the-blackbird-boundary.md)). Beads stays dev-only.

4. **Merge.** The only merge is Patrick's explicit, TTY-confirmed action
   at the reviewed SHA. Nothing merges automatically. An agent never
   merges.

5. **Grants.** Task agents run with the owner's grant until
   [ADR-0163](./0163-actors-and-delegated-agent-grants.md) stage 5.
   Delegated grants stay unverified until that stage is shown to be built.

6. **Worktrees and names.** One worktree per task. Those worktrees share
   `CARGO_TARGET_DIR` and sccache. Syncing a task branch to the default
   branch is optional and off by default. Grouping uses the
   [ADR-0168](./0168-project-identity-is-a-session-attribute.md) project
   attribute.

Defaults, which Patrick may override while this record is Proposed: ship
the interim store rather than wait for the coordinator; a built-in
`phux mission` verb; the feature name "mission control"; the chat agent
named Lead. "Coordinator" stays the [ADR-0092](./0092-durable-work-coordinator-authority.md)
authority.

## Why

Waiting on phux-ajxq withholds every task surface behind a protocol that
is still Proposed. A client store that already uses the coordinator's
nouns, ids, operation ids, and revisions imports without a second
identity. Putting that store in the server would add a wire and would
teach the terminal process about git. The event ring cannot be the task
log: it drops history on restart. A second writer, or a second product
store in the ops hub, Blackbird, or beads, splits the authority ADR-0092
exists to keep. An automatic or agent merge would land a SHA the human
did not confirm.

## Tradeoffs

Two stores in sequence, interim then coordinator. Every client resolves
the display alias `phux/12` through the store. One
writer serializes the keeper and the CLI on one lock. Owner-grant agents
can do what the owner can until stage 5; that matches a single-owner
machine and is the wrong posture for someone else's host. Shared
`CARGO_TARGET_DIR` and sccache couple task worktrees at the build cache.

## Alternatives

- Build the coordinator before any task UX. That is the end state, and it
  blocks the product on phux-ajxq.
- Store tasks in the server's organization database. That is a protocol
  change and a second authority beside the coordinator.
- Herdr-shaped files whose identity is the short key `phux/12`. Those keys
  will not import as opaque coordinator ids.
- Let the ops hub, Blackbird, or beads own tasks. Each is a second
  authority.
- Merge automatically, or let an agent merge. Refused.
