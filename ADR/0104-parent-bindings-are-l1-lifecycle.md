---
audience: contributors
stability: stable
last-reviewed: 2026-09-09
---

# 0104 - Parent bindings are L1 lifecycle

**TL;DR.** A resource may name one parent at spawn. The binding is immutable
and server-enforced: closing a parent closes every child with
`CloseReason::ParentClosed`, atomically under the single state lock, and
closing a child never affects the parent. One level only in v1; an
`AgentSession` requires a Terminal parent and a Terminal has none. The
relation is lifecycle, not metadata, so it lives on L1 beside atomic teardown.

Status: Proposed
Date: 2026-09-09

## Context

An `AgentSession`
([ADR-0103](./0103-agent-session-resource-and-producer-fed-streams.md)) lives
inside a Terminal: the harness process that produces its records is the PTY
child, and when that Terminal closes the session has nothing left to report.
Something has to close it, and every observer has to agree when.

phux already has a place for relations between served things.
[ADR-0027](./0027-terminal-references-and-l3-links.md) put tags and `group`
links in L3 as `phux.link/v1`, opaque to the server and resolved by clients,
and that has held for every relation since. It holds because those relations
are advisory: nothing breaks if two clients disagree for a moment about which
Terminals are grouped.

[ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md) admitted
exactly one irreducible group operation, atomic teardown, because a projection
cannot make N kills atomic and a concurrent observer would see the partial
state. That was stated as "one op, not a tier".

## Decision

1. **A parent is named at spawn and never changes.** `SPAWN_RESOURCE` field
   12 `parent: ResourceId`. The server validates it: `ParentNotFound` when it
   does not resolve, `ParentKindMismatch` when the kind does not accept that
   parent. There is no verb to rebind.
2. **Closing a parent closes its children.** A parent leaving for any reason
   (exit, `KILL_RESOURCE`, `KILL_RESOURCES`, server shutdown) closes every
   child with `CloseReason::ParentClosed`. The cascade runs under the single
   `ServerState` lock, in the same acquisition that removes the parent, so no
   client can observe a child whose parent is gone. This is the guarantee
   `KILL_RESOURCES` already gives a batch.
3. **Closing a child never affects the parent.** An `AgentSession` ending
   leaves its Terminal running.
4. **`RESOURCE_CLOSED` carries the reason.** New field 3
   `reason: CloseReason { Exited = 0, Killed = 1, ParentClosed = 2,
   ServerShutdown = 3, Unknown }`. A consumer can tell a cascade from a
   kill without correlating frames.
5. **One level in v1.** A resource with a parent cannot itself be a parent.
   `AgentSession` requires a Terminal parent; a Terminal has no parent. A
   deeper tree is a later ADR.
6. **Parents federate like ids.** A parent id is a `ResourceId` and routes as
   one. A hub retags parent ids in its aggregate inventory exactly as it
   retags resource ids, so a satellite child is reported under its satellite
   parent and never under a hub-local id.
7. **`ResourceInfo` carries `parent`** as a trailing additive field, so a
   consumer reading the inventory has the tree without a second query.
8. **The server keeps the graph.** `ServerState` holds
   `children: HashMap<ResourceId, Vec<ResourceId>>`; the cascade reads it in
   `kill_resource(s)` and on Terminal exit, under the one lock.

This amends ADR-0027 (the parent relation leaves L3; `phux.link/v1` stays for
advisory, client-defined relations) and ADR-0030 (the irreducible set is two
operations, atomic teardown and parent cascade). It builds on
[ADR-0102](./0102-resources-the-server-serves-kinds.md).

## Why

**The same argument that earned atomic teardown earns this.** ADR-0030 kept
`KILL_TERMINALS` on L1 because no projection can make a group teardown
atomic. A resource that lives inside another has that problem in the
lifecycle direction: no client can guarantee the child closes when the parent
dies, because the client that would do it may be detached, racing, or
absent, and the Terminal can exit without any client asking. Only the process
that removes the parent can close the child in the same step. That makes the
relation lifecycle, and lifecycle is the L1 list.

**Metadata cannot express it.** `phux.link/v1` is last-writer-wins bytes the
server does not read. A cascade encoded there is a client convention that
every consumer must implement identically, and a consumer that does not
leaves an orphan whose stream never ends. The one thing ADR-0027 was careful
to keep out of the server is the one thing this needs from it.

**Immutable, because rebinding is a second lifecycle.** A movable parent
needs a verb, an event, a race with the cascade, and a federation story for a
child whose parent moved hosts. None of that has a consumer. Spawn-time
binding gives the child exactly one parent for its whole life and the server
one invariant to hold.

**One level, because two kinds need one edge.** An `AgentSession` under a
Terminal is the only binding this program creates. A general DAG would have
to decide cycle detection, cascade order across levels, and what a
mid-tree kill means, for no caller.

## Tradeoffs

- **Not built.** The field, the reason enum, the graph, and the cascade are
  program work; today a Terminal closing leaves nothing behind because
  nothing lives under it.
- **A parent's exit is a fan-out under the lock.** One Terminal closing
  now also closes its children before the lock releases. Bounded by the
  one-level rule and by there being one session per harness in practice.
- **A frozen parent is a frozen placement.** A session cannot follow its
  harness if the harness is restarted in a different Terminal; it closes
  with the old one and a new session opens under the new one, which is what
  [ADR-0068](./0068-native-agent-session-restore.md)'s `native_id` is for.
- **`Unknown` in `CloseReason` is a client obligation**, like every open
  enum here: an unrecognised reason is displayed as closed, not dropped.
- **Cross-host atomicity is out of scope**, as it is for `KILL_RESOURCES`:
  a hub-side parent with a satellite child cannot exist under the one-level
  and kind rules, so the case does not arise in v1.

## Alternatives

**Parent as an L3 link with a client-side cascade.** Rejected: it races the
parent's own exit, it depends on a client being attached, and partial states
(a child with no parent) are observable by every other client while the
cascade runs.

**Arbitrary DAG bindings.** Rejected for v1: cycles, cascade ordering across
levels, and a movable-parent story, with no consumer that needs any of it.
The field shape does not preclude a later depth rule.

**Reuse the `owner_terminal` spawn field.** Rejected: that field
([ADR-0050](./0050-explicit-spawn-ownership.md)) is a placement hint that
tells the server which layout slot a new Terminal lands beside; it is not
lifecycle, it applies to Terminal-kind only, and giving it a second meaning
would make every existing spawn a binding.
