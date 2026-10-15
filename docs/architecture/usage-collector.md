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

[ADR-0166](../adr/0166-usage-collection-is-a-supervised-child.md) is the
decision, and it is still Proposed. This page is the contract an
implementer can build without reopening it. Nothing in the tree spawns
a collector, reads a usage snapshot, or defines `[usage]`. Three
choices are still open for Patrick; they are the Open decisions
section of that ADR. The answers below are the draft.

## Ownership

| Fact | Owner |
|---|---|
| Enabled flag, spawn, restart, and stop | The phux server, at process start |
| Scan, parse, dedup, pricing, ledger, snapshot | The collector process |
| Served reading | The server, by reading the snapshot on command |
| Presentation | A later client; presentation does not collect |
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
   child still exits. On Linux the child calls
   `prctl(PR_SET_PDEATHSIG, SIGTERM)` and, if the parent has already
   gone, exits itself. On macOS the parent holds the write end of a
   pipe and the child exits when a read on that pipe returns EOF. A
   kqueue `NOTE_EXIT` watch on the parent pid is the acceptable
   substitute on macOS.
5. If the child exits while the flag is still enabled, the server
   restarts it. Backoff starts at 1 second and doubles, capped at 30
   seconds. After five consecutive exits the server leaves it down
   until the next server start. Those numbers are hunches. What the
   user sees is fixed: the usage reading is `unavailable`, and
   `phux doctor` prints one line that the collector stopped after five
   restarts and stays down until the next server start.
6. The supervisor waits asynchronously. It does not run on a PTY actor,
   and a collector failure does not tear down the server.

Collection continues while clients are closed, because the child is tied
to the server. A client-owned local PTY outside phux is not collected.

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
| `unavailable` | Enabled, and no snapshot exists, or the supervisor has stopped restarting the child | Omitted |
| `stale` | A snapshot exists and `updated_at_ms` is older than 90 seconds | Last published numbers |
| `ok` | A snapshot exists and `updated_at_ms` is within 90 seconds | That snapshot |

The 90 second window is a hunch: three missed 30 second heartbeats.
While healthy, the child rewrites the snapshot at least every 30
seconds even when totals are unchanged, and at most once every 2
seconds when events arrive. Clients must not treat `stale` or
`unavailable` as a live zero. A supervisor that has stopped
restarting reports `unavailable` and omits totals even when an older
snapshot file is still on disk.

## Who can see the reading

The collector writes a file on the server machine. A client sees a
number only by sending the usage command to the server that collected
it.

In this draft any direct client of that server can request the
reading, local or remote, and the answer includes estimated cost.
Remote means a client attached on the transports in
[transport.md](./transport.md). Mobile is that kind of client: the
phone does not tail files, and it sees a reading only by asking a
server. Whether remote and mobile clients should see cost is open
for Patrick
([ADR-0166](../adr/0166-usage-collection-is-a-supervised-child.md)).

A federation hub does not collect and does not mirror the snapshot.
This slice does not relay the usage command to a satellite, so a
client attached only to a hub cannot see a satellite's reading. That
holds either way Patrick decides the remote-cost question.

## First source and scope

The only source in this slice is Claude Code's append-only JSONL under
each discovered `projects` directory, including
`subagents/agent-*.jsonl`. Root discovery matches Token Tach's Claude
tailer: comma-separated `CLAUDE_CONFIG_DIR`, then `~/.config/claude`,
then `~/.claude`, keeping directories that exist. Reads are read-only.

The collector counts Claude sessions phux did not launch. A Claude
Code process the user started outside phux still lands in these files,
and this draft adds those tokens to the same totals. That default is
open for Patrick
([ADR-0166](../adr/0166-usage-collection-is-a-supervised-child.md)).

This tail is a different record from
[ADR-0156](../adr/0156-agent-transcript-records.md). The Pi and Claude
integrations phux ships append what the pane shows onto the
AgentSession as `provider_raw` records in the `phux.transcript/v1`
convention. `PHUX_AGENT_TRANSCRIPT=full` adds tool output;
`PHUX_AGENT_TRANSCRIPT=0` turns those records off. The collector does
not read that stream and does not consult the variable. Turning
transcripts off leaves collection running, and enabling collection
leaves the transcript setting alone. The collector keeps usage fields
only: token counts, model, message id, and request id.

The collector makes no network calls. It does not fetch a price table,
does not perform Token Tach's OAuth allowance exchange, does not dial
the phux server, and does not upload telemetry. The snapshot file is
the only output.

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

A line is counted only once it is complete. Bytes after the last
newline stay in a per-file carry and are not parsed. The durable
offset sits on a line boundary: the raw read position minus the carry.
A crash therefore resumes at the start of an unfinished line, and a
finished line is not applied twice.

The carry is capped at 1 MiB. Token Tach releases a finished carry
buffer above that size (`carry_shrink_threshold`) and still parses the
line. This contract discards a line that reaches 1 MiB with no
newline, so one transcript line cannot pin the child. The carry is
cleared, the offset advances past the discarded span, and bytes up to
the next newline are skipped. That line is not counted.

If the file's size is below the stored offset, it was truncated or
rotated. The offset resets to 0 and the carry is dropped. If the
path's device and inode differ from the pair stored with the offset,
the path was replaced. The offset resets to 0 and the carry is dropped
even when the new file is longer than the old offset. Dedup keys
survive either reset, so a message already counted stays counted.

A dedup key is `message.id` and `requestId` joined by `:`, or
`message.id` alone. Each stored key is at most 512 bytes. A longer id
is still counted and the offset still advances, and the key itself is
not stored. The persisted set holds at most 65536 keys. Inserting past
that cap drops the oldest key. A later reset that re-reads a file can
count an evicted key again. Those two caps are the growth limits.

The ledger is the collector's state: epoch, rollups, the burn window,
dedup keys, and file offsets. It is not Token Tach's minute, hour, and
day history, and it is not a query API. Writes are atomic in the same
way as the snapshot.

## Price table

Estimated cost comes from a pruned snapshot of the LiteLLM pricing
database, `model_prices_and_context_window.json`. The bytes are Token
Tach's `src/core/fixtures/pricing/model_prices.json` at the commit
pinned below. That prune keeps the claude, gpt, o-series, and codex
families; this slice prices Claude models from it. The collector does
not download the table.

A phux release refreshes the table by replacing the embedded snapshot
and shipping the new binary. There is no runtime refresh. Until that
release, a model the table does not name stays unpriced.

Resolution follows Token Tach. Look up the exact key, then the segment
after the last `/`, then the longest table key that is a substring of
the model id, so a dated id can match its undated row. A model that
still does not resolve adds nothing to `estimated_cost_usd` and
increments `unpriced_events`. The reading shows that count. It does
not show the missing price as zero dollars. `estimated_cost_usd` is
null when every counted event in that total was unpriced, which is
the rule already stated under Delivery.

## What is reused from Token Tach

The source pin is [`phall1/token-tach`](https://github.com/phall1/token-tach)
commit `c4986109698d53a4857fe51321f3668ba9cf32bc` (release 0.10.0,
2026-09-12, the tree read on 2026-10-13). Port means the behavior and
the fixtures from that commit, into the Rust collector. The Zig
sources are not linked. A later Token Tach commit is a pin bump
recorded next to the notice below.

Token Tach is MIT, copyright 2026 phall. The collector keeps that
copyright and permission notice beside the ported parser and the price
snapshot, in a `NOTICE` file in the crate that holds them. The MIT
terms require the notice in every substantial copy.

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
microsecond budget would be tighter than that spread. phux-0ivk.6
compares an enabled server with a disabled server on the same host.
If enabled echo or input latency falls outside the disabled run's own
spread, the collector code does not merge. Further sources wait on
that same result.

Catch-up stays inside the child and yields. It does not run to
completion on one tick of the server.

## Testing

These tests gate the code that implements this page. They are not
required of this design document. phux-0ivk.6 is a merge gate for that
code.

- **Fixtures.** Port Token Tach's Claude fixtures at the pinned
  commit: `src/core/fixtures/claude/session1.jsonl`,
  `agent-sub1.jsonl`, and `garbage.jsonl`, and the parse, dedup, and
  skip-synthetic cases in `src/core/claude.zig` that feed them. The
  Rust parser matches those results.
- **Resume.** One test runs the child, disables it, appends lines,
  enables it again, and crashes it mid-tail. Lines already counted
  stay counted once across disable, enable, and crash. Lines appended
  while it was stopped are counted once. The restarted child reads
  the durable line-boundary offsets and dedup keys.
- **Partial lines.** A chunk with no newline adds nothing to the
  totals. The chunk that supplies the newline counts that line once.
  A line that hits the 1 MiB cap is skipped.
- **Truncate and inode.** Shrinking a file below the stored offset
  resets the offset and re-reads. Replacing the file so the device
  and inode change does the same, including when the new file is
  longer than the old offset. Messages still in the dedup set are
  not counted again.
- **Server stays up.** With the flag false, and with the child
  crashed, including after the supervisor has stopped restarting,
  attach, input, and PTY traffic still work. The usage command
  returns `disabled` or `unavailable` and does not block those paths.
  After the restart cap, `phux doctor` prints the one line described
  under Lifecycle.
- **Latency gate.** phux-0ivk.6, as specified under Budgets, is a
  merge gate for the collector code. A run whose enabled echo or
  input latency falls outside the disabled run's own spread does not
  merge.

## Status

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| Collector process, Claude tail, ledger, snapshot | Designed, not built | [ADR-0166](../adr/0166-usage-collection-is-a-supervised-child.md) | phux-0ivk.2 |
| Server supervision and the usage command | Designed, not built | [ADR-0166](../adr/0166-usage-collection-is-a-supervised-child.md) | phux-0ivk.3 |
| Idle, catch-up, and terminal-latency numbers | Targets above; not measured on phux | [ADR-0166](../adr/0166-usage-collection-is-a-supervised-child.md) | phux-0ivk.6 |
