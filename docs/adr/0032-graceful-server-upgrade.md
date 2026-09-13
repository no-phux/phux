---
audience: contributors
stability: stable
last-reviewed: 2026-06-09
---

# 0032 — Graceful server upgrade (sessions survive a binary update)

**TL;DR.** Upgrading the phux binary today means `phux kill` → restart, which
drops every live pane (your shells, your editor, your agent). Adopt
**graceful in-place upgrade via `execve` re-exec with fd inheritance + VT
snapshot replay**: on `phux upgrade`, the running server snapshots every pane
(the same `vt_replay_bytes` + `scrollback_bytes` it already sends a client on
attach), serializes the session/window/pane tree and an fd→pane manifest, then
re-execs the new binary, passing the PTY master fds and the UDS listener as
inherited descriptors. The new server adopts the fds and rebuilds each pane's
libghostty `Terminal` by replaying its snapshot. Child processes never notice
(fds survive `execve`); clients blink-reconnect. A separate fd-keeper daemon
and live engine-state serialization are the rejected alternatives.

Status: Accepted
Date: 2026-06-09

## Context

phux is a daemon mux: the server runs `setsid`-detached
(`commands/server.rs:52`), owns one libghostty `Terminal` plus a PTY master per
pane, and accepts clients on a `UnixListener`. Closing a terminal only kills
the *client*; panes survive and you re-attach — the core value prop.

But a *binary upgrade* breaks that. The running process is the old code in
memory; the only way onto a new build is to kill the daemon, which closes every
PTY master → the children get `SIGHUP`/EOF and die. This bit the maintainer
directly: installing a fix required killing the very session that hosted the
work. tmux has the same limitation — `tmux` has no in-place server upgrade.

For a tool whose whole point is session durability, "lose everything on
update" is a UX hole, and it hurts three audiences: the maintainer iterating on
phux (rebuild → lose panes), an agent (Claude) whose long-running session lives
in a pane, and non-dev users on `brew upgrade` / package updates who never
expect a refresh to nuke their work.

phux is unusually positioned to fix this. Under ADR-0013 the wire is VT bytes
and the server already synthesizes a self-contained `vt_replay_bytes` +
`scrollback_bytes` snapshot per pane (the `SnapshotSynthesizer`, used on every
client attach). Reconstructing a pane's grid is therefore already a solved
problem — it is "attach to yourself." The missing pieces are descriptor
hand-off and the re-exec orchestration, not state capture.

## Decision

Add a **graceful upgrade** path. On `phux upgrade` (a control command to the
running server; later, optionally auto-triggered when a newer binary appears on
disk), the server:

1. **Snapshots** every pane to `vt_replay_bytes` + `scrollback_bytes` via the
   existing synthesizer, and serializes the session/window/pane tree, the
   per-pane child metadata, and an **fd→pane manifest** into a state blob
   (passed via an inherited memfd / temp fd, not argv).
2. Clears `FD_CLOEXEC` on every PTY master and the `UnixListener`, then
   **`execve`s the new binary** as `phux server --resume <state-fd>`. Open fds
   survive the exec, so the children stay alive and attached to their PTY
   masters across the swap, and the socket path stays bound with no rebind race.
3. The new server reads the state blob, **adopts** the inherited PTY masters and
   listener, and **rebuilds** each pane's `Terminal` by `vt_write`-ing its
   snapshot — the same path a fresh client attach drives.

Clients reconnect: when the old image is replaced, accepted client connection
fds close, the client sees a disconnect and re-attaches to the (inherited,
still-bound) listener, getting a fresh snapshot. v1 is a brief blink-reconnect;
passing the live client sockets through for a flicker-free swap is a v2
refinement (more fd-passing, not a different design).

Phasing: **v1** is the explicit `phux upgrade` command. **v2** may auto-detect a
newer on-disk binary (e.g. after `cargo install` / `brew upgrade`) and prompt or
auto-upgrade per config — the install path then "just works" for non-dev users.

## Rationale

- **`fd`s survive `execve`** — the standard graceful-reload primitive (nginx,
  HAProxy, systemd socket activation). Keeping the PTY masters open across the
  exec is exactly what keeps the children alive, with zero cooperation from
  them.
- **Reuse, not reinvent.** The hard part of any hot-swap is reconstructing
  terminal state; phux already emits a replayable snapshot for attach, so the
  new server rebuilds via a path that is already tested. tmux would have to
  bespoke-serialize its grid model; phux replays the wire it already speaks.
- **No second process in the steady state** — the server stays a single daemon;
  the cost is paid only during the sub-second upgrade.

### Alternatives rejected

- **Kill + restart (status quo).** Loses every child process. The thing this
  ADR exists to fix.
- **Separate fd-keeper / state daemon** that permanently owns the PTYs +
  listener while the logic server restarts and reconnects via `SCM_RIGHTS`.
  Cleaner process isolation and survives a logic-server *crash*, not just an
  upgrade — but it is a permanent second process, a second IPC seam, and more
  moving parts. Recorded as the **v2 hardening** if re-exec proves fragile
  (e.g. if adopting fds across exec turns out error-prone in practice).
- **Serialize the libghostty engine state directly.** Not feasible across a
  version bump (no stable engine snapshot format) and would drift from the
  wire. Replaying the VT snapshot sidesteps it entirely.

## Consequences

- Sessions survive binary updates — the headline UX win, for maintainer, agent,
  and non-dev users alike.
- Upgrade is a brief stop-the-world per server: snapshot + exec + rebuild,
  targeted sub-second; a client reconnect blink. Acceptable for an explicit
  upgrade.
- Snapshot fidelity bounds what survives: viewport + bounded scrollback as the
  snapshot captures it (styled-scrollback is still plain-text pending
  `phux-q0x7`); transient out-of-band registries (OSC 8, kitty graphics) follow
  the same deferral as attach. A pane mid-`SPAWN` or with in-flight input needs
  a quiesce step before the snapshot.
- A re-exec that fails to bind/adopt must not strand the children: the upgrade
  path needs a fallback (abort the exec, keep serving on the old image) — so the
  command validates the new binary (`--version` / a dry `--resume` probe) before
  committing.
- New surface: a `--resume <fd>` server mode, the state-blob format (versioned,
  since old↔new server versions straddle it), and the fd manifest. The state
  format is an explicit compatibility boundary maintained across releases.

## Implementation (shipped)

v1 landed as `phux upgrade` (phux-fak5). Notes where the build refined the plan:

- **fd re-adoption is small, not a fork.** portable_pty exposes `MasterPty` /
  `Child` as *public traits*, so re-adopting an inherited `(master_fd, pid)`
  needed only two trait impls, extracted as the standalone `portable-pty-adopt`
  crate — not a portable_pty fork. A real-`execve` round-trip test proves the
  child + fd survive.
- **Blob carrier is an anonymous `tempfile`,** not a `memfd` (macOS has none) —
  written, `FD_CLOEXEC`-cleared, and inherited; read back with a seek-to-start.
- **JSON state blob,** versioned (`0.5.0-draft.4` allocates the `UPGRADE`
  command at tag `0x0e`); keyed by wire ids so the resumed image re-pins them.
- **Acceptance drill** (`crates/phux/tests/lifecycle_e2e/upgrade_e2e.rs`, run via `just e2e`):
  the server PID is unchanged across the upgrade (in-place `execve`, not
  kill+restart), the pane child stays alive, and scrollback survives.
- Client reconnect is the v1 blink (re-attach + `TERMINAL_SNAPSHOT` resync).
  The flicker-free client-socket hand-off and crash-surviving fd-keeper daemon
  remain v2.
