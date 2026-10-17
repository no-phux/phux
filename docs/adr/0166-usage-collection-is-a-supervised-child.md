---
audience: contributors
stability: stable
last-reviewed: 2026-10-13
---

# 0166 — Usage collection is a supervised child

**TL;DR.** Optional usage is one collector process the server starts and
stops. Hooks and the terminal runtime do not host it. The first reading
is the server user's Claude Code tokens and an estimated cost since
enable, pulled on demand. Token Tach's parser and prices are reused;
its history store and UI are not.

Status: Proposed
Date: 2026-10-13
Superseded in part by [ADR-0177](./0177-non-terminal-resources-come-from-providers.md): Decision §2 lifecycle, §5 delivery, and Open decision 2.

## Context

On 2026-09-12 the product decision was a clean cut from the standalone
Token Tach app: collect while the phux server runs, including with
Cockpit closed; show a quiet burn and estimated cost; keep a richer
Instrument view optional; keep the terminal lean. No Token Tach history
import.

The server is one process per user on a current-thread runtime
([ADR-0003](./0003-server-process-model.md),
[ADR-0014](./0014-server-terminal-pane-actor.md)). Plugin actions and
server hooks already spawn children through `CommandSpec`, but the hook
dispatcher is a bounded queue (`HOOK_EVENT_QUEUE` 64, at most
`MAX_CONCURRENT_HOOKS` 8) and kills each child at `HOOK_TIMEOUT` (30s).
That is an event hook, not a process that tails files for the life of
the server. [ADR-0096](./0096-always-on-performance-telemetry.md) keeps
hot-path histograms inside the binary and always on; usage parsing does
not belong there. Cockpit is a projection client and its direct-local
PTY path is ephemeral, so it must not grow a collector.

Token Tach (`phall1/token-tach`, read 2026-10-13) already tails Claude
Code JSONL and prices tokens. `engine.zig` and `cli.zig` each run a
sweep and a ledger. `Backfill.needsRun` in `statefile.zig` has no
production caller outside that file's tests, so the history store is
not a proven recovery path.

## Decision

1. **Isolation.** One optional collector process. `phux-server` does
   not parse transcripts, open agent databases, or price tokens, and it
   does not depend on the collector crate. The `phux` binary may link
   the collector and spawn itself. Disabled means that process is not
   started and nothing scans agent files.
2. **Lifecycle is supervision, not a plugin.** At server start,
   `[usage] enabled` (default false) decides. The server starts the
   child, kills it on shutdown, and restarts it with bounded backoff if
   it exits while still enabled. The child exits when its parent server
   is gone. Config is not hot-reloaded. Hooks, `CommandSpec`, and
   plugin manifests are not this host, and this does not add a sidecar
   framework.
3. **First source.** Claude Code local JSONL only, for the server user
   on that machine, including sessions phux did not launch. Exact
   assistant token counts; estimated cost from a bundled price table.
   No allowance, OAuth, other harnesses, system telemetry, pane
   attribution, or network calls. This tail is separate from the
   `PHUX_AGENT_TRANSCRIPT` stream
   ([ADR-0156](./0156-agent-transcript-records.md)). Platforms match
   the server install matrix (macOS and Linux; not Windows).
   Cockpit's direct-local PTY is out of scope. Collection follows
   the server, including a remote one.
4. **Accounting starts at first enable.** The epoch is that instant.
   Older lines are not counted. Token Tach's `history/` and
   `tailers.json` are not read. Disable stops the process and keeps the
   ledger; the next enable of the same profile resumes it and counts
   lines appended during the gap, without double-counting.
5. **Delivery and failure.** The collector atomically publishes one
   compact snapshot and does not dial the server. A new session-scoped
   JSON command, read on demand, returns it. Its tag and `features_ext`
   bit are allocated by the implementation commit
   ([ADR-0137](./0137-server-feature-word-extends.md)), not here.
   `disabled`, `unavailable`, and `stale` are explicit and never block
   attach, input, or PTY work. Exact tokens and estimated cost stay
   separate fields.
6. **Budgets.** Disabled and the terminal hot path have hard bounds.
   Idle CPU, RSS, staleness, and restart timing in the contract are
   hunches until measured. The contract, tests, reuse list, and budget
   table are in [usage-collector.md](../architecture/usage-collector.md).

No code in this tree implements the child, the flag, or the command.

## Why

A 30s hook cannot hold a tail, and raising that timeout would still run
unrelated plugin children through the same queue. Parsing JSONL on the
current-thread runtime would put catch-up work beside PTY actors.
`GET_PERF` is the wrong table: it is always on and diagnostic.
Cockpit-owned collection would stop when the app closes and would not
see a remote server's files. One Claude JSONL tail is the smallest
exact source Token Tach already verifies; Codex and OpenCode need
delta or row-replace accounting and can wait. Starting the ledger at
enable avoids both a Token Tach import and a surprise lifetime bill.
Resuming across disable keeps one accounting rule (epoch, offsets,
dedup) instead of a second fast-forward mode.

## Tradeoffs

- A long disable, then enable, replays the gap once. The replay yields
  and stays in the child. It can still be a burst of disk reads.
- Skipping files whose mtime is before the epoch can miss a transcript
  copied into place with an old mtime until something appends to it.
- User-level totals do not say which pane spent the tokens. Later views
  extend this ledger; they do not open a second parser.
- The `phux` binary grows by the collector and the price snapshot even
  when the feature is off. `phux-server` does not.
- Two enabled profiles for one user each run a collector over the same
  Claude tree, with separate ledgers. That matches profile isolation
  ([ADR-0080](./0080-socket-lifecycle-and-instance-isolation.md)).
- Idle CPU, RSS, the 90s stale window, and the restart cap are hunches.
  phux-0ivk.6 is a merge gate for the collector code.

## Alternatives

- **In-process collection.** Rejected. Catch-up and SQLite would sit on
  the terminal runtime, and a parser panic would take every session.
- **A long-lived plugin hook.** Rejected. The hook host is bounded and
  timed out, and a general sidecar API is a framework this one feature
  does not earn.
- **Import Token Tach history and UI.** Rejected. The history backfill
  gate is unproven in production, and the UI is a menu-bar app Cockpit
  does not absorb.
- **Count the whole Claude tree on first enable.** Rejected. Fresh
  accounting starts at enable. The files stay the source of truth after
  that.
- **Codex or OpenCode first.** Rejected for this slice. Claude JSONL is
  append-only exact usage with no network. Other harnesses are later
  parsers behind the same process.
- **Fold readings into `GET_PERF`.** Rejected. Performance telemetry
  stays always-on and in-process. Usage stays optional and out of
  process.

## Open decisions

Patrick has not ratified this draft. It stays Proposed until he does.

1. **Sessions outside phux.** This draft counts every Claude Code
   session on the server user's machine, including ones phux did not
   launch. The alternative is sessions phux launched, only.
2. **Remote and mobile cost.** This draft serves tokens and estimated
   cost to any direct client, including a remote or mobile one. The
   alternative is local clients only. Federation sees no reading here.
3. **Status.** Proposed until those two are settled. Accepted
   (forward-compat) is the status once Patrick ratifies them.
