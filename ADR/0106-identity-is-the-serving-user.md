---
audience: contributors
stability: stable
last-reviewed: 2026-09-10
---

# 0106 — Identity is the serving user; whoami reports it

**TL;DR.** A phux server never switches OS users: the effective user of
every pane is the user the server runs as (ADR-0003). `root@host` reaches
root's own server, not a privilege change inside someone else's. A new
read-only `phux whoami` reports the principal, auth route, serving user and
host for the connection. System-login integration (PAM, utmp, loginctl) stays
with the service manager, not the server.

Status: Proposed
Date: 2026-09-10

## Context

The Superlogical reconstruction (`research/2026-09-09-superlogical-demo/GAP.md`,
F06, F07 and F12 to F14) shows a CLI `whoami` against a remote host that
reports the principal, auth route and effective user. It also shows choosing
root as the effective user and server-side mapping of authenticated
principals to allowed OS users, with each session a real system login.

phux has no equivalent. One server runs per OS user (ADR-0003). The remote
`user@` is a label that names the ssh destination and registry key, not a
wire identity (`crates/phux/src/commands/remote_target.rs`). Remote
connections authenticate with a bearer credential whose `principal` the
server records (`crates/phux-server/src/auth.rs`, ADR-0031), or delegate
authentication to ssh through `stdio-bridge`. Panes are PTY children of the
daemon, and only service-managed servers start a login shell.

## Decision

1. **No user switching.** The server does not setuid, run privileged, or
   map principals onto other OS users. The effective user of a pane is
   always the serving user.
2. **`user@host` selects a server.** Reaching root means reaching root's
   own server (its port, registry entry or ssh-stdio route), which is the
   existing `--remote` resolution. Host-grouped selectors list `root@host`
   and `me@host` as separate entries.
3. **`phux whoami` is read-only and covers every transport.** It reports:
   - the credential id and principal (bearer), or the peer uid (UDS)
   - the auth route (UDS peer credentials, QUIC or WSS bearer, or ssh-stdio)
   - the serving OS user (uid and name) and host name
   - the server version

   These are served through one read-only `Global` metadata key. The spec
   change lands with the implementation.
4. **Login semantics belong to the service manager.** Real logins (utmp
   entries, `loginctl` sessions, PAM limits) come from running the server
   as a lingering `systemd --user` or launchd unit, which `phux host enroll`
   already installs. The server does not call PAM.

## Why

User switching needs a root-owned daemon. That reverses ADR-0003's security
boundary, where the kernel enforces socket ownership and a compromised
server is limited to one user's authority. It also makes phux an
authentication system that must get PAM, session accounting and privilege
separation right. Reaching the target user's own server keeps each process
at the authority it already has and reuses the enrollment path that exists.
`whoami` makes that model visible, so it no longer rests on an unstated
promise.

## Tradeoffs

- A root session and a user session on the same host need two servers and
  two enrollments. The demo's single-connection root switch is not possible.
- No per-pane PAM limits beyond what the unit provides.
- Principal-to-user policy has nowhere to live. Operators who want it must
  give each OS user its own credentials.

## Alternatives

**A privileged per-host daemon that spawns panes as the mapped user.** This
is the demo's model. It needs a root process on every host, privilege
separation, PAM and utmp bookkeeping, plus a mapping policy language. It
contradicts ADR-0003 and is rejected for now. A future ADR could add it as a
separate optional component.

**The username on the wire to pick among per-user servers behind one port.**
This adds a front-door process tier per host to route connections, which
duplicates what distinct ports and registry entries already do.
