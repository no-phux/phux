---
audience: contributors
stability: stable
last-reviewed: 2026-09-14
---

# 0126 — Creates are idempotent under a client key

**TL;DR.** A create carries a client-drawn 16-byte key. The server binds the
key to the payload and the resource it created for a bounded horizon: a
retry with the same key and payload returns the original resource marked
replayed, and a different payload is refused. Events the create caused carry
the key. One dedupe substrate serves input batches and creates.

Status: Accepted
Date: 2026-09-14

## Context

A client that loses a `RESOURCE_SPAWNED` reply to a disconnect or a timeout
cannot tell whether the spawn happened. Retrying spawns a second resource;
not retrying may leave the caller without one. `phux.session.create/v1`
correlates its result through a `request_token`, but a repeat of the same
request is refused as a duplicate name, so a retried session create fails
even when the first one succeeded. ADR-0053 solved exactly this for
`APPLY_INPUT` with a bounded, incarnation-scoped operation cache, and nothing
else uses it. Agents retry: every orchestration loop that creates panes
needs a create that is safe to repeat.

## Decision

1. **Key.** `SPAWN_RESOURCE` gains field 17 `idempotency_key: bytes16`,
   non-zero, drawn by the client from a CSPRNG. It is valid for every kind.
2. **Replay and conflict.** The server binds the key of a spawn that
   succeeded to a digest of the request's fields 2 through 16 and to the
   resource it created. A repeat with the same key and digest inside the
   horizon creates nothing and answers `RESOURCE_SPAWNED { OK(id) }` with the
   original id and additive field 4 `replayed = 1`, plus the original
   instance token when that spawn was bound, whether or not the resource
   still exists. A repeat with a different digest answers the new
   `SpawnError::IDEMPOTENCY_CONFLICT = 0x07` and creates nothing. A refused
   spawn binds nothing.
3. **Session create.** `phux.session.create/v1`'s `request_token` becomes an
   idempotency key: a repeat inside the horizon returns the same
   `phux.session.created/v1/<token>` result instead of a duplicate-name
   refusal.
4. **One substrate.** ADR-0053's operation cache becomes a shared dedupe
   service keyed by operation id, holding a digest and the recorded outcome,
   with the same bounds: ten minutes, 65,536 entries, scoped to the server
   incarnation (`HELLO_OK.server_id`). `APPLY_INPUT` behaves as before.
5. **Events carry the key.** An event a keyed operation caused carries the
   key as `EVENT.operation_id` (ADR-0123): `pane_spawned` for a keyed spawn
   and for a keyed session create's seed pane. Other events carry an actor
   only; keyed kills can follow on the same substrate.
6. **Federation.** A hub forwards the key unchanged; the satellite that
   creates the resource evaluates it.
7. **Gate.** `ServerFeature::SPAWN_IDEMPOTENCY = 0x04000000`.
8. **Consumer surface.** This amends the ADR-0071 freeze: `phux spawn
   --idempotency-key KEY` and `phux new --idempotency-key KEY` send the key.

The normative text is `docs/spec/L1.md` §3.1 and §7.3.

## Why

- **The key is the client's.** The lost round trip is the one that would
  carry a server-issued token, so the client must name the operation before
  it sends it. `request_id` cannot serve: it is scoped to one connection.
- **The digest catches misuse.** A reused key with a different payload is a
  client bug, and refusing it is safer than returning a resource the caller
  did not describe.
- **Replay answers the operation, not the present.** Returning the original
  id even after that resource closed tells the caller its spawn happened; the
  caller reads current state for the rest.
- **One cache, one set of bounds.** A per-verb cache would repeat ADR-0053's
  horizon, incarnation, and eviction rules for every verb that needs them.

## Tradeoffs

- The horizon is bounded: a retry after ten minutes, or after 65,536 newer
  keys, spawns again.
- A restart forgets every key. The new `server_id` makes that visible, and a
  client re-reads `GET_STATE` instead of retrying blind.
- A replayed id may name a resource that has since closed.
- Only spawns and session creates carry keys in this decision.

## Alternatives

- **Retry by name.** Rejected: Terminals are unnamed, and a session name
  cannot distinguish a retry from a second request for the same name.
- **A cache per verb.** Rejected: duplicated bounds and eviction, and no
  single place to reason about replay.
- **Idempotency in L3.** Rejected: metadata is not the create authority, and
  a metadata write cannot return a spawned id.
- **A server-issued token before the create.** Rejected: it adds a round trip
  whose loss has the same problem.
