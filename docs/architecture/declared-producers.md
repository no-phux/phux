---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-17
---

# Declared producers and feeds

**TL;DR.** The target contract for resources that have no PTY. A
declared producer is an owner-declared program whose process is the
activation of the resource it feeds; it dials back under a credential
bound to that one resource and appends records. `FEED` is the first
kind with one, and usage is the first feed. This page is the target.
Nothing here is built.

[ADR-0177](../adr/0177-non-terminal-resources-have-declared-producers.md)
is the decision, and it is Proposed. Three choices are open for Patrick;
they are that ADR's Open decisions, and the answers below are the
draft. The wire bytes (kind tag, codec tag, launch-spec encoding, exit
policy, held flag, session designation, workload-auth rows) are
allocated in `docs/spec/` by the implementation change, not here.

## The model in one table

| Noun | What it is | Where it already exists |
|---|---|---|
| Resource | Record, one session, activations, stream, events, L3 scope | [ADR-0102](../adr/0102-resources-the-server-serves-kinds.md), [ADR-0160](../adr/0160-sessions-own-resources-and-window-is-retired.md), [ADR-0162](../adr/0162-organization-records-persist-and-activations-reconcile.md) |
| Kind | The facet: which methods, which codec | `phux_protocol::kinds` ([ADR-0125](../adr/0125-kind-catalog-is-generated-metadata.md)) |
| Producer | The caller of `APPEND_RESOURCE_OUTPUT` | L1 §4.8 |
| Declared producer | A producer the server launches from a declaration, as the resource's activation | New. The native ACP host (L1 §5.5.1) is the nearest precedent |
| Activation | One run of the producer process | ADR-0162, with a new `LaunchSpec::Producer` |
| Feed | A kind fed by a declared producer: snapshots and deltas under a schema | New kind `FEED` |
| Schema | The name and JSON shape of a feed's `data`, e.g. `phux.usage/v1` | New, one per feed, documented where defined |
| Producers session | The designated session that holds every producer record on an authority | New, designated as Intake is |

"Provider" is not used here. It already means the harness an
`AGENT_SESSION` reports (its facet field `provider`) and the ACP seam
([ADR-0170](../adr/0170-acp-is-the-provider-seam.md)).

A Terminal is a resource whose activation is a PTY child. An
AgentSession is a resource fed by a producer inside a Terminal. A feed
is a resource whose activation is its own producer. The substrate
(identity, lifecycle, attach, events, inventory, kill, wait, L3 scope)
is the same for all three.

## Declaration

A producer is declared, never created over the wire. Two sources in
the draft:

- **Built-in.** The `phux` binary ships the usage producer. The user
  switch stays `[usage] enabled` (default false), which compiles to the
  declaration below.
- **Owner config.** A `[[producer]]` table.

```toml
[[producer]]
id = "usage"                 # record name, credential principal, state dir
kind = "feed"
schema = "phux.usage/v1"
argv = ["phux", "producer", "usage"]   # no shell; resolved like [launch]
enabled = true
stale-after-secs = 90        # hunch; drives health
```

Plugin manifests (`[producer]`, as `[acp]` is declared) are the open
third source. A remote client, a workload grant, and `SPAWN_RESOURCE`
cannot introduce an argv. They can only relaunch or terminate a
declared producer, under the verbs below.

## Lifecycle

The server reconciles declarations at start and when
`phux.config.reload/v1` rings. The store keeps each declaration's
`ResourceKey`, so reconcile finds its record by key, not by name.

| Declaration | Record | Activation |
|---|---|---|
| Enabled, no record | Created in the producers session, named by `id` | Launched |
| Enabled, record exists, not held | Kept | Launched if not live |
| Enabled, record held | Kept, health `stopped` | Left stopped |
| Disabled | Kept, health `stopped` | Terminated; producer state kept |
| Removed from config | Kept, health `stopped`, unprotected, until `phux rm` | Terminated |

This is the declaration's intent, an exception to L2 §3.2's "no
implicit activation" that covers declared producers only. User work is
still never relaunched.

### Held

A record is held when the owner terminates it (`phux resource terminate
usage`) or when `Restart` reaches its cap. The flag is on the record in
the store, so it survives a restart. Reconcile skips a held record. It
clears on:

- `phux resource relaunch` of that record;
- a reload in which that declaration changed, including a disable and
  re-enable.

Terminate is therefore a stop until the next relaunch or declaration
change. The config switch is the durable off.

### Restart

`on_exit = Restart` applies only to producer-backed kinds. Backoff
starts at 1 second, doubles, caps at 30 seconds, and stops after five
consecutive exits. The activation is then `failed`, the record is held,
and health is `unavailable`. `phux doctor` names the stopped producer.
The numbers are ADR-0166's hunches, unmeasured.

### After a crash

The activation follows ADR-0162 §4 and §5: it records `ProcIdentity`,
and a crash leaves it `Lost` (the process is still there) or
`Completed/Unobserved` at the next start.

- `Completed/Unobserved`: reconcile launches a fresh activation.
- `Lost`: the orphan producer may still be running against a dead
  socket, and a fresh one would write the same ledger. Reconcile
  rechecks `ProcIdentity` (pid, start time, boot), terminates the
  process group by pgid (the producer runs in its own group), waits for
  it to go, and then launches a fresh activation. If the recheck fails,
  the process is gone and the record is `Completed/Unobserved`.

This is the one place reconcile acts on a `Lost` record. ADR-0162 §5
still holds for every other kind: a `Lost` Terminal waits for an
explicit terminate.

### Server lifetime

ADR-0162 §7 lets the server exit when no activation is live and no
client is connected. Producer activations and connections bound to a
`producer:<id>` principal do not count toward that test; otherwise an
enabled producer would keep the server up forever. When the server
exits, by idle or by `SHUTDOWN`, it terminates producer activations
first. The records stay, not held, so the next start relaunches them.

An auto-spawned ephemeral server
([ADR-0063](../adr/0063-ephemeral-server-lifetime.md)) keeps its
idle-exit contract with usage enabled. Usage is collected while the
server runs; the collector's enable epoch and resume rules
([usage-collector.md](./usage-collector.md)) catch up on the next start.

The child also exits on its own when the server is gone: when its
connection closes and a redial within its backoff fails, it exits. A
parent-death signal (Linux `PR_SET_PDEATHSIG`, a macOS pipe EOF) is the
backstop, as ADR-0166 specified.

## Binding

At launch the server mints a delegated credential
([ADR-0163](../adr/0163-actors-and-delegated-agent-grants.md)):

- principal `producer:<id>`, `on_behalf_of` the owner;
- `BIND` and `INPUT` on the producer's own resource, nothing else;
- revoked when the activation ends.

The child receives the credential, its `ResourceKey`, its activation
id, and a private binding proof in its environment, which is stripped
from anything it spawns. Binding requires the proof and the
kernel-attested owner-uid peer PID of the launched child, as the native
host binding does. A connection bound to an old activation is fenced:
its appends are `NOT_PRODUCER`. No other connection, owner included,
may append to a feed.

A compromised producer can forge only its own feed, and only while its
activation is live. It cannot read other resources; it has no
`OBSERVE`, not even on its own feed, since it keeps its own ledger.

## The `FEED` kind

Facet state:

| Field | Meaning |
|---|---|
| `producer` | The declaration id |
| `schema` | The `data` schema name, e.g. `phux.usage/v1` |
| `health` | Server-derived: `ok`, `stale`, `unavailable`, `stopped` |
| `data_at_ms` | Server time of the last `snapshot` or `delta`; a heartbeat does not move it |

Facet command: `APPEND_RESOURCE_OUTPUT` (L1 §5.5). The L1 §1 facet
table gains a `FEED` row with that command and the codec `FeedJsonlV1`.
The command is shared with `AGENT_SESSION`; the target's kind picks the
codec the server validates against.

Health is derived, never reported:

| Health | When |
|---|---|
| `stopped` | Declaration disabled or removed, or the record is held |
| `unavailable` | Enabled, and the activation is not running, or no snapshot has arrived |
| `stale` | A snapshot exists and no record of any type has arrived within `stale-after-secs` |
| `ok` | Otherwise |

Health proves the producer is alive and talking, not that its data is
right: a wedged collector that still heartbeats stays `ok`. A schema
whose data changes rarely, like usage when nothing runs, relies on
heartbeats to stay `ok`. A client shows `data_at_ms` as the data's age
and must not treat `stale` or `unavailable` as a live zero.

Records are one JSON object per line, at most 16 KiB:
`{"seq":u64,"ts_ms":u64,"type":<str>,"data":{...}}`. The server
assigns `seq` and `ts_ms`. `type` is closed:

- `snapshot`: the whole current value. It replaces the previous one.
- `delta`: a change since the last snapshot, under the schema's own
  merge rule. A schema with no merge rule sends only snapshots.
- `heartbeat`: no data; it proves liveness.

The server validates the envelope and does not parse `data`. It keeps
the latest snapshot and the bounded ring after it, ceiling
`defaults.feed-log-bytes` (256 KiB, a hunch). A new snapshot drops the
records before it. Retention is memory only; the producer's next
snapshot after a server restart refills it.

## Methods and authority

A feed answers the substrate plus its one facet command. The catalog
gains a `FEED` `KindSpec`, so `phux resource methods` and MCP
annotations derive from it.

A feed has no parent, so it is its own subject in the
[workload-auth §6](../spec/workload-auth.md) matrix: a row whose subject
is "named Terminal" matches a feed through a Terminal selector naming
its id, or through its session's Group, its Host, or Global. The spec
change states that rule and adds one row; every other carrier below
already exists.

| Action | Carrier | Verbs | Workload-auth row |
|---|---|---|---|
| List, show | `GET_STATE`, `GET_ORGANIZATION` | `INVENTORY` | Existing |
| Read and watch | `ATTACH_RESOURCE` | `BIND` and `OBSERVE` | Existing; the feed is the named subject |
| Lifecycle events, wait | `SUBSCRIBE_RESOURCE_EVENTS` | `OBSERVE` | Existing; the feed is the named subject |
| Relaunch | `RESOURCE_RELAUNCH` | `CREATE` | Existing |
| Stop | `RESOURCE_TERMINATE` | `TERMINATE` | Existing; the feed is the named subject |
| Append | `APPEND_RESOURCE_OUTPUT` to a parentless kind | `BIND` and `INPUT` | New: on the named resource itself, bound activation only. The existing row (on the parent Terminal) stays for `AGENT_SESSION` |

Input atoms, resize, and every Terminal facet frame answer
`WRONG_RESOURCE_KIND`. There is no `resource.invoke`. An action such as
resetting the usage epoch is a producer subcommand run by the owner, or
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

A producer runs on the authority that declares it and reads only that
machine.

### The producers session

Each authority designates one producers session, as it designates
Intake (L2 §4), and the store keeps the designation beside Intake's.

- **Created** by the server at the first reconcile that finds an
  enabled declaration. A server with no producers has none.
- **Persistent.** While designated it refuses `SessionDelete`,
  `SessionKill`, and the switch to `Ephemeral`. Its name is a label;
  rename is allowed, as for Intake.
- **Closed to clients.** `ResourceCreate`, `SessionCreate`, and a move
  that names it as the destination are refused. Only reconcile puts
  records there.
- **Pinned records.** While its declaration exists, a producer record
  refuses `ResourceMove`, `ResourceMoveMany` (the whole batch),
  `ResourceRename`, and `ResourceRemove`. A move cannot silently change
  who sees cost. A removed declaration unpins its record, so `phux rm`
  works. The refusal code is the spec change's; `Unsupported` is the
  draft.
- **Not organized.** The organize judge
  ([ADR-0174](../adr/0174-organize-asks-a-judge-and-code-decides.md))
  excludes the session as a destination and its records as candidates,
  as it excludes Intake as a destination.
- **Counted and labeled.** `GET_ORGANIZATION` reports the designation
  as it reports Intake's (Global only) and counts its records in
  totals like any record. Clients use the designation to hide or label
  it in session lists.

### Who sees a feed

Who sees a feed is who holds `BIND` and `OBSERVE` on the producers
session. The owner does. A remote or mobile client sees usage cost only
if its grant covers that session. That replaces ADR-0166's open
question 2 with an ordinary grant decision.

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
| CLI | `phux usage [--json]` | The usage feed's latest snapshot, formatted; health and data age first |
| CLI | `phux resource show`, `methods`, `wait` | Unchanged; feeds are ordinary resources |
| MCP | `phux_resource_read` | Same document as `resource read --json` |
| MCP | `phux_resource_show`, `phux_resource_methods`, `phux_resource_wait` | Unchanged |
| Client runtime | One feed projection: key, producer, schema, health, data age, snapshot, seq | Shared by every binding |
| TUI | A status widget bound to a feed by declaration id | Quiet burn and cost; nothing when disabled |
| Desktop | The same projection; the optional Instrument view | Through `phux-client-runtime` |
| Mobile | The same projection through the UniFFI encoder | Grant-gated as above |

Typed decoders for a schema live in `phux-client` with a committed JSON
Schema, as `phux.mission/v1` does. A client that does not know a
schema shows the producer, health, and data age, and nothing else.
A client that does not know `FEED` keeps the inventory entry and does
not render it as a pane (L1 §1.1 unknown-kind rule).

## Usage as the first feed

Declaration id `usage`, schema `phux.usage/v1`. The snapshot `data` is
the document [usage-collector.md](./usage-collector.md) specifies under
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
| New session-scoped JSON command and `features_ext` bit | Substrate methods and the `FEED` facet; the kind is gated by its own bit |
| `status` field written by the collector | `health` derived by the server, plus `data_at_ms` |
| Any direct client may read | `BIND` and `OBSERVE` on the producers session |
| Config read only at start | Reconciled at start and on config reload |
| The child never dials the server | The child dials the server under its bound credential, and that connection does not hold the server up |

The ledger stays the collector's own state, under
`<state dir>/producers/usage/` instead of `<state dir>/usage/`.

## What is not a declared producer

| Thing | Stays | Why |
|---|---|---|
| Server hooks | The bounded, timed hook queue | Event reactions, not long-lived producers |
| Plugin actions | Client-side commands | They run on demand and own no resource |
| L3 keys (`phux.program-status/v1`, approvals) | Metadata | Last-writer-wins facts about an existing resource or the server |
| Performance telemetry (`GET_PERF`) | In-process, always on | [ADR-0096](../adr/0096-always-on-performance-telemetry.md) |
| Tasks in `phux-work` | Its keeper's store | Durable records with one writer ([ADR-0173](../adr/0173-mission-control-is-a-client-composition.md)) |
| ACP agents | AgentSession under a guardian | [ADR-0170](../adr/0170-acp-is-the-provider-seam.md); a later ADR may move the guardian onto `LaunchSpec::Producer` |

## Status

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| Producer declaration, activation, `Restart`, held flag, binding | Designed, not built | [ADR-0177](../adr/0177-non-terminal-resources-have-declared-producers.md) | phux-0ivk.3 |
| `FEED` kind, codec, retention, health, workload-auth row | Designed, not built | [ADR-0177](../adr/0177-non-terminal-resources-have-declared-producers.md) | phux-0ivk.3 |
| Producers session designation and protections | Designed, not built | [ADR-0177](../adr/0177-non-terminal-resources-have-declared-producers.md) | phux-0ivk.3 |
| Usage producer (collector) | Designed, not built | [ADR-0166](../adr/0166-usage-collection-is-a-supervised-child.md), [ADR-0177](../adr/0177-non-terminal-resources-have-declared-producers.md) | phux-0ivk.2 |
| `resource read`, `phux usage`, MCP, client projection | Designed, not built | [ADR-0177](../adr/0177-non-terminal-resources-have-declared-producers.md) | phux-0ivk.3, phux-0ivk.5 |
| Isolation and latency budgets | Not measured | [ADR-0166](../adr/0166-usage-collection-is-a-supervised-child.md) | phux-0ivk.6 |
