---
audience: contributors
stability: stable
last-reviewed: 2026-10-13
---

# 0162 — Organization records persist; activations reconcile

**TL;DR.** Each server keeps its sessions, resources, arrangements, and
receipts in a transactional SQLite store and commits every change there
before applying it in memory. Each execution is an activation recorded
with the process identity behind it. After a crash the server reconciles
those activations, marks each lost or unobserved, and relaunches nothing.
An exit policy replaces respawn-in-place.

Status: Accepted
Date: 2026-10-13

## Context

Organization lives only in server memory. A crash loses every session name
and layout; `--autosave` rewrites a JSON archive and restores it on a cold
start by launching fresh shells
([ADR-0150](./0150-the-server-keeps-the-restore-archive-current.md)).
When a session's last shell exits naturally, the server execs a new one in
the same Terminal ([ADR-0131](./0131-last-shell-exit-keeps-a-terminal.md)),
so `exit` cannot end a session. Exited-pane retention is a per-spawn flag
([ADR-0124](./0124-retain-on-exit.md)). The server exits once it holds no
sessions after serving a client
([ADR-0063](./0063-ephemeral-server-lifetime.md)).
[ADR-0160](./0160-sessions-own-resources-and-window-is-retired.md) makes
sessions persistent records, which needs durable storage, and agents
organizing thousands of resources need indexed queries and atomic bulk
moves, not a whole-file rewrite per change.

## Decision

1. **The store.** `OrgStore` is SQLite through `rusqlite` with the
   `bundled` feature, in WAL mode with `synchronous=FULL`, at
   `<state_dir>/org/v1.sqlite3`. `ServerConfig` names its location as a
   path or memory; in-process tests use memory and every test server gets
   its own `XDG_STATE_HOME`. It holds the `AuthorityId`, the designated
   Inbox session, sessions and intake marks, resources with activations,
   arrangements, tombstones (30 days), operation receipts (24 hours or
   10,000, whichever comes first), migration receipts, and the last
   journal sequence. Terminal output is not stored here
   ([ADR-0130](./0130-on-disk-pty-journal-is-not-built.md) stands).
2. **Write discipline.** `ServerState::commit` is the only mutation path:
   apply the command in `phux-core`, commit the transition, its receipt,
   and the journal position in one store transaction, then update memory,
   acknowledge, and publish events. A failed commit refuses the command and
   leaves memory unchanged. Commits run inline on the runtime thread; the
   input lane is a separate thread and is unaffected. Schema migrations are
   versioned, transactional, and backed up first; an unsupported store
   version is refused, not guessed at.
3. **First start.** The server mints an `AuthorityId`, imports the live
   registry and each session's `phux.tui.layout/v1` document with groups
   intact, designates a persistent Inbox session, and marks imported
   sessions awaiting organization. A migration receipt makes the import
   idempotent.
4. **Activations.** An activation records its id, the server instance
   that started it, its state (starting, running, stopping, completed,
   failed, lost, abandoned), its outcome, and for a Terminal its
   `ProcIdentity { pid, pgid, start_raw, boot_id }`. `start_raw` is the
   platform's raw process start time; `boot_id` distinguishes boots.
5. **Startup reconciliation.** For each activation recorded live under a
   previous instance: a live-upgrade handover rebinds it; otherwise a pid
   that exists on the same boot with the same start time is marked `Lost`;
   anything else is `Completed` with outcome `Unobserved`. Nothing is
   relaunched. A `Lost` resource refuses remove and relaunch until it is
   terminated by pgid after a recheck or explicitly abandoned. Resources
   ended by the crash stay as records, flagged, until `phux restore`
   relaunches them.
6. **Exit policy.** Each resource has `on_exit: Remove | Retain`. A shell
   started by `new`, `spawn`, or a split is `Remove`: exiting it removes
   the resource. A launched command, an agent, or anything started with
   `--keep` is `Retain` and keeps its outcome, using ADR-0124's retention
   mechanism. Nothing respawns implicitly.
7. **Server lifetime.** The server may exit when no activation is live and
   no client is connected, after the existing idle backstop. Persistent
   empty sessions do not hold it up; their records survive in the store.
   Auto-spawned ephemeral servers keep their idle-exit contract.
8. **Archives.** `phux archive export` writes a portable copy without live
   handles, credentials, or receipts. Import creates records with
   provenance and never launches; relaunch is a separate explicit step.
   `--autosave` remains for one release as an export schedule.

This supersedes ADR-0131, and in part ADR-0124 (the per-spawn retention
flag), ADR-0063 (the last-pane self-exit), and ADR-0150 (restoring the
archive on a cold start).

**Dependency.** `rusqlite` is the one new dependency. Atomic bulk moves,
durable receipts for idempotent retry, and indexed membership and intake
queries under automation load need transactions and indexes; a JSON file
rewritten per change does not scale to thousands of records and cannot
make a receipt and its effect atomic. No SQLite binding exists in the
tree. `bundled` pins the SQLite version instead of depending on the host's.

## Why

Records that outlive the process are what makes a named session mean
something after a crash. Committing before applying in memory means a
failed write cannot leave clients told about a change that is not durable.
Recording process identity, rather than trusting a pid, is the only way to
tell a surviving orphan from a reused pid after restart. Relaunching
nothing automatically keeps a crash from silently rerunning commands with
side effects. An explicit exit policy gives `exit` its tmux meaning while
keeping agent outcomes inspectable.

## Tradeoffs

- A synchronous fsync per commit sits on the runtime thread; a perf gate
  pins commit latency.
- A crash no longer brings shells back on its own. The user runs
  `phux restore`, and sees what was there in the meantime.
- Downgrading to a binary without the store is not safe by ignoring it; it
  requires an explicit export and a stopped owner.
- One more native dependency, compiled from C source.

## Alternatives

**JSON file, rewritten atomically.** Simple, but every change rewrites
every record, and receipts cannot commit atomically with their effects.

**An embedded key-value store.** It would need hand-built secondary
indexes and migrations for the membership and intake queries SQLite
answers directly.

**Keep respawn-in-place.** It makes `exit` never end a session and blurs
what ran; the explicit exit policy covers the GUI case without it.
