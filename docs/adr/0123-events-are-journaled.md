---
audience: contributors
stability: stable
last-reviewed: 2026-09-14
---

# 0123 — Events are journaled, resumable, and never dropped silently

**TL;DR.** The server stamps every event with a server-wide sequence, a
time, an actor, and the operation that caused it, keeps a bounded journal,
replays it to a subscription that names a cursor, and reports every loss as
a typed gap. Both subscribe verbs share one registry. The journal is a
wake-up over state read by level, not durable history.

Status: Accepted
Date: 2026-09-14

## Context

`EVENT` carries no sequence, cursor, timestamp, or attribution beyond the
numeric connection ids inside `terminal_control`. Delivery is a non-blocking
send into a small per-connection mailbox, and an overflow drops the event
with no signal. Two registries feed subscribers: a client subscribed through
both `SUBSCRIBE_EVENTS` and `SUBSCRIBE_RESOURCE_EVENTS` receives duplicates,
and the second path never delivers `bell`, `title_changed`, or `asked`. A
watcher that reconnects cannot resume, and a waiter that subscribes after a
pane exited cannot tell that exit from a pane that never existed. The spec
therefore called the stream an accelerator, not a contract.

Orchestrators now want to wait on exits, resume after a disconnect, and audit
who took the wheel. Each of those needs an order, a cursor, and an honest
account of what was missed. `RESOURCE_OUTPUT` already has that discipline
through its tombstones; events have none of it.

## Decision

1. **Stamp.** `EVENT` gains additive fields 3 `seq: u64`, 4 `ts_ms: u64`,
   5 `actor: ActorRef`, and 6 `operation_id: bytes16`. `seq` is one
   server-wide sequence that starts at 1, is assigned once under the state
   lock, and never wraps. Every subscriber sees the same stamp.
2. **Journal.** The server keeps its most recent stamped events in a bounded
   ring: 4,096 events and 1 MiB by default, both configurable, evicting whole
   events oldest first. It lives in memory and a restart empties it.
3. **Cursor.** `SUBSCRIBE_EVENTS` gains field 2 `after_seq: u64`. The server
   installs the subscription and cuts the replay together, replays retained
   events with a greater `seq` in the subscription's scope, then goes live. A
   cursor is `(HELLO_OK.server_id, seq)`; a changed `server_id` voids it, the
   incarnation rule of ADR-0053. A cursor past the newest `seq` is void
   and answered with a `journal_gap`, except `2^64 - 1`, which asks for
   journal semantics with no replay.
4. **Gaps, not drops.** Delivery stays non-blocking. A replay the ring cannot
   cover, or a send the connection cannot take, becomes one `journal_gap
   { first_missing, last_missing }` (tag `0x0b`) ahead of the next delivered
   event; it is a per-subscription notice and is never journaled. Events a
   resource produced faster than the server could journal them become a
   journaled `source_gap { dropped }` (tag `0x0c`) for that resource. A gap
   tells the consumer to re-read level state.
5. **One registry.** `SUBSCRIBE_RESOURCE_EVENTS` keeps its wire shape and
   becomes a filtered scope in the `SUBSCRIBE_EVENTS` registry. Duplicates
   end, every event type reaches both, and re-subscribing replaces a filter.
6. **Attribution.** `ActorRef = { client: u32, credential_id: optional<str>,
   client_name: optional<str> }`. The server keeps each connection's
   `HELLO.client_name`. `METADATA_CHANGED` gains field 4 `actor`. Through a
   federation hub the actor of a satellite's event is the hub's link.
7. **No per-event schema version.** The tag under the negotiated
   `PROTOCOL_VERSION` and the feature bit are the schema.
8. **`EXPIRED`.** `ControlAction` gains `EXPIRED = 9` for an input lease that
   reached its `ttl_ms`. A server sends it only to a subscription opened with
   `after_seq`, and reports the transition as `RELEASED` to any other.
   Decoders from this draft on read an unknown `lifecycle` or `action` as an
   opaque event instead of failing the frame.
9. **Gate.** `ServerFeature::EVENT_JOURNAL = 0x01000000`.
10. **`TERMINAL_EVENT` is retired.** The spec-only frame at `0xB1` would have
    been a second carrier for facts that already ride `EVENT` tags and
    `GET_TERMINAL_STATE`. The byte stays reserved and is never reused.
11. **Consumer surface.** This amends the ADR-0071 freeze: `phux watch
    --after CURSOR` resumes a watch, and `phux resource wait TARGET
    [--timeout SECS] [--after CURSOR] [--json]` with MCP `phux_resource_wait`
    waits for a resource to exit or disappear. A wait is a cursor subscription
    followed by a `GET_STATE` on one connection; there is no wait command.

The normative text is `docs/spec/L1.md` §3.3, §7, §7.1, and §7.3,
`docs/spec/L3.md` §1, and `docs/spec/proto.md` §6.2.

## Why

- **One order.** A fleet waiter watches many resources; a per-resource
  sequence would make every cursor a vector.
- **Loss is data.** Binding loss to the journal and reporting it on the wire
  is the `BOOTSTRAP_TOMBSTONE` discipline applied to events. A consumer never
  has to guess whether silence means nothing happened.
- **The event path never waits.** Keeping `try_send` means a slow reader
  costs itself a gap and costs the server nothing.
- **A wait composes.** Subscribe-then-read on one connection is race-free
  because frames are processed in order: a close after the read arrives as an
  event, a close before it is visible in the read. Timeout, cancel, and resume
  stay client concerns and work through a hub unchanged.
- **`EXPIRED` cannot break a deployed client.** A 0.9.0 decoder fails the
  frame on an unknown `action`; only a subscriber that sent a cursor is known
  to decode the new value.
- **One carrier per fact.** Events already carry title, cwd, and command
  boundaries, so `TERMINAL_EVENT` would only have duplicated them.

## Tradeoffs

- The ring costs up to 1 MiB of server memory by default.
- The journal is not durable: after a restart every consumer re-reads, and a
  gap spanning a restart is reported as a new incarnation, not a range.
- A scoped subscription's gap names global sequences, so a scoped consumer
  cannot tell whether the missed events concerned it and must re-read.
- Attribution through a hub is the link, not the person behind it; that
  refinement stays deferred with ADR-0038.
- `client` ids are connection-scoped and mean nothing after a reconnect;
  `credential_id` exists only on paired routes.
- The server rewrites `EXPIRED` per subscriber, a small cost on a rare event.

## Alternatives

- **Per-resource sequences.** Rejected: a multi-resource cursor becomes a
  vector, and a fleet view loses its order.
- **A broadcast channel with a lag error.** Rejected: it would still need a
  wire gap, and it would tie loss to a channel size instead of the journal.
- **Durable event history.** Rejected: durable work state belongs to the
  coordinator endpoint (ADR-0097), and ADR-0103 keeps live streams bounded.
- **A `WAIT_RESOURCE` command.** Rejected: a server future per waiter with its
  own timeout and cancel vocabulary, new relay semantics, and it would still
  need the journal to survive a reconnect.
- **A `schema_version` on each event.** Rejected: it versions the same bytes
  twice, and ADR-0071 point 6 already makes the event name the contract.
- **Keep `TERMINAL_EVENT` as a spec-only reservation.** Rejected: an unbuilt
  second carrier invites a second implementation of the same facts.
