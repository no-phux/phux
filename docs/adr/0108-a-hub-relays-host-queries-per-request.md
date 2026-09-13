---
audience: contributors
stability: stable
last-reviewed: 2026-09-10
---

# 0108 — A hub relays host queries to satellites per request

**TL;DR.** A federation hub relays a host query that names one of its
satellites, today `LIST_DIRECTORY.host`, over that satellite's link, one
request at a time, and answers with the satellite's own reply. It never relays
metadata, never chains, and turns every routing failure into the query's own
typed refusal (`OTHER`, message naming the host). Satellite links negotiate L3
so the satellite answers.

Status: Accepted
Date: 2026-09-10

## Context

`LIST_DIRECTORY` (`docs/spec/L3.md` §4) lists child directories on the
serving host. On a hub that is the hub's host, so the TUI's directory picker,
opened on a satellite pane, browsed the wrong machine. [ADR-0107](./0107-satellite-sessions-are-listed-never-adopted.md)
made satellite sessions reachable through the pane relay; a pane opened that
way still could not open a sibling window at a directory on its own host.

L3 §1.3 said a satellite link "carries no L3 leg in either direction", and
the hub dialed its satellites offering L1 only. A satellite drops a
`LIST_DIRECTORY` from a consumer that did not negotiate L3 (§1.2), so the
query had no route even in principle.

## Decision

1. **Relay per request.** `LIST_DIRECTORY` gains an optional `host` field. A
   hub that receives one naming a satellite in its registry forwards the
   request on that satellite's link without `host`, under a link-side
   `request_id`, and answers the consumer with the satellite's reply under the
   consumer's `request_id`. The reply is not rewritten: its paths are the
   satellite's. Gated on `ServerFeature::LIST_DIRECTORY_HOST`.
2. **No chaining.** A hub never forwards `host`. A satellite that is itself a
   hub lists its own host.
3. **One refusal shape.** Every routing failure (an unknown host, any `host` on
   a server that is not a hub, a link that is down, connecting, or saturated, a
   satellite without the query, a correlated `ERROR`, the hub's deadline) is a
   `DIRECTORY_LISTING` refusal with `OTHER` and a message naming the host,
   never an `ERROR` and never silence.
4. **Satellite links negotiate L3.** The hub offers L3 in its link `HELLO` and
   keeps the satellite's advertised features, refusing at once a query the
   satellite does not implement.
5. **Metadata still does not federate.** The hub relays no `GET_METADATA`,
   `SET_METADATA`, `LIST_METADATA`, or `SUBSCRIBE_METADATA` over a link; L3
   §1.3's refusals stand.
6. **Bounded like the local query.** The reference hub caps the path at 4096
   bytes, holds at most 8 relayed listings server-wide and 2 per satellite,
   answers a silent satellite with a refusal after 10 s, and abandons a
   request whose consumer disconnects.

## Why

- **A host query is stateless.** It stores nothing on either server and has
  one reply, so relaying it needs only a request-id remap, which the link
  already does for commands. A subscription would need a relayed lifecycle;
  a query does not.
- **The consumer already knows the host.** The focused pane's id carries
  `SATELLITE { host, id }`, so naming the host costs the consumer nothing and
  needs no new addressing.
- **`OTHER` is the code a consumer can act on.** Recovery is the same for
  every routing failure (list somewhere else), and a new code would read as
  `OTHER` on every receiver that predates it. The message says which failure.
- **L3 on the link is inert otherwise.** The hub sends no metadata frames on
  a link, so the extra layer only lets the satellite answer what the hub asks.
- **A per-host cap isolates a wedged satellite.** Without it, one satellite
  holding permits for the whole deadline starves listings on healthy ones.

## Tradeoffs

- **An older hub is misleading, not broken.** It skips `host` and lists
  itself. Consumers must check the feature bit and say which host they show.
- **The listing reads the satellite as its server user**, the identity the
  hub already acts as when it relays a spawn. It exposes nothing a relayed
  shell could not, but it is a second route to that identity.
- **Relayed spawns attach separately.** A window spawned on a satellite from a
  listing needs an explicit attach before content flows; the TUI opens the
  window only when that attach succeeds.

## Alternatives

- **Tell the user to attach to the satellite directly** (`phux --remote`).
  Rejected: it leaves the hub's picker wrong on every satellite pane.
- **Route by the focused pane's id instead of a host field.** Rejected: a
  directory query is not scoped to a pane, and a host field lets a consumer
  list a satellite with no pane open there.
- **Relay all of L3.** Rejected for now: metadata subscriptions need a relayed
  lifecycle and a decision about whose store answers. This ADR does not
  preclude it.
