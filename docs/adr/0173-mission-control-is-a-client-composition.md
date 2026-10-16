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
Delegated grants are [ADR-0163](./0163-actors-and-delegated-agent-grants.md)
Decision §5 plus its amendment of 2026-10-13 (`fa14a54f`). ADR-0163 has no
stages. The grant is built on main (`#1191`, `fa80d732`).

## Decision

1. **Client composition.** Mission control is a client composition, as
   `phux worktree` is. For tasks it may own the worktree, the brief, and
   starting the task's agent, by composing existing client verbs. The
   user-facing verb is `start`. The server and the wire learn nothing new.
   This amends the
   [ADR-0009](./0009-phux-vs-mux-positioning.md) /
   [ADR-0092](./0092-durable-work-coordinator-authority.md) /
   [ADR-0132](./0132-swarm-members-are-coordinator-clients.md) boundary
   for that task composition only. Model pickers, cost chrome, and
   compaction stay out of phux.

2. **Interim store.** Until phux-ajxq ships the coordinator, one SQLite
   store in a `phux-work` crate holds tasks. Its nouns are the
   coordinator's: Objective, Run, binding, Signal, and event. When the
   coordinator ships, the rows migrate in and this store becomes a
   client of it. The store is coordinator-shaped:
   - ids are opaque 16-byte values; a short per-repo label such as
     `phux/12` is a display alias only;
   - every mutation carries an OperationId;
   - every structural edit carries a revision;
   - exactly one writer: one keeper process per store holds its lock and
     is the only process that opens it. The CLI, Lead, the board, MCP, and
     the desktop are clients of that keeper through one client API, which
     starts the keeper on demand. A keeper owns its host's store, so a
     task on another host is reached by dialing that host's keeper, never
     by reading its files. `phux-mission` renders the client's results and
     never opens the store.
   A herdr-shaped tree keyed by the short label is outside this schema.
   Task history lives in this store. The [ADR-0123](./0123-events-are-journaled.md)
   ring stays bounded and in memory. Clients, including phux-mobile,
   decode this store through the versioned `phux.mission/v1` JSON contract
   and the JSON Schema the MC-1.1 pull request commits. That schema is the
   stable decode surface.

3. **One layer.** The coordinator owns tasks. Until phux-ajxq, the interim
   store is that coordinator. The ops hub is its first read-and-command
   client and keeps no task state. Blackbird stays frozen and optional
   ([ADR-0095](./0095-the-blackbird-boundary.md)). Beads stays dev-only.

4. **Merge.** The only merge is Patrick's explicit, TTY-confirmed action
   at the reviewed SHA. Nothing merges automatically. An agent never
   merges.

5. **Grants.** A task worker runs under the ordinary delegated grant.
   [ADR-0163](./0163-actors-and-delegated-agent-grants.md) Decision §5, as
   amended on 2026-10-13 (`fa14a54f`), is that grant, and it is built on
   main (`#1191`, `fa80d732`): `crates/phux-server/src/delegation.rs` and
   `policy.rs` hold the ledger, and `phux agent start` mints and revokes
   the credential. It carries `INVENTORY`, `OBSERVE`, `INPUT`, and `BIND`
   on the worker's own session only, and never `CREATE`, `ORGANIZE`,
   `TERMINATE`, or `SIGNAL`. Delegation only narrows. The default lifetime
   is 12 hours. The ledger is in memory, so a cold restart revokes every
   credential. `phux mission` runs as the owner. Until MC-1.4b, Lead runs
   with the owner's authority and only while attended. After MC-1.4b it
   holds the scoped grant and never the owner's: `CREATE` and `ORGANIZE`
   on the `mission` session and the sessions created when a task is
   started, never `TERMINATE` or `SIGNAL`. Stopping a worker is an Inbox
   approval as in [ADR-0128](./0128-approvals-are-held-actions.md). That
   scoped grant is a later ADR, tracked as MC-1.4b.

6. **Lost credential.** A lost credential, including one a cold restart
   revoked, is a `credential_lost` Signal on the task. The task moves to
   Needs you. It stays out of Working.

7. **Worktrees and names.** One worktree per task. Those worktrees share
   `CARGO_TARGET_DIR` and sccache. Syncing a task branch to the default
   branch is optional and off by default. Grouping uses the
   [ADR-0168](./0168-project-identity-is-a-session-attribute.md) project
   attribute.

Defaults, which Patrick may override while this record is Proposed: ship
the interim store rather than wait for the coordinator; a built-in
`phux mission` verb whose action for a task's agent is `start`; the
feature name "mission control"; the chat agent named Lead. "Coordinator"
stays the [ADR-0092](./0092-durable-work-coordinator-authority.md)
authority. "Delegation" in this record means an ADR-0163 credential.

## Why

Waiting on phux-ajxq withholds every task surface behind a protocol that
is still Proposed. A client store that already uses the coordinator's
nouns, ids, operation ids, and revisions imports without a second
identity. Putting that store in the server would add a wire and would
teach the terminal process about git. The event ring cannot be the task
log: it drops history on restart. A second writer, or a second product
store in the ops hub, Blackbird, or beads, splits the authority ADR-0092
exists to keep. An automatic or agent merge would land a SHA the human
did not confirm. A delegated grant cannot create panes or start workers,
so until MC-1.4b Lead runs with the owner's authority and only while
attended; after that it holds the scoped grant and never the owner's.
A cold restart clears the ledger unless the store raises `credential_lost`.
A keeper process, not a lock every caller takes, makes the writer the
one place that projects agent state into tasks. It also gives another
host something to dial, and that keeper is what the coordinator later
replaces.

## Tradeoffs

Two stores in sequence, interim then coordinator. Every client resolves
the display alias `phux/12` through the store, and decodes through
`phux.mission/v1`. Every call is a local socket round trip to the keeper,
and a keeper that will not start fails every verb until it does. Workers cannot create, organize, terminate, or signal. Until
MC-1.4b Lead holds the owner's authority only while attended. Shared
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
- Run task workers or an unattended Lead on the owner's grant, or name
  the user-facing verb `delegate`. Workers would hold terminate and signal
  authority. `delegate` stays the name of an ADR-0163 credential. Refused.
