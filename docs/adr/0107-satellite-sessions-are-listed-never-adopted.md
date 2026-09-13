---
audience: contributors
stability: stable
last-reviewed: 2026-09-10
---

# 0107 — Satellite sessions are listed, never adopted

**TL;DR.** A hub reports each satellite's sessions in a trailing, feature-gated
`GET_STATE` inventory keyed by host, under their satellite-local ids. It never
renumbers them into its own id space and never relays a session-scoped
`ATTACH`. Selecting one from a hub-attached consumer opens that session's
active pane through the existing resource relay.

Status: Accepted
Date: 2026-09-10

## Context

A hub merges only *terminals*. `handle_get_state_federated` discarded every
satellite's `sessions` and `windows` lists outright, because `u32` session ids
from different servers collide and [ADR-0016](./0016-terminal-id-as-wire-primary.md)
made `ResourceId` the wire primary. The cost: a person with a laptop hub and a
devbox satellite has no surface anywhere — TUI picker, `phux ls`, Cockpit
catalog — that admits the devbox's sessions exist. Panes on that satellite are
already addressable (`host/@id`, [ADR-0066](./0066-host-namespace.md)) and
already relayed, so the gap is a *listing* gap, not a routing one.

Two things were conflated in "not federation-routable": that a satellite
session **id** cannot be resolved by the hub (true, permanently), and that a
satellite session cannot be **named** to a consumer (never argued for).

## Decision

1. **A hub lists satellite sessions, host-qualified, without adopting them.**
   `SessionSnapshot` grows a trailing `hosts` list: one row per configured
   satellite, carrying that host's sessions with their satellite-local ids,
   name, window and pane counts, and the session's active pane re-tagged
   `SATELLITE { host, id }`. It is gated on a new `ServerFeature`,
   `HOST_SESSIONS`, and is additive: a decoder that predates it stops at the
   resource facets and never sees it (`docs/spec/L1.md` §9.1).
2. **The hub never renumbers a satellite session.** The reported `id` is the
   satellite's own, MUST NOT be joined against the snapshot's `sessions`, and
   is never accepted as a hub-side selector. `sessions` keeps meaning
   "sessions on the server you are talking to".
3. **A session-scoped `ATTACH` is not relayed.** Selecting a satellite session
   from a hub-attached consumer opens that session's **active pane** as a
   window of the consumer's own session and attaches it through the resource
   relay. Choosing it again focuses that window. A consumer that wants the
   satellite's whole layout dials that server: `phux attach --remote`.
4. **An unreachable satellite keeps its row**, marked, alongside the
   un-correlated `ERROR` the aggregate already pushes.

## Why

- **Listing needs no routable id; attaching does.** A name plus counts plus
  one routable pane handle is everything a selector renders. Only the `ATTACH`
  path needs an id the hub can resolve, and that is the one thing this does
  not add.
- **Renumbering is the trap.** A hub-side id space for foreign sessions means
  a mapping table that must survive link churn, satellite restarts, and id
  reuse, and it makes every session verb ambiguous about which server it acts
  on. Closing that door explicitly is the point of this ADR.
- **The pane relay already works.** Reusing it means a satellite session is
  reachable through code paths that already carry input leases, bootstrap,
  and teardown, with no second attach mechanism to keep correct.
- **Feature bit, not a minor bump** ([ADR-0061](./0061-capabilities-add-versions-break.md)):
  the shape is trailing-additive, so peers that do not implement it keep
  interoperating unchanged.

## Tradeoffs

- **The opened window holds the satellite's real Terminal.** Closing it kills
  that pane on the satellite. This is consistent with every other leaf, and
  with a `spawn --satellite` pane, but it is a sharper edge than "close a view".
- **One pane, not the session's layout.** A satellite session with four splits
  opens as its active pane only. The full layout needs a direct attach.
- **The inventory is a poll, not a subscription.** Consumers refresh it on
  `GET_STATE`; a session created on a satellite a second ago shows up on the
  next refresh, unlike local sessions, which arrive live.
- **Counts are per-satellite truth at query time**, and a satellite that goes
  down between the query and the selection turns a row into a bell.

## Alternatives

- **Renumber satellite sessions into a hub id space.** Rejected per Why: a
  durable mapping table and permanently ambiguous session verbs, in exchange
  for an `ATTACH` that would still have to be relayed.
- **Relay session-scoped `ATTACH` to the satellite.** Rejected for now: the
  hub would have to proxy a whole attach lifecycle (bootstrap, layout
  metadata, lease, detach) for a session it does not own. That is a larger
  design that this listing does not preclude.
- **Leave it to `phux ls --json` consumers to stitch by parsing `host/@id`
  selectors.** Rejected: pane ids carry no session membership, so no consumer
  can recover the grouping from them.
- **Do nothing.** Rejected: the fleet's other half stays invisible on every
  surface, which is the reported gap.
