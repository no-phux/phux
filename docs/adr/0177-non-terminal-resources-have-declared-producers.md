---
audience: contributors
stability: stable
last-reviewed: 2026-10-17
---

# 0177 — Non-terminal resources have declared producers; usage is the first feed

**TL;DR.** A declared producer is an owner-declared program whose process
is the activation of the resource it feeds. It dials back with a delegated
credential and appends records. `FEED` is the first kind with one:
schema-named snapshots and deltas, health derived by the server. Usage is
the feed `usage`, an ordinary resource, so show, wait, attach, grants,
federation, and MCP need no usage-specific wire.

Status: Proposed
Date: 2026-10-17

## Context

[ADR-0102](./0102-resources-the-server-serves-kinds.md) made the resource
the served unit; [ADR-0162](./0162-organization-records-persist-and-activations-reconcile.md)
gave it a persistent record and activations. Both kinds still have a PTY
behind them: an `AgentSession` needs a Terminal parent. The native ACP
host (L1 §5.5.1) dials back with a delegated credential and a binding
proof to feed a precreated resource, but its guardian is a Terminal.
[ADR-0166](./0166-usage-collection-is-a-supervised-child.md) proposed a
supervised child, a snapshot file, and a new JSON command: a second
lifecycle, a second read path, and its own visibility rule. On
2026-10-17 Patrick asked that terminals stop being the only resource.

"Provider" already names the `AGENT_SESSION` facet field and the ACP seam
([ADR-0170](./0170-acp-is-the-provider-seam.md)). The program added here
is a *producer* in L1 §4.8's sense that the server itself launches and
binds: a declared producer.

## Decision

1. **Declared, never sent.** Owner config or a built-in declares a
   producer by id, argv, kind, and schema. No wire command carries its
   argv; it has no shell and no hook queue.
2. **The producer process is the activation.** A new
   `LaunchSpec::Producer` runs a non-PTY child in its own process group,
   with `ProcIdentity`. Exit policy gains `Restart`, for producer-backed
   kinds only, with a bounded backoff.
3. **Declarations reconcile** at start and on `phux.config.reload/v1`: an
   enabled one ensures a record and a live activation unless the record
   is held; a disabled one terminates it and keeps its state. This is a
   declared-producer exception to L2 §3.2's "no implicit activation".
4. **Held records.** An owner terminate, or the `Restart` cap, sets
   `held`; reconcile skips it until a relaunch or a change to that
   declaration. The config switch is the durable off.
5. **A `Lost` producer is terminated, then relaunched.** At start,
   reconcile rechecks its `ProcIdentity`, terminates it by pgid, then
   launches. It is the one reconcile action on a `Lost` record, so two
   producers never write one ledger.
6. **Producers do not keep the server alive.** Their activations and
   their bound connections do not count toward ADR-0162 §7's idle test;
   the server terminates them as it exits. Ephemeral servers keep their
   idle-exit contract.
7. **Binding.** An [ADR-0163](./0163-actors-and-delegated-agent-grants.md)
   credential, principal `producer:<id>`, holds `BIND` and `INPUT` on its
   own resource only, plus a private binding proof checked with the
   owner-uid peer PID, as the native host does. A stale activation is
   fenced. A parentless resource is its own subject in
   [workload-auth §6](../spec/workload-auth.md), which gains one row.
8. **`FEED` is a kind.** Facet state `producer`, `schema` (e.g.
   `phux.usage/v1`), derived `health`, and `data_at_ms`; no parent. Its
   facet command is `APPEND_RESOURCE_OUTPUT`, shared with
   `AGENT_SESSION` under its own codec `FeedJsonlV1`: `snapshot`,
   `delta`, or `heartbeat` records, server-stamped. The server keeps the
   latest snapshot and a bounded ring and does not parse `data`.
9. **No per-producer methods**; ADR-0125's ban on string invoke stands.
   **Watch is attach:** the latest snapshot, then live, with the
   [ADR-0159](./0159-agent-streams-resume-from-committed-records.md)
   cursor.
10. **Placement.** A producer reads only its own authority. Its record
    lives in one designated producers session, created and protected as
    Intake is: records pinned while declared, the session undeletable,
    and the organize judge never sees either. Grants on it decide who
    reads. A hub lists a satellite's feeds and does not sum them.
11. **Consumers.** `phux resource read`, MCP `phux_resource_read`, and
    `phux usage` read feeds; the client runtime projects them once for
    TUI, desktop, and mobile. No client renders a feed as a pane.
12. **Usage is the first feed**, schema `phux.usage/v1`. ADR-0166's
    source, accounting, Token Tach reuse, privacy rule, and budgets carry
    over; its snapshot file, command, `features_ext` bit, and status
    field do not.

Detail: [declared-producers.md](../architecture/declared-producers.md).
If accepted, this supersedes ADR-0166's lifecycle, delivery, visibility.

## Why

Activations already answer start, crash, identity, and outcome; a
supervisor beside them answers the same questions a second way, invisible
to `resource show`, `wait`, and the journal. A one-resource credential
limits a compromised producer to forging its own feed. Server-derived
health proves liveness, not correctness; `data_at_ms` shows the data's
own age. Push replaces polling, and a late viewer gets the current value
from the bootstrap. Session grants replace a usage-only visibility rule.
The second reading is a schema and a declaration, not a wire bump.

## Tradeoffs

- The collector dials the server, which is on its write path, bounded
  by append size, `OVERFLOW`, and the ring ceiling.
- A kind, codec, launch spec, exit policy, held flag, session
  designation, and a workload-auth row, where ADR-0166 needed one
  command and one bit.
- `Restart`, reconcile, and terminating a `Lost` producer are narrow
  exceptions to "nothing respawns", for declared producers only.
- An idle-exited server collects nothing until it starts again; the
  collector's resume rules catch up, as ADR-0166's child had to.
- `FEED` could become a junk drawer. Limits: opaque data, no per-feed
  methods, one documented schema per feed.
- A second protected session that clients hide or label, as Intake.

## Alternatives

**ADR-0166 as written.** Smallest diff, strongest isolation; its child
never dialed the server, so the idle test was untouched. Rejected: a
second lifecycle, a polled side channel, and its own visibility rule,
which each later non-terminal thing would copy.

**A guardian-hosted producer, as the native ACP host runs.** No new
launch spec, and the binding path exists. Rejected: the guardian is a
PTY with an engine, an input lease, and a pane, all unused by a program
with no screen; nothing respawns it, so it needs `Restart` anyway; and
the feed would need a Terminal parent. The native host may later move
onto `LaunchSpec::Producer` instead.

**An L3 key, a typed `USAGE` kind, or the work store.** Rejected: an L3
key has no owner, lifecycle, or order (ADR-0102's reason); a kind per
reading is the `PROCESS_*` pattern ADR-0102 closed; and `phux-work`
holds durable tasks
([ADR-0173](./0173-mission-control-is-a-client-composition.md)), not a
live, bounded reading.

## Open decisions

Patrick has not ratified this draft. It stays Proposed until he does.

1. **Where producer records live.** This draft: a protected producers
   session per authority. The alternative amends ADR-0160 so a resource
   may belong to its authority with no session.
2. **One `FEED` kind or a kind per reading.** This draft has one.
3. **Who may declare producers.** This draft: owner config and built-ins;
   plugin manifests (`[producer]`, as `[acp]`) next. ADR-0166's question
   on counting sessions phux did not launch carries over.
