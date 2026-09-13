---
audience: contributors
stability: stable
last-reviewed: 2026-06-16
---

# 0033 — Input authority leases and process signals ("take the wheel + kill")

**TL;DR.** A human (or supervising agent) can seize exclusive input
authority over a running Terminal and deliver explicit POSIX signals to
the process group inside it — freeze, resume, interrupt, terminate,
kill. Both ride the existing `COMMAND` envelope as three additive,
Terminal-scoped verbs (`ACQUIRE_INPUT`, `RELEASE_INPUT`,
`SIGNAL_TERMINAL`); no new top-level frames, no session/window
vocabulary on the wire ([ADR-0016](./0016-terminal-id-as-wire-primary.md),
[ADR-0017](./0017-tui-not-protocol-privileged.md) hold). A new
broadcast `TerminalControl` event on the agent-event stream makes the
current holder and lifecycle observable to every attached client — and
is the seed of the recorded audit trail.

Status: Accepted
Date: 2026-06-16

## Context

phux already forwards everything a process in a pane sees (VT bytes,
ADR-0013) and accepts structured input from clients. What it does **not**
have is the supervisory pair every operator of a long-running agent
wants: *grab the wheel from whatever is driving this pane*, and *stop the
process before it finishes the thing it is about to do*.

Two holes in the current code line up exactly with this:

1. **Input is unarbitrated.** `handle_terminal_input`
   (`runtime/commands.rs`) gates input only by subscription; its own
   comment notes it "approximated PRIMARY by subscription" and that
   per-connection roles are "future work." Any subscriber can type;
   there is no way to assert "only me, now."

2. **Termination is fd-drop only.** `KILL_TERMINAL` (command `0x03`)
   cancels the actor token, which drops the PTY master fd and lets the
   child get SIGHUP. There is no way to send SIGSTOP/SIGINT/SIGTERM/
   SIGKILL, no reversible *pause*, and no way to signal the process while
   keeping the pane (and its final scrollback / exit status) alive.

Both are Terminal-scoped supervisory operations. ADR-0021 already settled
that such verbs ride the generic `COMMAND` / `COMMAND_RESULT` envelope
rather than minting dedicated frame families. This ADR adds the three
verbs and the broadcast that makes their effect visible.

## Decision

1. **Input authority is an explicit, leased, single-holder property of a
   Terminal.** A Terminal is either `Open` (unheld — today's behavior,
   any subscriber's input reaches the PTY) or `Held { holder, mode,
   expires_at }`. While held, only the holder's `INPUT_*` frames are
   written to the PTY; others are dropped (still acked OK — the
   fire-and-forget invariant of SPEC §12.2 is preserved). Two new verbs:
   - `ACQUIRE_INPUT { terminal_id, mode, ttl_ms }` → `OK | DENIED`.
     `mode = Cooperative` grants only if `Open`; `mode = Seize` preempts
     the current holder (the supervisory "take the wheel"). Reply names
     the prior holder.
   - `RELEASE_INPUT { terminal_id }` → `OK`. Returns the Terminal to
     `Open`.
   The lease is held until the holder releases it or its connection drops
   (`ServerState::detach` clears it and the runtime broadcasts `Released`), so
   a dead operator never strands the wheel. `ttl_ms` rides the wire as an
   advisory lifetime for a future timer-based expiry; the v1 server treats it
   as advisory only.

2. **Process control is an explicit signal verb, distinct from pane
   teardown.** `SIGNAL_TERMINAL { terminal_id, signal }` delivers one of
   `Interrupt` (SIGINT), `Freeze` (SIGSTOP), `Resume` (SIGCONT),
   `Terminate` (SIGTERM), `Kill` (SIGKILL) to the **process group** of
   the pane's child, then replies `OK`. `Freeze`/`Resume` is the
   reversible brake — halt the agent mid-step, inspect, resume or kill.
   This is orthogonal to `KILL_TERMINAL`: `SIGNAL_TERMINAL` acts on the
   process and leaves the pane addressable (read its last screen, its
   exit status); `KILL_TERMINAL` removes the pane.

3. **Signals target the whole subtree.** Signals go to the child's *process
   group* (`killpg`), so they reach the agent *and* every subprocess it
   spawned — agents fan out into many children, and signaling only the parent
   pid leaves orphans running. No spawn-site change is needed: `portable_pty`
   already makes the PTY child a session/process-group leader (`setsid` +
   `TIOCSCTTY` to give it a controlling terminal), so the child's pid *is* its
   process-group id.

4. **Control state is broadcast, not polled.** A new agent-event variant
   `TerminalControl { terminal_id, ts, lifecycle, input_holder,
   last_action, actor }` is emitted to every subscriber on every lease
   change and every lifecycle transition (`Running | Frozen |
   Exited{status}`). Clients render "who has the wheel" and "frozen"
   from the event; they never guess from input failures.

5. **Additive wire change.** Three command tags (`0x0f`–`0x11`) under the
   existing `COMMAND` envelope, one agent-event tag, one optional
   `ErrorCode::InputLeaseHeld`. No existing bytes change →
   `PROTOCOL_VERSION` draft bump (`0.5.0-draft.5` → `-draft.6`), SPEC
   §5.1 catalog + CHANGELOG entry. `ErrorCode` is `#[non_exhaustive]`,
   so the new code is non-breaking.

## Why

- **It materializes the PRIMARY role the code already reserved a hole
  for**, instead of inventing a parallel mechanism. The lease *is* the
  per-connection role that `handle_terminal_input` deferred — scoped to a
  Terminal, leased rather than static, which is what a control plane with
  attach/detach actually needs.

- **It rides the envelope ADR-0021 built.** Adding `ACQUIRE_INPUT` /
  `SIGNAL_TERMINAL` as command verbs costs zero new frame pairs and keeps
  the wire Terminal-scoped and substrate-shaped. No session, no window,
  no "operator" concept crosses the wire.

- **The broadcast event is the audit seam.** `TerminalControl` is the
  live-dashboard signal *and* the first-class, timestamped record of
  every takeover / freeze / kill. Persisting it alongside the already-
  sequenced VT byte stream (ADR-0013) is the forward path to replayable,
  tamper-evident "who supervised what, when" — without designing the
  recording store now.

## Tradeoffs

- **Default `Open` keeps a foot-gun.** Back-compat means an unheld
  Terminal still accepts input from any subscriber. We accept this:
  changing the default to "locked until acquired" would break every
  current single-client flow. Consumers that want strict control acquire
  on attach.

- **Lease arbitration is not authorization.** Under server-per-user
  ([ADR-0003](./0003-server-process-model.md)) all local clients are the
  same Unix user, so `Seize` is cooperative-by-trust, not a security
  boundary. Per-role policy (who may seize, who may kill) is deferred to
  remote-consumer auth ([ADR-0031](./0031-remote-consumer-auth-and-encryption.md)),
  where the trust boundary actually exists.

- **TTL adds a timer per held Terminal.** Cheap, and bounded by the
  number of *actively-seized* panes, not total panes. The alternative —
  leases that never expire — strands the wheel when a client dies mid-
  hold, which is the exact failure a control plane must not have.

## Alternatives

- **A static PRIMARY/VIEWER role negotiated at attach.** Simpler, but
  wrong shape: supervision is transient ("grab it, fix it, let go"), and
  a static role can't model an operator who attaches read-only and then
  decides to intervene. The lease subsumes the static role (acquire-on-
  attach == PRIMARY) without freezing it.

- **Overload `KILL_TERMINAL` with a `signal` field.** Rejected: it
  conflates "signal the process" with "destroy the pane." Freeze/resume
  and "kill the agent but keep its output for the post-mortem" both
  require the pane to outlive the signal. Two verbs keep the lifecycle
  honest.

- **Inject signals as input bytes (write `0x03` for Ctrl-C).** Works for
  SIGINT only, depends on the child's line discipline, and cannot express
  SIGSTOP/SIGKILL at all. Real job-control signals must go to the process
  group out-of-band.
