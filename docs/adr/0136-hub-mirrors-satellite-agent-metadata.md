---
audience: contributors
stability: stable
last-reviewed: 2026-09-21
---

# 0136 — A hub mirrors two satellite agent metadata keys

**TL;DR.** A hub keeps a read-only copy of `phux.agent/v1` and
`phux.agent.asked/v1` for each satellite terminal, retagged
`Local` to `Satellite`. Every other L3 key stays on the server that
owns it. This is not general metadata federation.

Status: Accepted
Date: 2026-09-21

## Context

The fleet and the sidebar are supposed to show every agent the user can
reach, with live state, and the host is only where that agent sits.
[ADR-0108](./0108-a-hub-relays-host-queries-per-request.md) kept L3
server-local: a hub relays `LIST_DIRECTORY.host` and nothing else, and
`SUBSCRIBE_METADATA` on a satellite terminal is refused. That refusal is
right for layout, tags, and every other key. It is wrong for the two
facts the fleet is built from — who the agent is, and whether it is
waiting on a person.

`AgentEvent::Asked` already says a question arrived. A retraction emits
no event ([ADR-0036](./0036-agent-asked-detection.md)), so a
consumer that was not attached at the time cannot learn that the
question went away.

## Decision

1. **Two keys, read-only.** A hub mirrors `phux.agent/v1` and
   `phux.agent.asked/v1` per satellite terminal. It subscribes and reads
   them on the satellite under `Local`, and stores the answer under
   `Satellite { host, id }`. `METADATA_CHANGED` from that store is what
   a hub consumer watches. Client `SET` and `DELETE` of a satellite
   terminal scope are ignored.
2. **The asked flag is a projection.** The owning server writes the byte
   `1` while any ask source still holds, and deletes the key when none
   does. The key is server-owned. No new event is added.
3. **Nothing else moves.** Any other key, `Global` and `Group` scopes,
   and a satellite-tagged id on the link (no chaining) stay as they
   were. A server that does not route the host still refuses the
   subscription with `UNSUPPORTED_SATELLITE_ROUTE`.

## Why

The fleet needs those two facts on the hub the user is attached to.
Copying the whole metadata store would federate layout and every other
convention, which [ADR-0015](./0015-protocol-layering.md) keeps
server-local. A named allowlist is the smaller cut: the refusal remains
the default, and the two keys a consumer already knows how to read
become present.

## Tradeoffs

- The mirror is eventually consistent. A `GET` issued before the
  satellite answers is unset; the following `METADATA_CHANGED` is the
  update.
- A link that is down keeps the last copy until the terminal closes or
  a later value arrives.
- `phux.agent.asked/v1` is a convention on the existing metadata verbs,
  not a new frame.

## Alternatives

- **Subscribe the hub link to every satellite event.** Rejected: that
  journals the satellite's whole stream on the hub.
- **General L3 federation.** Rejected: layout, tags, and session keys
  are not a fleet problem, and a relayed subscription needs a lifecycle
  this carve-out does not.
- **Leave the user to attach to the satellite.** Rejected: the host
  would stay something the user has to think about.
