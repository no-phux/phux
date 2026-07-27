---
audience: contributors
stability: stable
last-reviewed: 2026-07-27
---

# 0063 — Ephemeral server lifetime

**TL;DR.** `phux server --exit-after-idle SECS` exits once no client
connection has been open for the interval, even with live panes. Opt-in, so
the multiplexer contract — live until the last pane dies — is untouched. The
clock is armed at startup and re-armed on the last disconnect, so both "never
had a client" and "last client left" are covered. No wire change.

Status: Accepted
Date: 2026-07-27

## Context

A phux server is a multiplexer: it outlives the client that started it,
because that is the point. `crates/phux-server/tests/server_self_exit.rs`
pins the one condition under which it stops by itself — the last pane was
reaped after at least one client had been served.

That contract has no exit for an **ephemeral** caller. A harness that
bootstraps a private server per run on a unique temp socket owns a daemon
with no lifetime, and its only lever is tracking the pid and killing it. Any
harness that dies before its cleanup step — a SIGKILL, a panic, a CI runner
reaped mid-job — leaks a daemon holding a live PTY child forever.

This is measured, not hypothetical: 69 live `phux server --session
gc-phux-bootstrap --socket /var/folders/.../gc-phux-<hex>.sock --daemonize`
processes on one developer machine, all sharing a single burst start
timestamp, still running hours later. They were found because their CPU
contention made sub-second tests time out at 80 seconds during an unrelated
`cargo nextest` run.

The cost is already being paid per-caller inside this repo:
`scripts/demo-record.sh` traps on exit to kill its server, and
`crates/phux/tests/*_e2e.rs` each hand-roll the same kill-on-drop guard.
Every one of those is correct and none of them survives a SIGKILL.

## Decision

**A server-local flag, `--exit-after-idle SECS`, threaded to
`ServerConfig::exit_after_idle`.** No new frame, no new command tag, no
`ServerFeature` bit, no version bump — nothing about this crosses the wire
(ADR-0061 §"prefer zero wire change" is satisfied trivially, because a
server's own lifetime is not something a client negotiates).

**"Idle" is zero open client connections, not zero attached clients.**
One-shot control verbs (`phux ls`, `phux send-keys`, `phux new --json`)
connect, issue a `COMMAND`, and leave without ever entering
`ServerState::attached`. Gating on attachment would reap a server between two
`send-keys` calls, which is exactly how the scripted harnesses drive it. The
count is incremented in `accept_loop` and decremented when the client task
ends, so every transport (UDS, WebSocket, QUIC, WebTransport) is covered by
one pair.

**The clock is armed at construction, not at first disconnect.** A server
that has never been connected to is idle from birth. This is not a corner
case — it is precisely the leak shape in the evidence: a daemon bootstrapped
by a harness that then died before dialing in.

**Expiry cancels the root token.** That is the identical signal Ctrl-C
delivers and the identical signal the last-pane self-exit delivers, so panes
tear down through the one graceful path (`TerminalActor::shutdown_pty`
SIGHUPs the pane's process group, waits out `PANE_KILL_GRACE`, and reaps the
child) and the socket is unlinked on the way out. The lifetime chooses *when*
the server stops; it does not get its own idea of *how*.

**The lifetime survives a graceful upgrade.** It rides in `RuntimeFlags` and
is re-emitted on the re-exec argv, because an upgrade that silently dropped
it would promote a bounded harness daemon to an immortal one — this bug,
reintroduced by the one operation whose promise is "same server, new image".

**The default is unchanged and guarded.** Absent the flag nothing differs, and
two tests assert the *absence* case explicitly so that making this a default
fails loudly rather than killing a human's session while they are at lunch.

## Why

Idle-since-last-connection is the only definition that covers the observed
failure. `--exit-with-parent` — the obvious alternative — does not: those 69
daemons were started `--daemonize`, which `setsid(2)`s away from the
launching client, and the auto-spawn parent exits by design moments later.
Tying life to a parent that is *supposed* to leave is tying it to nothing.
It is also not portable: Linux has `PR_SET_PDEATHSIG`, macOS needs a kqueue
`NOTE_EXIT` watcher or a `getppid()` poll, and phux ships both.

Seconds as the unit, minimum 1, maximum 86400: it matches `phux rec
--duration SECS` and `--idle-limit SECS`, and a floor of 1 removes
`--exit-after-idle 0`, which reads like "never" and would mean "immediately".

## Tradeoffs

A long-lived server started with the flag by a wrapper script the operator
did not write will exit while they are away, with no warning beyond a log
line and the startup banner. That is the flag doing its job, but it means
the flag is genuinely dangerous in a config file or a shell alias — which is
why it is not one, and why there is no `defaults.exit-after-idle`.

An open-but-silent connection pins the server alive indefinitely. A harness
that leaks a *socket* rather than a *process* therefore still leaks the
server. Tightening this to "no traffic" was rejected: an attached human
reading a log for an hour sends nothing, and reaping them would be far worse
than the leak.

The flag does not subsume the callers' existing trap/kill cleanup, and was
not used to delete any. A `kill` reclaims the socket now; the lifetime only
bounds the leak. `scripts/demo-record.sh` and the e2e `ServerGuard`s keep
their kill and gain the flag as the backstop for the path where the kill
never runs.

## Alternatives

**`--exit-with-parent`.** Ties the daemon to the spawning process. Incoherent
with `--daemonize` (the case in evidence), unportable across the two
platforms phux ships, and answers "my parent left" rather than "nobody is
using me". Rejected.

**Leave it to callers; document a helper.** Legitimate on its face — the
three in-repo callers already do it. Rejected on the evidence: they each
reimplemented the same trap, and it still leaked, because no in-process
cleanup can survive SIGKILL. A lifetime the *server* enforces is the only
version that holds when the caller does not.

**Make it the default with a large interval.** Would have fixed the observed
leak with no caller changes at all, and would silently break the tmux
contract for every human who closes a laptop. Rejected; the two
absence-of-flag tests exist to keep it rejected.

**Count "idle" as no frames received.** Simpler to reason about and strictly
worse: a human parked in an attached pane sends nothing for minutes at a
time, and would be reaped mid-session. Rejected.
