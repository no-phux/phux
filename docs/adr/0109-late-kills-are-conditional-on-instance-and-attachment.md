---
audience: contributors
stability: stable
last-reviewed: 2026-09-11
---

# 0109 — Late kills are conditional on instance and attachment

**TL;DR.** A kill that may arrive after its resource stopped being the
caller's is sent as `KILL_RESOURCE_IF`. The server kills only if the caller's
instance token still names its id space and, when asked, no connection but
the spawning one has attached or used the resource. It checks and kills atomically,
refuses with `PRECONDITION_FAILED`, and a hub relays the check to the
satellite.

Status: Accepted
Date: 2026-09-11

## Context

The TUI kills a satellite pane it spawned when the follow-up attach is
refused, and phux-c2td.23 retries that kill after the link recovers. By then
the client's own state cannot say whether the pane is still safe to kill:

- A hub-relayed spawn is placed as a split in the satellite's most recently
  used session, so a client attached there, or one that attaches later, may
  be using it.
- A cold satellite restart reissues pane ids from 1, so a recorded id can
  name a brand-new pane that belongs to someone else.

`KILL_RESOURCE` is unconditional. Only the server knows both facts, and only
the server can check them without a race.

## Decision

1. **Instance token.** A server names its terminal id space with 16 random
   bytes, minted with the id allocator. A graceful upgrade restores the token
   together with the allocator. A cold restart replaces both. The token
   therefore changes exactly when ids can repeat. It is not
   `HELLO_OK.server_id`, which changes on every re-exec.
2. **Bound spawn.** `SPAWN_RESOURCE` gains `bind_instance` (field 15). A
   server answers such a spawn with `RESOURCE_SPAWNED.instance` (field 3). A
   spawn that does not ask gets the reply it always had. A hub relays both
   unchanged, so a satellite pane carries the satellite's token.
3. **Conditional kill.** New command `KILL_RESOURCE_IF { terminal_id,
   instance, conditions }`, tag `0x1b`. The server kills only when `instance`,
   if given, equals its token. `UNATTACHED_SINCE_SPAWN` requires `instance`
   and holds when three things are true. The resource came from
   `SPAWN_RESOURCE`. No connection but the spawning one has ever attached
   it (subscribed to its output) or used it (input, the input lease, an
   upload, a transcription, a signal, a screen read). And it has no child
   resource the kill would close. Check and kill share one acquisition of
   the state lock. A failed or unknown condition answers
   `PRECONDITION_FAILED` (212) and kills nothing.
4. **Federation.** A hub forwards the precondition unchanged and the
   satellite evaluates it. Every hub consumer is the same link connection on
   the satellite, so the hub first checks its own record. It must have
   relayed the spawn under the kill's token, and no other hub consumer may
   have attached or used the resource through it. A
   satellite without the feature never sees the command; the hub refuses it.
5. **Gate.** `ServerFeature::CONDITIONAL_KILL = 0x00200000`.

The normative text is `docs/spec/L1.md` §3.1, §5.2.1 and §9.1.

## Why

- **A new tag fails closed.** A trailing field on `KILL_RESOURCE` would be
  skipped by length on a peer that predates it, including an older hub or
  satellite in the middle of the route, and that peer would kill
  unconditionally. A peer that cannot decode a tag kills nothing.
- **Exempt the spawner, not the requester.** The requester's own refused
  attach was made by the connection that spawned the pane. On a satellite
  that connection is the hub's link. Keying the exemption to the spawning
  connection keeps a retry valid when it arrives after the link redialed over
  a new connection.
- **Two choke points cover every attach and use.** A session attach, a
  re-attach sweep, and `ATTACH_RESOURCE` all subscribe through one table
  method. Marking the resource there covers every path, including a
  subscription that is later rolled back. The verbs that drive or read a
  resource without subscribing, such as `phux send-keys` input, are marked
  at the one command dispatch, before they run.
- **The attachment condition needs the token.** Without it, an id reused
  after a restart names a new pane whose only user may be its own spawner,
  and a record keyed by id alone would vouch for it.
- **Opt-in binding keeps replies byte-identical.** A consumer that never
  binds sees the frame, and the Rust shape, it always had. No golden and no
  existing consumer changes.
- **One refusal code.** Every failed condition has the same recovery: leave
  the resource alone. The message says which condition failed, as ADR-0108
  argued for `OTHER`.

## Tradeoffs

- **The hub keeps state.** A bounded ledger of its 1024 most recent satellite
  spawns survives link reconnects. A resource it has forgotten is refused,
  which leaks a pane rather than risking someone else's.
- **Conservative after an upgrade.** Rebuilt resources carry no spawn record,
  so a late kill after a graceful upgrade is refused even though the token
  survives.
- **A glance counts as use.** Another connection's `GET_SCREEN` refuses a
  later kill even though it changed nothing: leaking a pane someone looked
  at is cheaper than killing one they are about to use.
- **A pane with a child is never killed conditionally.** The kill would
  close the child too, and the check does not cover it.
- **A refused pane leaks.** A consumer must not fall back to an unconditional
  kill, so the pane stays until someone kills it by hand.
- **No token in `HELLO_OK`.** A hub's `HELLO_OK` names the hub, not its
  satellites, so a client could bind a satellite id to the wrong space. The
  spawn reply is the only place the binding is atomic.

## Alternatives

- **Client-side heuristics** (forget a stray when a fresh id on its host is
  no higher than the stray's). Rejected as the only defense: it notices a
  restart only after another spawn there, and it can never see another
  client's attach.
- **A field on `KILL_RESOURCE`.** Rejected: it fails open on older peers, as
  above.
- **Ask for the token, then kill.** Rejected: a restart between the question
  and the kill races it. The check must share the kill's lock acquisition.
- **Reuse `server_id`.** Rejected: it changes on every re-exec, including an
  upgrade that keeps every id, and would refuse kills the upgrade made no
  less safe.
- **An ownership lease on the pane.** Rejected for now: it changes who may
  attach, which this problem does not need.
