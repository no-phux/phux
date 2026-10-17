---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-17
---

# Resource providers and feeds

**TL;DR.** The target contract for resources that have no PTY. A
provider is an owner-declared program whose process is the activation
of the resource it serves; it dials back under a credential bound to
that one resource and appends records. `FEED` is the first
provider-backed kind, and usage is the first feed. This page is the
target. Nothing here is built.

[ADR-0177](../adr/0177-non-terminal-resources-come-from-providers.md) is
the decision, and it is Proposed. Three choices are open for Patrick;
they are that ADR's Open decisions, and the answers below are the
draft. The wire bytes (kind tag, codec tag, launch-spec encoding, exit
policy, session designation) are allocated in `docs/spec/` by the
implementation change, not here.

## The model in one table

| Noun | What it is | Where it already exists |
|---|---|---|
| Resource | Record, one session, activations, stream, events, L3 scope | [ADR-0102](../adr/0102-resources-the-server-serves-kinds.md), [ADR-0160](../adr/0160-sessions-own-resources-and-window-is-retired.md), [ADR-0162](../adr/0162-organization-records-persist-and-activations-reconcile.md) |
| Kind | The facet: which methods, which codec | `phux_protocol::kinds` ([ADR-0125](../adr/0125-kind-catalog-is-generated-metadata.md)) |
| Provider | A declared program that produces one resource's stream | New. The native ACP host (L1 §5.5.1) is the nearest precedent |
| Activation | One run of the provider process | ADR-0162, with a new `LaunchSpec::Provider` |
| Feed | A provider-backed kind: snapshots and deltas under a schema | New kind `FEED` |
| Schema | The name and JSON shape of a feed's `data`, e.g. `phux.usage/v1` | New, one per feed, documented where defined |

A Terminal is a resource whose activation is a PTY child. An
AgentSession is a resource fed by a producer inside a Terminal. A feed
is a resource whose activation is its own producer. The substrate
(identity, lifecycle, attach, events, inventory, kill, wait, L3 scope)
is the same for all three.

## Declaration

A provider is declared, never created over the wire. Two sources in
the draft:

- **Built-in.** The `phux` binary ships the usage provider. The user
  switch stays `[usage] enabled` (default false), which compiles to the
  declaration below.
- **Owner config.** A `[[provider]]` table.

```toml
[[provider]]
id = "usage"                 # record name, credential principal, state dir
kind = "feed"
schema = "phux.usage/v1"
argv = ["phux", "provider", "usage"]   # no shell; resolved like [launch]
enabled = true
stale-after-secs = 90        # hunch; drives health
```

Plugin manifests (`[provider]`, as `[acp]` is declared) are the open
third source. A remote client, a workload grant, and `SPAWN_RESOURCE`
cannot introduce an argv. They can only relaunch or terminate a
declared provider, under the verbs below.

## Lifecycle

The server reconciles declarations at start and when
`phux.config.reload/v1` rings.

| Declaration | Record | Activation |
|---|---|---|
| Enabled, no record | Created in the services session, named by `id` | Launched |
| Enabled, record exists | Kept | Launched if not live |
| Disabled | Kept, health `stopped` | Terminated; provider state kept |
| Removed from config | Kept, health `stopped`, until `phux rm` | Terminated |

The activation follows ADR-0162: it records `ProcIdentity`, and a
crash marks it `Lost` or `Completed/Unobserved` at the next start.
Reconcile then launches a fresh activation for an enabled declaration.
That relaunch is the declaration's intent, not recovery of user work,
which ADR-0162 still never relaunches.

`on_exit = Restart` applies only to provider kinds. Backoff starts at
1 second, doubles, caps at 30 seconds, and stops after five consecutive
exits. The activation is then `failed` and health is `unavailable`
until a relaunch or the next reconcile. `phux doctor` names the stopped
provider. The numbers are ADR-0166's hunches, unmeasured.

The child exits when the server does. It holds a connection to the
server; when that connection closes and a redial within its backoff
fails, it exits. A parent-death signal (Linux `PR_SET_PDEATHSIG`, a
macOS pipe EOF) is the backstop, as ADR-0166 specified.

## Binding

At launch the server mints a delegated credential
([ADR-0163](../adr/0163-actors-and-delegated-agent-grants.md)):

- principal `provider:<id>`, `on_behalf_of` the owner;
- `INPUT` and `OBSERVE` on the provider's own resource, nothing else;
- revoked when the activation ends.

The child receives the credential, its `ResourceKey`, its activation
id, and a private binding proof in its environment, which is stripped
from anything it spawns. Binding requires the proof and the
kernel-attested owner-uid peer PID of the launched child, as the native
host binding does. A connection bound to an old activation is fenced:
its appends are `NOT_PRODUCER`. No other connection, owner included,
may append to a feed.

A compromised provider can forge only its own feed, and only while its
activation is live.

## The `FEED` kind

Facet:

| Field | Meaning |
|---|---|
| `provider` | The declaration id |
| `schema` | The `data` schema name, e.g. `phux.usage/v1` |
| `health` | Server-derived: `ok`, `stale`, `unavailable`, `stopped` |
| `updated_at_ms` | Server time of the last record |

Health is derived, never reported:

| Health | When |
|---|---|
| `stopped` | Declaration disabled or removed, or the owner terminated it |
| `unavailable` | Enabled, and the activation is not running, or no snapshot has arrived |
| `stale` | A snapshot exists and no record has arrived within `stale-after-secs` |
| `ok` | Otherwise |

A client must not treat `stale` or `unavailable` as a live zero.

Records are one JSON object per line, at most 16 KiB:
`{"seq":u64,"ts_ms":u64,"type":<str>,"data":{...}}`. The server
assigns `seq` and `ts_ms`. `type` is closed:

- `snapshot`: the whole current value. It replaces the previous one.
- `delta`: a change since the last snapshot, under the schema's own
  merge rule. A schema with no merge rule sends only snapshots.
- `heartbeat`: no data; it keeps health `ok`.

The server validates the envelope and does not parse `data`. It keeps
the latest snapshot and the bounded ring after it, ceiling
`defaults.feed-log-bytes` (256 KiB, a hunch). A new snapshot drops the
records before it. Retention is memory only; the provider's next
snapshot after a server restart refills it.

## Methods and authority

A feed answers the substrate and nothing kind-specific beyond the
producer append. The catalog gains a `FEED` `KindSpec`, so
`phux resource methods` and MCP annotations derive from it.

| Action | Carrier | Verb |
|---|---|---|
| List, show | `GET_STATE`, `GetOrganization` | `INVENTORY` |
| Read and watch | `ATTACH_RESOURCE` | `OBSERVE` |
| Lifecycle events | `SUBSCRIBE_RESOURCE_EVENTS` | `OBSERVE` |
| Wait for the provider to exit (`phux resource wait`) | `SUBSCRIBE_RESOURCE_EVENTS` | `OBSERVE` |
| Relaunch | `RESOURCE_RELAUNCH` | `CREATE` |
| Stop | `RESOURCE_TERMINATE` | `TERMINATE` |
| Append | `APPEND_RESOURCE_OUTPUT` | `INPUT`, bound activation only |

Input atoms, resize, and every Terminal facet frame answer
`WRONG_RESOURCE_KIND`. There is no `resource.invoke`. An action such as
resetting the usage epoch is a provider subcommand run by the owner, or
a later ADR that adds a typed method.

## Watching

`ATTACH_RESOURCE` on a feed bootstraps with the latest snapshot and the
records after it, then streams live records. It carries the
[ADR-0159](../adr/0159-agent-streams-resume-from-committed-records.md)
cursor: a reconnecting client asks for records after its last `seq`,
and gets a replacement when the ring no longer holds them. Health and
activation changes are journaled events
([ADR-0123](../adr/0123-events-are-journaled.md)), so `phux watch`
reports them with a cursor.

## Placement and hosts

A provider runs on the authority that declares it and reads only that
machine. Each authority designates one persistent services session, as
it designates Intake. Every provider record lives there. Clients hide
or label it in session lists.

Who sees a feed is who holds `OBSERVE` on that session. The owner
does. A remote or mobile client sees usage cost only if its grant
covers the services session. That replaces ADR-0166's open question 2
with an ordinary grant decision.

A hub lists a satellite's feeds in its satellite inventory and routes
an attach to one as it routes any satellite resource
([ADR-0102](../adr/0102-resources-the-server-serves-kinds.md) §3,
[ADR-0107](../adr/0107-satellite-sessions-are-listed-never-adopted.md)).
It does not sum feeds across hosts. A client that wants a fleet total
attaches each host's feed and adds them itself, and labels the sum as
a client total.

## Consumers

| Surface | Reads | Shape |
|---|---|---|
| CLI | `phux resource read TARGET [--follow] [--json] [--tail N]` | Any feed: facet plus records |
| CLI | `phux usage [--json]` | The usage feed's latest snapshot, formatted; health first |
| CLI | `phux resource show`, `methods`, `wait` | Unchanged; feeds are ordinary resources |
| MCP | `phux_resource_read` | Same document as `resource read --json` |
| MCP | `phux_resource_show`, `phux_resource_methods`, `phux_resource_wait` | Unchanged |
| Client runtime | One feed projection: key, provider, schema, health, snapshot, seq | Shared by every binding |
| TUI | A status widget bound to a feed by provider id | Quiet burn and cost; nothing when disabled |
| Desktop | The same projection; the optional Instrument view | Through `phux-client-runtime` |
| Mobile | The same projection through the UniFFI encoder | Grant-gated as above |

Typed decoders for a schema live in `phux-client` with a committed JSON
Schema, as `phux.mission/v1` does. A client that does not know a
schema shows the provider, health, and update time, and nothing else.
A client that does not know `FEED` keeps the inventory entry and does
not render it as a pane (L1 §1.1 unknown-kind rule).

## Usage as the first feed

Provider id `usage`, schema `phux.usage/v1`. The snapshot `data` is the
document [usage-collector.md](./usage-collector.md) specifies under
Delivery, minus `status`, which health replaces. The collector sends a
snapshot at most every 2 seconds when totals change and a heartbeat
every 30 seconds otherwise. It sends no deltas in v1.

Unchanged from ADR-0166 and usage-collector.md: the Claude Code JSONL
source and its root discovery, the enable epoch and resume rules, the
partial-line carry, truncate and inode handling, dedup caps, the price
table and its resolution, the Token Tach reuse list and MIT notice, the
privacy rule (usage fields only), and the budgets and latency gate
(phux-0ivk.6).

Changed:

| ADR-0166 | ADR-0177 |
|---|---|
| Supervisor beside the runtime | The resource's activation, `on_exit = Restart` |
| `usage/reading.json`, read on command | Snapshot records on the feed's stream |
| New session-scoped JSON command and `features_ext` bit | Substrate methods; the kind is gated by its own bit |
| `status` field written by the collector | `health` derived by the server |
| Any direct client may read | `OBSERVE` on the services session |
| Config read only at start | Reconciled at start and on config reload |
| The child never dials the server | The child dials the server under its bound credential |

The ledger stays the collector's own state, under
`<state dir>/providers/usage/` instead of `<state dir>/usage/`.

## What is not a provider

| Thing | Stays | Why |
|---|---|---|
| Server hooks | The bounded, timed hook queue | Event reactions, not long-lived producers |
| Plugin actions | Client-side commands | They run on demand and own no resource |
| L3 keys (`phux.program-status/v1`, approvals) | Metadata | Last-writer-wins facts about an existing resource or the server |
| Performance telemetry (`GET_PERF`) | In-process, always on | [ADR-0096](../adr/0096-always-on-performance-telemetry.md) |
| Tasks in `phux-work` | Its keeper's store | Durable records with one writer ([ADR-0173](../adr/0173-mission-control-is-a-client-composition.md)) |
| ACP agents | AgentSession under a guardian | [ADR-0170](../adr/0170-acp-is-the-provider-seam.md); a later ADR may move the guardian onto a provider activation |

## Status

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| Provider declaration, activation, `Restart`, binding | Designed, not built | [ADR-0177](../adr/0177-non-terminal-resources-come-from-providers.md) | phux-0ivk.3 |
| `FEED` kind, codec, retention, health | Designed, not built | [ADR-0177](../adr/0177-non-terminal-resources-come-from-providers.md) | phux-0ivk.3 |
| Usage provider (collector) | Designed, not built | [ADR-0166](../adr/0166-usage-collection-is-a-supervised-child.md), [ADR-0177](../adr/0177-non-terminal-resources-come-from-providers.md) | phux-0ivk.2 |
| `resource read`, `phux usage`, MCP, client projection | Designed, not built | [ADR-0177](../adr/0177-non-terminal-resources-come-from-providers.md) | phux-0ivk.3, phux-0ivk.5 |
| Isolation and latency budgets | Not measured | [ADR-0166](../adr/0166-usage-collection-is-a-supervised-child.md) | phux-0ivk.6 |
