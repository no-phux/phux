---
audience: contributors
stability: stable
last-reviewed: 2026-10-17
---

# 0177 — Non-terminal resources come from providers; usage is the first feed

**TL;DR.** A provider is an owner-declared program whose process is the
activation of the resource it serves. It dials back with a delegated
credential and appends records. `FEED` is the first provider-backed kind:
schema-named snapshots and deltas, health derived by the server. Usage is
the feed `usage`. It is an ordinary resource, so show, wait, attach,
grants, federation, and MCP need no usage-specific wire.

Status: Proposed
Date: 2026-10-17

## Context

[ADR-0102](./0102-resources-the-server-serves-kinds.md) made the resource
the served unit; [ADR-0162](./0162-organization-records-persist-and-activations-reconcile.md)
gave it a persistent record and activations;
[ADR-0125](./0125-kind-catalog-is-generated-metadata.md) made one table its
catalog and classifier. Both kinds still have a PTY behind them: an
`AgentSession` ([ADR-0103](./0103-agent-session-resource-and-producer-fed-streams.md))
needs a Terminal parent. The native ACP host (L1 §5.5.1) already dials
back with a delegated credential and a binding proof to feed a precreated
resource, but its guardian is a Terminal.

[ADR-0166](./0166-usage-collection-is-a-supervised-child.md) proposed a
supervised child writing a snapshot file, read by a new JSON command: a
second lifecycle beside activations, a second read path beside streams,
and its own visibility rule. On 2026-10-17 Patrick asked that terminals
stop being the only resource and questioned that framing. More
non-terminal things are queued: other harnesses' usage, the reserved
`PROCESS_*` and `FORWARD_PORT` space, builds on a devbox.

## Decision

1. **A provider is a declared program, not a plugin host.** The owner's
   config, or a built-in, declares it by id, argv, kind, and schema. A
   remote client cannot declare one, and no wire command carries a
   provider argv. A declaration has no shell and no hook queue.
2. **The provider process is the resource's activation.** A new
   `LaunchSpec::Provider` runs a non-PTY child through the launch
   executor. Its activation carries `ProcIdentity` and reconciles after
   a crash as ADR-0162 says. Exit policy gains `Restart`, for provider
   kinds only, with a bounded backoff. After the cap the activation is
   `failed` until the next reconcile.
3. **Declarations reconcile.** At start and on `phux.config.reload/v1`,
   an enabled declaration ensures one record named by its id and a live
   activation. A disabled one terminates the activation and keeps the
   record and the provider's own state. This is reconciliation from
   declared intent, not ADR-0162's forbidden relaunch of user work.
4. **Binding and authority.** The server mints an
   [ADR-0163](./0163-actors-and-delegated-agent-grants.md) credential with
   principal `provider:<id>`: `INPUT` and `OBSERVE` on its own resource,
   nothing else. The child receives it with a private binding proof and
   its activation id. Binding checks the proof and the owner-uid peer
   PID, as the native host does. A stale activation is fenced.
5. **`FEED` is a kind.** Facet: `provider`, `schema` (for example
   `phux.usage/v1`), and server-derived `health`: `ok`, `stale`,
   `unavailable`, or `stopped`. No parent. The codec is JSONL records
   whose type is closed to `snapshot`, `delta`, and `heartbeat`, with
   server-stamped `seq` and `ts_ms`. The server keeps the latest snapshot
   and a bounded ring after it, and it does not parse `data`. Tags for
   the kind and codec are allocated with the spec change.
6. **Methods are the substrate.** Show, inventory, attach, events, wait,
   relaunch, and terminate apply as to any resource, plus the producer
   append. A feed adds no per-provider wire methods. A provider action is
   a client verb or a later ADR; ADR-0125's ban on string invoke stands.
7. **Watch is attach.** `ATTACH_RESOURCE` bootstraps from the latest
   snapshot and goes live, with the
   [ADR-0159](./0159-agent-streams-resume-from-committed-records.md)
   resume cursor. Health changes and activation changes ride the event
   journal ([ADR-0123](./0123-events-are-journaled.md)).
8. **Placement.** A provider runs on the authority that declares it and
   reads only that machine. Its record lives in one designated
   persistent services session, as Intake is designated. Grants on that
   session decide who reads it. A hub lists a satellite's feeds like any
   resource and does not sum them. A multi-host client attaches each
   host's feed.
9. **Consumers.** `phux resource read TARGET [--follow] [--json]` and
   MCP `phux_resource_read` print any feed; `phux usage` formats one. The
   client runtime projects feeds once for the TUI status widget, the
   desktop, and mobile. No client renders a feed as a pane.
10. **Usage is the first feed.** Provider id `usage`, schema
    `phux.usage/v1`. ADR-0166's source, accounting, Token Tach reuse list,
    privacy rule, and budgets carry over unchanged. Its snapshot file,
    usage command, `features_ext` bit, and status field do not.

Detail: [resource-providers.md](../architecture/resource-providers.md).
This supersedes ADR-0166's lifecycle, delivery, and visibility, and turns
its remote and mobile cost question into an ordinary grant.

## Why

Activations already answer start, crash, identity, and outcome for
processes. A supervisor beside them would answer the same questions a
second way and would not appear in `resource show`, `wait`, or the
journal. A credential bound to one resource is an existing mechanism, and
it limits a compromised provider to forging its own feed. Server-derived
health means a provider cannot report itself fresh. A snapshot-plus-delta
stream gives the TUI and desktop push instead of polling, and a late
viewer gets the current value from the bootstrap. Grants on a session
replace a usage-specific rule about who may see cost. One generic kind
means the second reading is a schema and a declaration, not a wire bump.

## Tradeoffs

- The collector now dials the server, which is on its write path,
  bounded by append size, `OVERFLOW`, and the ring ceiling.
- A kind, codec, launch spec, exit policy, and session designation are
  wire and store changes. ADR-0166 needed one command and one bit.
- `Restart` is a narrow exception to "nothing respawns", for declared
  providers only.
- `FEED` could become a junk drawer. Its limits: opaque data, no
  per-feed methods, one documented schema per feed.
- Clients must hide or label the services session, as they do Intake.

## Alternatives

**ADR-0166 as written.** Smallest diff, strongest isolation, no new kind.
Rejected: a second lifecycle, a polled side channel, and its own
visibility rule, which each later non-terminal thing would copy.

**An L3 key the collector writes, such as `phux.usage/v1`.** No wire
change and subscribable today. Rejected for the reasons ADR-0102 gave for
agent sessions: no owner, no lifecycle, no order, and any `BIND` holder
could overwrite it.

**A typed `USAGE` kind.** Explicit facet fields. Rejected: a kind per
reading is the `PROCESS_*` pattern ADR-0102 closed.

**The work store.** Rejected: `phux-work` holds durable tasks under one
keeper ([ADR-0173](./0173-mission-control-is-a-client-composition.md));
a reading is live and bounded.

## Open decisions

Patrick has not ratified this draft. It stays Proposed until he does.

1. **Where provider records live.** This draft uses one designated
   services session per authority. The alternative amends ADR-0160 so a
   resource may belong to its authority with no session.
2. **One `FEED` kind or a kind per reading.** This draft has one kind.
3. **Who may declare providers.** This draft: owner config and built-ins;
   plugin manifests (`[provider]`, as `[acp]`) next. ADR-0166's question
   on counting sessions phux did not launch carries over unchanged.
