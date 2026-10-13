---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-13
---

# Usage collector

**TL;DR.** The contract for optional usage collection. A supervised
child tails one user's Claude Code transcripts and publishes a compact
reading; the server only starts, stops, and serves that reading. This
page is the target. No collector is built.

[ADR-0160](../adr/0160-usage-collection-is-a-supervised-child.md) is the
decision. This page is the contract an implementer can build without
reopening it. Nothing in the tree spawns a collector, reads a usage
snapshot, or defines `[usage]`.

## Ownership

| Fact | Owner |
|---|---|
| Enabled flag, spawn, restart, and stop | The phux server, at process start |
| Scan, parse, dedup, pricing, ledger, snapshot | The collector process |
| Served reading | The server, by reading the snapshot on command |
| Presentation | A later client. Cockpit does not collect |
| Token Tach history, UI, and OAuth allowance | Out of this design |

`phux-server` does not depend on the collector. The `phux` binary may.
The server passes the state directory it already resolved
(`telemetry::state_dir`); the child does not resolve a profile itself.
The child does not dial the server and does not receive a socket path
for callbacks.

The collector retains usage fields only. Prompt text, tool bodies, and
credentials from a transcript never enter the ledger or the snapshot.

## Lifecycle

`[usage] enabled` defaults to false in the config file
([ADR-0023](../adr/0023-config-ux-philosophy.md)). The running server
does not watch the file. A change applies at the next server start.

When the flag is false, the server starts no child and does not stat
agent transcripts or the usage directory.

When the flag is true:

1. The server spawns the current `phux` binary as one collector
   subcommand. The subcommand is that process's entry, not a plugin
   slot and not a user-facing query.
2. The child writes under `<state dir>/usage/`, directory mode `0700`,
   files mode `0600`.
3. On server shutdown the server signals the child, waits, then kills
   it. The wait is 2 seconds. That limit is a hunch.
4. If the server process disappears without running that stop, the
   child still exits. Parent-death is part of the contract; the
   mechanism is the implementer's.
5. If the child exits while the flag is still enabled, the server
   restarts it. Backoff starts at 1 second and doubles, capped at 30
   seconds. After five consecutive exits the server leaves it down
   until the next server start. Those numbers are hunches.
6. The supervisor waits asynchronously. It does not run on a PTY actor,
   and a collector failure does not tear down the server.

Collection continues while Cockpit is closed, because the child is
tied to the server. A Cockpit direct-local PTY is not a phux server
session and is not collected.

## Delivery

The collector publishes `usage/reading.json` by writing a temporary
file in the same directory, fsyncing it, and renaming it over the
previous snapshot. The server reads that file only inside the usage
command. It does not poll.

The command is session-scoped and returns JSON, the same result shape
as `GET_PERF`. It is gated on a `features_ext` bit. The command tag and
the bit are chosen when the command lands
([ADR-0137](../adr/0137-server-feature-word-extends.md)). This page
does not assign them.

The document is small enough to stay under the 8 KiB cap below:

```json
{
  "schema_version": 1,
  "status": "ok",
  "source": "claude",
  "epoch_ms": 0,
  "updated_at_ms": 0,
  "tokens": {
    "input": 0,
    "output": 0,
    "cache_creation": 0,
    "cache_read": 0
  },
  "estimated_cost_usd": null,
  "unpriced_events": 0,
  "burn": {
    "window_ms": 900000,
    "tokens": {
      "input": 0,
      "output": 0,
      "cache_creation": 0,
      "cache_read": 0
    },
    "estimated_cost_usd": null,
    "unpriced_events": 0
  }
}
```

`tokens` are exact counts since the epoch. `estimated_cost_usd` sums
priced events only. `unpriced_events` is how many counted events had no
price. Null cost means nothing in that total was priced, not that the
cost was zero. `burn.window_ms` is 900000 (15 minutes), the same window
as Token Tach's burn default.

`status` is one of:

| status | When | Totals |
|---|---|---|
| `disabled` | The flag was false at server start | Omitted. The server does not read a snapshot |
| `unavailable` | Enabled, and no snapshot exists, or the child is down and left none | Omitted |
| `stale` | A snapshot exists and `updated_at_ms` is older than 90 seconds | Last published numbers |
| `ok` | A snapshot exists and `updated_at_ms` is within 90 seconds | That snapshot |

The 90 second window is a hunch: three missed 30 second heartbeats.
While healthy, the child rewrites the snapshot at least every 30
seconds even when totals are unchanged, and at most once every 2
seconds when events arrive. Clients must not treat `stale` or
`unavailable` as a live zero.

## First source and scope

The only source in this slice is Claude Code's append-only JSONL under
each discovered `projects` directory, including
`subagents/agent-*.jsonl`. Root discovery matches Token Tach's Claude
tailer: comma-separated `CLAUDE_CONFIG_DIR`, then `~/.config/claude`,
then `~/.claude`, keeping directories that exist. Reads are read-only.
The server user's files count, including sessions launched outside
phux.

Counted lines are assistant messages with usage objects. Dedup key is
`message.id` plus `requestId`, or `message.id` alone when `requestId`
is absent. Skip `message.model == "<synthetic>"`. Ignore a transcript
`costUSD`; cost comes from the price table.

The reading is one user on one server machine. It does not attribute
spend to a pane, session, or work identity. A later Instrument view
reads this ledger rather than adding a parser. Project-root rollup is
not part of the snapshot and is not required in the ledger yet.

Platforms are the server's: macOS (Apple Silicon release artifacts;
x86_64 from source) and Linux x86_64 and aarch64. Windows is out.
Cockpit remains a macOS client of whatever server it attached to.

## Accounting

On the first enable for a profile, the epoch is the enable instant,
stored in the ledger. Events timestamped earlier are not counted. A
file whose mtime is strictly before the epoch is recorded at EOF
without parsing. A file touched at or after the epoch is parsed; events
before the epoch are dropped. That mtime skip can miss a file copied in
with an old timestamp until a later append.

Disable does not delete the ledger. The next enable keeps the epoch and
the offsets. Lines appended while the collector was stopped are counted
once on resume. A crash while enabled resumes from the last durable
offsets and dedup keys, also without double-counting.

The ledger is the collector's state: epoch, rollups, the burn window,
dedup keys, and file offsets. It is not Token Tach's minute, hour, and
day history, and it is not a query API. Writes are atomic in the same
way as the snapshot.

## What is reused from Token Tach

Read from `phall1/token-tach` at 2026-10-13. Port means the behavior and
the fixtures, into the Rust collector. The Zig sources are not linked.

| Piece | Disposition |
|---|---|
| `src/core/claude.zig` parse, dedup, and fixtures | Reuse. This is the first source. |
| `src/core/pricing.zig` and `fixtures/pricing/model_prices.json` | Reuse. Filtered price snapshot and the cost function. |
| Idle sweep shape in the Token Tach README | Reuse the cadence: about a 2 second sweep, mtime and hot-file checks when nothing changed, a full walk about every 30 seconds. The ~0.1 ms idle figure is Token Tach's, not a phux measurement. |
| `src/core/project.zig` | Not now. Add only if a later view needs project keys, on this ledger. |
| `src/core/fleet.zig` and the other harness parsers | Not now. A later harness is another parser in this process. |
| `src/engine.zig` and `src/cli.zig` sweeps | Do not reuse. They each account the same events. One ledger lives here. |
| `src/core/history.zig` and `statefile.zig` | Do not reuse. `Backfill.needsRun` is only defined and tested; no engine caller was found. Do not read `token-tach` `history/` or `tailers.json`. |
| Popover, dashboard, HUD, OAuth allowance, system sampler | Do not reuse. |

## Budgets

Hard bounds:

- Disabled: no collector process, no transcript or database I/O, no
  usage-command traffic, no wake of a PTY actor for usage.
- Enabled: the PTY actor, the input lane, and the state-sync tick do no
  usage work. The only server I/O for a reading is the on-demand
  snapshot read inside the command.
- Snapshot file under 8 KiB. That cap is a hunch with a wide margin
  over the document above; exceeding it is a bug in the publisher.

Hunches, until the measurement recorded as phux-0ivk.6:

| Target | Number |
|---|---|
| Idle collector CPU | under 1% of one core, sustained |
| Collector RSS after warm start | under 64 MiB |
| Stale threshold | 90 seconds |
| Restart backoff | 1 second, doubled, cap 30 seconds, stop after 5 |
| Shutdown wait | 2 seconds, then kill |

[docs/performance.md](../performance.md) records phux PTY-byte echo
medians of 1934 µs and 278 µs on two runs of one loaded host. A fixed
microsecond budget would be tighter than that spread. The measurement
compares an enabled server with a disabled server on the same host. If
enabled echo or input latency falls outside the disabled run's own
spread, further sources wait.

Catch-up stays inside the child and yields. It does not run to
completion on one tick of the server.

## Status

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| Collector process, Claude tail, ledger, snapshot | Designed, not built | [ADR-0160](../adr/0160-usage-collection-is-a-supervised-child.md) | phux-0ivk.2 |
| Server supervision and the usage command | Designed, not built | [ADR-0160](../adr/0160-usage-collection-is-a-supervised-child.md) | phux-0ivk.3 |
| Idle, catch-up, and terminal-latency numbers | Targets above; not measured on phux | [ADR-0160](../adr/0160-usage-collection-is-a-supervised-child.md) | phux-0ivk.6 |
