---
audience: contributors
stability: stable
last-reviewed: 2026-09-15
---

# 0127 — Attach roles are intent on the input lease

**TL;DR.** An attach may declare a role: `VIEWER` makes the subscription
observe-only, and `PRIMARY` with `DELIBERATE` takeover attaches and seizes the
input lease in one step. The lease stays the only arbitration and the scope
grant stays the security boundary; a role is declared intent the server
projects onto them. Absent is today's attach, byte for byte.

Status: Accepted
Date: 2026-09-15

## Context

L1 §8.1 specified per-Terminal `PRIMARY` / `VIEWER` roles with an exclusive
primary and a takeover policy, and left them spec-only with a warning: its
default, `{ PRIMARY, NEVER }`, would refuse every existing `ATTACH_RESOURCE`
caller, the recorder's observer attach
([ADR-0060](./0060-self-contained-session-recording.md)) included, against an
attached TUI. [ADR-0033](./0033-input-authority-and-process-signals.md) had
already rejected a static role negotiated at attach, because the input lease
subsumes it, and said lease arbitration is not authorization. The workload
grant is now enforced at dispatch, so authorization has a home. Neither
covers PHA-406's editor journey: a client that attached to watch must not be
able to widen silently to input. Today any subscriber may type whenever the
lease is open.

## Decision

1. **Intent, not state.** An attach declares a `role_policy` byte: bit 0 the
   role (`PRIMARY` 0, `VIEWER` 1), bit 1 the takeover (`NEVER` 0,
   `DELIBERATE` 1), bits 2-7 reserved. It trails `ATTACH_RESOURCE` and is
   field 6 of session `ATTACH`, which applies it to every Terminal the attach
   returns. Absent means `{ PRIMARY, NEVER }` and writes nothing.
   `{ VIEWER, DELIBERATE }` is refused. `ServerFeature::ATTACH_ROLES =
   0x08000000` advertises it; a client must see the bit before declaring a
   role, because an older server ignores the byte and grants an ordinary
   attach.
2. **`VIEWER` is observe-only.** The server marks the subscription. Every
   request that needs `INPUT` on that Terminal, and `ACQUIRE_INPUT`, is
   refused whatever the grant admits, through the dispatch guard's own
   paths: a correlated `PERMISSION_DENIED`, or a drop with the rate-limited
   uncorrelated `ERROR` for fire-and-forget input. A holder that narrows to
   `VIEWER` releases its lease. The mark is a per-connection tombstone:
   detach and a stream's end leave it, and only the connection closing or a
   fresh `PRIMARY` attach (journaled) clears it. A session attached as
   `VIEWER` marks its own spawns too.
3. **Widening is visible.** Only a fresh attach declaring `PRIMARY` widens,
   and a role that flips on an existing subscription is journaled as
   `terminal_control { action: ROLE_CHANGED = 10 }`, the attaching
   connection its actor. A subscription that never proved it decodes this
   draft is not offered it: a pre-`0.9.0-draft.15` decoder fails the frame on
   an unknown action, and no older action means the same thing. A later
   decoder that predates it reads `Unknown`.
4. **`DELIBERATE` is a seize.** `{ PRIMARY, DELIBERATE }` subscribes and
   performs `ACQUIRE_INPUT { SEIZE, ttl_ms: 0 }` in the same critical section,
   with one `SEIZED` broadcast through the engine. The displaced holder stays
   attached. `{ PRIMARY, NEVER }` leaves the lease untouched.
5. **One gate.** "At most one `PRIMARY`", `ALREADY_ATTACHED` on a second
   primary, `DETACHED { REPLACED }` eviction, and the `PRIMARY`-only gates on
   `KILL_RESOURCE`, `KILL_RESOURCES`, and `RESIZE_TERMINAL` are retired. Those
   verbs are governed by the scope verbs the dispatch guard already enforces.
6. **Federation.** On a hub the consumer's role is the hub's to hold, since
   every consumer shares the link's one identity on the satellite. A viewer
   mark never crosses the link. A takeover crosses only to a satellite that
   advertises `ATTACH_ROLES`; any other gets a plain attach and a relayed
   `ACQUIRE_INPUT { SEIZE }`, and the hub's lease ledger records the new
   holder either way.
7. **Consumers.** `phux attach --viewer` and `--take`; the recorder and
   `phux agent log` attach as `VIEWER` when the server advertises the bit;
   the snapshot's `RESOURCE_STATE` lists viewers (field 4) beside
   `input_holder`; the native ABI gains `phux_client_attach_role`. This is
   the consumer surface ADR-0071 point 7c asks an ADR to name.

## Why

A role as intent keeps one arbitration. The lease already answers "who types
now" and the grant answers "who may ever"; a role records what the
connection said it wanted, which is what the editor journey needs: the
observer's own promise, enforced. Refusing through the dispatch guard reuses
a refusal every consumer already handles. Journaling the flip is what makes a
widening non-silent; a narrowing is journaled for symmetry.

## Tradeoffs

A viewer is a subscription property, not a connection property: a connection
that subscribed a Terminal as a viewer can still `ROUTE_INPUT` to a Terminal
it never subscribed, if its grant allows. The grant is the boundary; the role
is the observer's declaration. `ROLE_CHANGED` names who changed, not the new
role; a consumer reads that from `RESOURCE_STATE`. The hub reports a
satellite Terminal's `ROLE_CHANGED` with lifecycle `RUNNING`, as its seize
notice already does. A takeover through a satellite without the bit is two
relayed commands, so the attach can stand while its seize is refused. An
`AgentSession` has no lease, so a takeover of one is refused as the wrong
kind, and a role flip on one is not journaled. A failed takeover attach
returns the lease to `Open`, not to the holder it displaced; a session
`ATTACH`'s role applies after its `ATTACHED` snapshot is sent; and a client
spends a declared takeover on one attach, never on its reconnects. A session
`ATTACH` journals a flip only on a re-attach to the same session, so a
narrowing that only it performs on a pane first attached by `ATTACH_RESOURCE`
goes unjournaled. A hub's viewer mark on a satellite Terminal can outlive a
satellite restart that reuses the id, until the consumer's connection
closes; it only ever restricts. A viewer's viewport still sizes the
session's panes, and a viewer cannot answer terminal queries.

## Alternatives

Building L1 §8.1 literally: an exclusive primary refusing a second one with
`ALREADY_ATTACHED` and evicting with `DETACHED { REPLACED }`. It breaks every
observer and puts a second arbitration beside the lease.

Deleting §8.1: it loses the observe-only promise the editor journey needs.

Making `VIEWER` a scope reduction on the grant: grants are the owner's, per
credential; roles are the client's, per attach. Conflating them would let a
client narrow a grant it could not widen back without authenticating again.
