---
audience: contributors
stability: stable
last-reviewed: 2026-08-15
---

# 0088 — Adopting a live server into supervision

**TL;DR.** Neither launchd nor systemd can restart-supervise a process it did
not start, so "adopt this running server" is not a thing any OS offers, and no
phux verb can pretend otherwise. `phux service install --adopt` therefore
transfers the *supervision*, not the process: it writes the unit and **arms**
it instead of loading it, so the incumbent keeps every pane, nothing
crash-loops on a socket someone else holds, and the supervisor takes over the
first time a server starts after that one exits.

Status: Accepted
Date: 2026-08-15
Builds on: ADR-0055 (`phux service install`), ADR-0080 (the corrected restart
policy and profile scoping), ADR-0083 (in-place unit reconcile)

## Context

`phux service install` refuses when a server already holds the socket
(phux-67wg). The refusal is correct — the supervised process would fail
`bind(2)` on every start, forever — but it leaves a user who has a live server
full of shells and agents with exactly one route to supervision: stop
everything, then install. For a terminal multiplexer the panes *are* the
product, so that is the wrong trade, and phux-m3ot asked for a route that does
not make it.

The bead's own design note proposed reusing ADR-0032's graceful in-place
handoff. That reuse is not available, and the reason matters more than the
verdict:

**ADR-0032 hands off by `execve`.** The old server clears `FD_CLOEXEC` on the
PTY masters and the listener and execs the new binary, so the fds survive
because the process does. `execve` preserves the pid *and the parent* — which
is the one property adoption needs to change. An external process can already
ask for that handoff (`phux upgrade` sends `Command::Upgrade`), but it can only
say "re-exec yourself", never "hand off to me", and afterwards launchd or
systemd still does not own the result.

**A process the supervisor did not start cannot be given fds by it.**
`blob.listener_fd` and `PaneBlob::master_fd` are integers meaningful only
inside the exec'd image's own descriptor table. A server launchd or systemd
spawned independently has an unrelated table, and phux has no `SCM_RIGHTS`
anywhere in the tree and no socket activation in either renderer. Getting live
panes into an independently-spawned process needs a descriptor-passing channel
phux does not have.

**And neither supervisor can adopt a pid regardless.** launchd offers no API to
place an existing process under a job. systemd can enclose existing pids in a
transient *scope*, but `systemd.scope(5)` has no `Restart=` — scopes track
processes, they do not restart them. So even the platform that appears to
support adoption cannot supply the thing supervision is for. This is not a
launchd-versus-systemd asymmetry to paper over; it is the same answer on both,
which is what lets one verb behave identically on both.

## Decision

**Adopt the supervision, not the process.** `--adopt` writes the same unit a
plain install writes, from the same flags — so a listener the operator asks for
here is in the unit, and nothing is recovered from the incumbent — and then
stops short of the one step that would collide: it does not ask the init system
to *start* anything.

**Arming is a real state on both platforms, not a simulation.** launchd
bootstraps every plist in `~/Library/LaunchAgents` when the user's domain comes
up, so writing the file *is* arming it. systemd needs the `WantedBy=` link,
which is what `enable` *without* `--now` writes. Both leave the unit committed
and inert, and both start it at the next login.

**The hand-over is completed by the auto-spawn path.** An armed unit alone is a
promise that never keeps itself: the moment the incumbent exits, the next
`phux` command forks a fresh unsupervised server and the host is back where it
started. So `ensure_server`, having probed and found nothing accepting, gives
an armed unit first refusal — asking the init system to start it instead of
forking — and clears the arming. This is deliberately not a general "prefer the
supervisor whenever a unit exists" rule, which would resurrect a server the
user stopped on purpose and contradict ADR-0080.

**Armed is recorded, not inferred.** A marker under the profile's state
directory names the armed unit. An unloaded unit and a unit whose supervised
server was deliberately stopped are indistinguishable on disk and to `launchctl
print`; recording the state that was entered removes the guess and gives `phux
service status` something true to say. `uninstall` revokes it, and the marker is
swept on sight in two cases: when the unit it names has vanished (the adoption
can never complete), and when the hand-over it describes is already done —
whether this process completed it or `status` observed the init system running
the unit, which is how a marker that outlived a login is retired rather than
repeated.

**A flag, not a verb.** ADR-0083 took the opposite route for `reconcile`
because it resolves nothing and conflicts with every install flag. `--adopt`
is the reverse: it wants all of them, renders the identical unit, and differs
only in whether the unit is loaded. Over a socket with no live server it
degenerates to a plain install, so it is always safe to pass — the flag reads
"never stop a running server to install", and that is exactly what it means.

**What it does not do, in the command's own words.** The running process stays
unsupervised for the rest of its life. `--adopt` says so on stdout, next to the
two commands that would hand over immediately and the panes that would cost.
Printing "installed" and stopping would leave the user believing the one thing
this whole path exists to prevent.

## Tradeoffs

- The live server is never restart-managed. A crash before the hand-over is not
  caught by anything, exactly as before the install.
- Supervision begins at an event the user does not schedule. Predictable and
  reported, but "installed" and "in force" are now separated in time, which is
  one more state to hold — the same trade ADR-0083 made on macOS, now on both
  platforms.
- The marker is state outside the unit, so a hand-edited or externally removed
  state directory silently reverts adoption to "at next login".
- Live pane transfer between two processes stays unbuilt. phux-m3ot's
  acceptance criteria about pane pids surviving *a handoff* are not met, because
  no handoff occurs; the panes survive by not being touched.

## Alternatives

**Build the `SCM_RIGHTS` handoff.** The only design that satisfies the bead
literally: the supervised newcomer connects, the incumbent passes its listener
and every PTY master as ancillary data, the newcomer rebuilds each pane by
replaying the snapshot it already knows how to build. Rejected for now, not
forever — it needs a descriptor-passing channel, a handoff request verb with a
target, a parameterized upgrade so listener config can change, and an
environment-substitution story, none of which exist. It is the correct
post-1.0 shape and this decision does not close it off.

**Let `install` take this path automatically when a server is live.** Rejected
on ADR-0083's grounds: `install`'s contract must not become "sometimes I load
the unit and sometimes I do not", decided by state the user cannot see.

**Install and let it crash-loop until the incumbent exits.** Self-healing on
paper. In practice it is phux-67wg exactly — a failed start every 30s, a
`doctor` crash-loop report that names the wrong cause, and on systemd a unit
that hits `StartLimitBurst` and gives up permanently.

**Stop the incumbent with `SHUTDOWN` and install normally.** Clean, correct,
and precisely the cost the user came here to avoid.
