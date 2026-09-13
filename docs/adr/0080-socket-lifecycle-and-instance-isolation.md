---
audience: contributors
stability: stable
last-reviewed: 2026-08-09
---

# 0080 — Socket lifecycle and instance isolation

**TL;DR.** Socket-file *existence* was standing in for server *liveness* on
both ends, which wedged phux permanently whenever a server died uncleanly and
let an exiting server unlink a live successor's socket. Liveness is now
established by a connect probe, a stale entry is reaped rather than treated as
a server, auto-spawn is serialised by an advisory lock, and a server only
unlinks the inode it bound. Every phux build resolves a **profile** that scopes
the socket, runtime, and state directories, so a development binary cannot
touch the installed one's sessions. Supervision is kept but corrected: restart
on *abnormal* exit only, throttled, with a start history that makes a
crash-loop reportable — and a binary upgraded out from under a running server
now triggers a graceful in-place handoff instead of persisting as version skew.

Status: Accepted
Date: 2026-08-09
Amends: ADR-0055 (`phux service install`'s restart policy is corrected; the
verb remains)

## Context

Field report: "zombie servers, doesn't work, phux fully goofs." The evidence on
the reporting machine was a single `server.log` of 17 MB containing **1487**
`phux server started` records, and a `phux` that printed

```
phux: no server at /tmp/phux-phall/phux.sock. Start one with: phux server
```

on every invocation while no server process existed. Four defects compounded:

**1. Existence used as liveness (client).** Auto-spawn was gated on
`!socket_path.exists()` at five call sites. A Unix socket file outlives the
process that bound it, so a server killed uncleanly left the file behind. From
that moment the gate saw a file and declined to spawn, while the connect found
nothing listening. The wedge was permanent and unrecoverable without a human
running `rm` on a path they had no reason to know about.

**2. Unconditional unlink on exit (server).** The shutdown path called
`remove_file(&socket_path)` with no check that the entry was still the one it
bound. If a second server had taken the path, the first server's *ordinary
exit* deleted the second's socket. The survivor stayed healthy but became
unreachable by name, the next client saw no socket and started a third, and the
cycle sustained itself.

**3. Unserialised auto-spawn.** N concurrent invocations against a cold socket
all observed "no server" and all forked one. The server's own bind-time probe
(`probe → unlink → bind`, non-atomic, 50 ms timeout) was the only arbiter, and
it can be lost by a healthy-but-briefly-busy server — the daemon is a
current-thread runtime (ADR-0003), so any synchronous stall reads as death.

**4. A supervisor over an unreliable process.** `phux service install`
(ADR-0055) generated a launchd `LaunchAgent` with `KeepAlive: true` and no
throttle. Every exit — including every failure above — became an instant
respawn. This is what produced 1487 generations, and, worse, it made a broken
server look like a running one for weeks: the symptom users saw was
intermittent weirdness rather than "the server died."

A fifth issue is adjacent and was making all four hard to diagnose: phux is
developed on the same machine it is used on. A `cargo run` bound the same
socket, wrote the same log, and appeared in the same `phux ls` as the
day-to-day installed build.

## Decision

**Liveness is a connect probe, never a stat.** `phux_config::socket::probe`
classifies a path as `Absent`, `Stale`, or `Live` by attempting a connection.
`Stale` — the file exists, nothing accepts — is treated as "no server": the
entry is reaped and a server is started. Connect errors other than
`ECONNREFUSED`/`ENOENT` classify as `Live`, because the cost of a false `Stale`
(unlinking a healthy server's socket) far exceeds the cost of a false `Live`
(one clear error message).

**A server unlinks only what it bound.** The runtime records the `(dev, ino)`
of the socket at bind time and compares before removing. A path that now names
a different inode belongs to another server and is left alone. When identity
could not be established, nothing is unlinked — a stray socket file is reaped
by the next client's probe, whereas a wrongly deleted one strands a live
server.

**Auto-spawn is serialised by an advisory lock.** A `flock` on
`<runtime_dir>/spawn.lock` elects one spawner; the others wait, re-probe, and
find the winner's server. Failure to acquire the lock is never fatal — it
degrades to the previous unserialised behaviour rather than refusing to start a
terminal. The live-server fast path takes no lock at all.

**Waiting means waiting to accept.** Auto-spawn polls until the socket answers,
not until the file appears; the file exists for a window before the listener is
ready.

**Every build resolves a profile.** `phux_config::instance::profile()` returns
`$PHUX_PROFILE` if set, else `dev` when the binary is not a released artifact
(`debug_assertions`, or an executable under a Cargo `target/` directory), else
`default`. The profile suffixes the runtime directory
(`/tmp/phux-$USER[-<profile>]`, `$XDG_RUNTIME_DIR/phux[-<profile>]`) and the
state directory (`$XDG_STATE_HOME/phux[-<profile>]`), and the service unit —
its launchd label (`com.phux.server[.<profile>]`) and its systemd unit name
(`phux[-<profile>].service`), which together scope the unit's own filename.
The default profile is unsuffixed everywhere, so paths and jobs created by
earlier releases stay valid and addressable.

Isolation is **automatic rather than opt-in**. An environment variable a
developer must remember is not isolation: the one time it is forgotten is the
time a `cargo run` takes over the production socket. `phux doctor` reports a
non-default profile as a warning, since the isolation is otherwise silent and
its only symptom is "my sessions are gone."

Scoping the service unit is the same rule applied to the one path that
originally escaped it. `service install` already wrote profile-scoped socket,
state and log paths, but filed them under a single hard-coded label — so a
dev-profile install silently *replaced* the job supervising the production
server (phux-gyza). A partial isolation is worse than none here: it looks
correct in every path the operator inspects and fails at the one they do not.

**Supervision is corrected, not withdrawn.** An always-on server that survives
logout and reboot is the point of ADR-0055 and worth keeping. What was wrong
was the *policy*, in two specific ways, both now fixed:

- `KeepAlive: true` → `KeepAlive: {SuccessfulExit: false}` (systemd:
  `Restart=always` → `Restart=on-failure`). A clean exit — `phux kill
  --server` — now stays exited. Previously a server could not be stopped.
- No throttle → `ThrottleInterval: 30` (systemd: `RestartSec=30s`). A
  crash-loop becomes two starts a minute instead of a firehose.

**A crash-loop is a reportable fact.** Restart policy alone does not help if
nobody learns the server is dying: a supervised server that crashes and
restarts is externally identical to one that never fell over. Each start
appends a record to `<state_dir>/server-starts.log`, and `phux doctor` fails
the `server-health` check when starts exceed a threshold within an hour. This
is the check whose absence let a broken server pass for a working one.

**Version skew triggers a graceful handoff.** `phux update` already asks the
running server to re-exec (ADR-0032, which passes the listening fd so panes
survive). Every *other* upgrade path — Homebrew, Nix, a distro package —
replaced the binary with no such hook, leaving the old server running
indefinitely. The start history records each server's version, so a client can
see the skew the wire handshake cannot show it (that negotiates the *protocol*
version, not the build). On detecting it, the attach path performs the same
in-place handoff automatically, and `phux doctor` reports it.

## Consequences

- The stale-socket wedge is unreachable: the state that used to require manual
  `rm` now triggers a reap and a spawn.
- A lost bind race costs one server exit instead of a self-sustaining respawn
  cycle.
- Development and production instances coexist. The cost is that a developer
  running a `target/` build genuinely will not see their installed sessions —
  intended, and surfaced by `phux doctor` rather than left to be discovered.
- A deliberately stopped server stays stopped, which is a behaviour change for
  anyone who relied on `KeepAlive` bringing it back.
- A crash-loop now costs up to 30s of downtime per restart instead of
  restarting instantly. That is the intended trade: visible and slow beats
  invisible and fast.
- Attaching with a newer binary restarts the server process (in place, panes
  intact). Users who upgrade mid-session will see one line about it.
- `server.log` is rotated aside at 8 MiB on server start, keeping one previous
  generation. Startup-only rotation still permits one very long-lived, very
  chatty server to exceed the threshold within a single run; bounding that
  needs a rolling appender and is deferred until there is evidence it matters.

## Alternatives considered

**A pidfile.** Rejected as the primary mechanism: a pidfile can be stale in
exactly the same way a socket file can, adds a second thing to keep consistent,
and PID reuse makes "is this pid my server" its own guessing game. The socket
already *is* the liveness signal — it just has to be asked rather than
observed.

**Always unlink before spawning.** Simpler, and it would cure the wedge — by
destroying a healthy server's socket on every invocation. A regression test
pins the opposite direction (`a_second_invocation_reuses_the_live_server`).

**Withdraw supervision entirely until the server is proven.** Tempting, and
the first cut of this work did exactly that. Rejected: it removes a feature
users want in order to fix a defect in its configuration, and it trades one
invisible failure (a silently respawning server) for another (a host with no
server after reboot and no explanation). Throttling alone would also have been
insufficient — it lowers the respawn rate without ever telling anyone the
server is dying — which is why the start history and the `server-health` check
are part of this decision rather than a follow-up.

**Hook each package manager's upgrade path.** Rejected as unbounded: Homebrew,
Nix, distro packages, `cargo install`, and a hand-copied binary all need the
same outcome, and only some of them offer a hook. Detecting skew at attach
time covers every path, including ones that do not exist yet.

**Isolate by socket path alone, leaving the state directory shared.** Rejected:
the 17 MB log was itself a symptom, and interleaving dev and production log
lines in one file recreates the attribution problem the profile is meant to
remove.
