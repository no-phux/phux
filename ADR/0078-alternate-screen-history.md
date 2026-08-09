---
audience: contributors
stability: stable
last-reviewed: 2026-08-09
---

# 0078 — Harvesting alternate-screen history

**TL;DR.** Full-screen agents keep their transcript where no `--scrollback`
reaches it. The server may harvest it by driving the application's own
scrollback with synthesized wheel events — opt-in per call, primary-only,
lease-acquiring, actor-owned, restored by an obligation the actor owns rather
than the caller, returned in its own array. It is a multi-week subsystem, and
the one read that moves a pane.

Status: Proposed
Date: 2026-08-08

## Context

Agents such as Claude Code and OpenCode paint their conversation into the
**alternate screen**, which by construction has no host scrollback: rows scroll
out of the application's own buffer and never enter the emulator's history. No
value of `--scrollback` recovers them, so an agent supervising another agent
through phux can drive it and report its state but cannot read what it said.
The only mechanism that reaches those rows is the application's own scrollback,
driven by input — a read that writes. That is why this was split out of
[ADR-0077](./0077-agent-read-surface.md): it breaks a normative role sentence,
needs the input lease, needs a wire capability bit, and is larger than
everything else in its wave combined.

**One empirical prerequisite, now partly discharged.** That the named agents
paint to the alternate screen *is* established in this tree, though not by the
manifests, which key on titles and regions rather than screen mode:
`crates/phux-server/src/grid/synthesizer.rs` documents a shipped fix whose bug
report names "opencode, Claude Code" as mouse-tracking TUIs and "on the alt
screen with 1007 set" as the failing path, with a regression test that writes
`\x1b[?1049h` plus the mouse modes. What remains unestablished is the part the
fixtures actually gate: rows per wheel notch, repaint settle time, and the seam
a merge has to splice. Capture live viewports before accepting this ADR — the
Tradeoffs of
[ADR-0046](./0046-server-side-agent-state-detection.md) record what writing
against an imagined TUI cost last time.

## Decision

1. **Server-side, owned by the terminal actor, encoding its own wheel events.**
   The traversal is a phase machine stepped inside the actor's `select!`, shaped
   like the existing `step_native_bootstrap` stepper but inverted: that one
   gates nine other arms off, and the harvest gates **none** off, because it
   depends on PTY ingress to see the repaint it caused. An `async` traversal
   awaited inside an arm parks the loop, stops the PTY drain, and reads a frozen
   screen until its budget expires; that is the version an implementer writes
   first. Its reply is deferred and sent out of band, since `handle_command` is
   awaited inline in the per-client read loop and the TUI holds no special path
   there ([ADR-0017](./0017-tui-not-protocol-privileged.md)). Wheel events are
   encoded by the actor's own mouse encoder, not routed through the input lane,
   which is fed a `ClientId` a harvest does not have —
   [ADR-0044](./0044-dedicated-input-lane.md) matters only for ordering. Phase
   constants and merge heuristics belong in `docs/architecture/`.

2. **The harvest acquires the input lease; it does not merely check it.** It
   holds an [ADR-0033](./0033-input-authority-and-process-signals.md) lease
   under a reserved server-owned holder id, released on every exit path. The
   earlier draft checked it in the gate and claimed to hold it in Tradeoffs;
   resolving toward acquiring makes precondition and postcondition one mechanism
   and broadcasts `Acquired`/`Released`, so an attached human is told rather
   than surprised. It owes ADR-0033 an amendment: a holder with no connection
   has no `detach` to clear it and `ttl_ms` is advisory in the v1 server, so the
   release is actor-owned and runs on terminal death and server shutdown.

3. **The opt-in rides `GET_SCREEN` behind a `ServerFeature` bit, and the gate
   carries a PRIMARY clause.** [`../docs/spec/L1.md`](../docs/spec/L1.md) §6.1
   calls `GET_SCREEN` side-effect-free and allowed for viewers; §6.2 contrasts
   it as the read-only, viewer-safe surface. Both sentences change in the same
   PR: `GET_SCREEN` is viewer-safe **except** with `request_transcript`, which
   is primary-only. Left unedited, they would classify a wheel-driving traversal
   as a viewer read for whoever implements `RolePolicy`. The capability bit is
   load-bearing rather than polite: a missing trailing field decodes as a
   default, so an unadvertised flag returns a passive read that looks like a
   successful harvest
   ([ADR-0061](./0061-capabilities-add-versions-break.md) §2). No version bump.

4. **The gate is conjunctive, every clause is a refusal, and a refusal is a
   successful passive read** — exit 0, no new exit code, a named reason on the
   payload. The clauses are the flag, PRIMARY, a local PTY-backed terminal, an
   alternate active screen, a wheel event that the pane's mouse encoder encodes
   to something non-empty (the oracle is the emitting path, so it cannot
   disagree with what would be sent), an `idle` `phux.agent/v1` state, an
   acquirable lease, no harvest in flight, and a viewport shorter than the
   request. Two reasons are not merely descriptive: `no_detector` is distinct
   from `agent_not_idle`, since a server with detection off would otherwise
   advertise a permanent refusal as retryable, and `not_local` covers satellite
   panes, which the hub refuses rather than relays.

5. **The `idle` clause is a safety gate and takes the level, deliberately.**
   [`../docs/spec/L3.md`](../docs/spec/L3.md) §3.7 rules that a level read
   asserts only that no contrary state is being asserted, and that a safety gate
   — "refusing to scroll a screen that may be repainting" — may read it. That is
   this gate. The consequence is accepted rather than papered over: the level is
   equally true of a crashed agent, so a harvest may run against a dead pane. No
   extra liveness check is specified, because a dead pane does not repaint,
   which makes that the traversal's safest case.

6. **Harvested rows return in their own array, with a seam count.** They are
   reconstructed application rows, not emulator history; merging them into
   `scrollback[]` would make two provenances indistinguishable. A `transcript`
   object carries the rows, a status, a refusal reason, and a seam count, since
   the merge cannot prove it spliced correctly. `--transcript` and
   `--scrollback` stay composable, and ADR-0077's `truncated` keeps one meaning.
   Concurrent `GET_SCREEN` and `GET_TERMINAL_STATE` are served the settle-phase
   capture, and detector publication freezes by extending ADR-0046's existing
   `skip-state-update` clause — a deliberately scrolled alt screen is the
   transcript-viewer case that clause already names — rather than by inventing a
   second freeze the implementation would then build twice. That freeze is
   visible to other consumers and this ADR owns saying so: for the traversal's
   duration a `GET_METADATA` on that Terminal returns the settle-phase value,
   not a current one, and an
   [ADR-0076](./0076-agent-prompt-and-lifecycle-wait.md) `agent wait` on the
   same pane observes no edge — bounded and self-healing, never lost.

7. **Restore has two primitives, not one absolute, and three aborts.** A
   graceful phase handles soft exits; a synchronous inverse burst runs one line
   before every hard-abort return. "Every exit path restores" is unachievable —
   the PTY writer queue drops on a full mailbox, and a dead PTY cannot be
   restored — so the enforceable claim is the valuable one: the obligation
   belongs to the actor, never the caller's task. The aborts are required, not
   deferred: on the pane's title-derived busy signal (our own scrolling
   invalidates the screen rules but not the title rule); on client disconnect;
   and in `prepare_upgrade`, which otherwise captures the scrolled transcript as
   the replay image and erases the restoring job from every address space
   ([ADR-0032](./0032-graceful-server-upgrade.md)) — abort and await there, and
   refuse the upgrade on timeout, since a refused upgrade is harmless and a
   permanently scrolled pane is not.

8. **The docs must stop implying this read is safe, and this ADR does not write
   that text.** [`../docs/consumers/agents.md`](../docs/consumers/agents.md)
   §1's viewport-safety paragraph keeps its five-verb list and needs two
   changes: its "`snapshot`/`wait` are side-effect-free" clause narrowed to
   `snapshot` *without* `--transcript`, and a following sentence naming
   `--transcript` as the one read that moves the pane — opt-in, primary-only,
   gated, lease-acquiring, restored, and visible to an attached human as a
   bounded scroll and an input-authority acquisition. That file's owner makes
   the edit. The same qualification is owed to `agents.md` §2 and §4.2, `L1.md`
   §6.1 and §6.2, `input.md` §8, and both side-effect-free claims in
   [`../docs/consumers/pi.md`](../docs/consumers/pi.md); `agents.md`'s `wait`
   and `agent explain` paragraphs stay untouched, because those paths cannot
   trigger a harvest. MCP's `phux_snapshot` does **not** gain the flag, its
   arguments being frozen at 1.0
   ([ADR-0071](./0071-what-phux-1-0-commits-to.md) point 1 — itself still
   Proposed, so this is a commitment to the freeze's shape rather than a
   consequence of it).

## Why

**Opt-in, because the cost is visible.** Triggering a harvest on any read of an
alt-screen pane is defensible in a product that owns its window; it is wrong for
a substrate whose panes a human is often watching. A flag makes the caller state
that a bounded, restored scroll is acceptable, and keeps the polling paths —
`wait`, `watch`, the ADR-0046 detector — categorically unable to trigger one.

**Acquiring the lease is what makes exclusion real.** Leases are opt-in and
normally unheld, so a gate that merely checks for one passes in the common case
— a human attached with no lease — and the wheel events interleave with their
keystrokes anyway. That is the interleaving used below to reject the
client-side design, so checking would have argued both ways.

## Tradeoffs

- **This is the largest item in its wave — on the order of 1,760 lines of
  production code and 2,750 of tests and fixtures, roughly three weeks with
  review.** It is not a flag on an existing read, and the schedule risk is not
  the code: it is capturing real viewports from the actual agent CLIs, which
  gates every merge heuristic and every fixture and must start first.
- **The `idle` clause is a heuristic on a heuristic, not a correctness
  guarantee.** The detector fails safe to `idle`, so a pane whose manifest
  stops matching publishes `idle` mid-turn, and a declared record outranks the
  detector, so a consumer can declare `idle` on a busy agent. The settle phase
  and the title-derived abort mitigate that; they do not remove it.
- **A documented guarantee narrows.** "Reads are side-effect-free" becomes
  "except one flag", taken over a silent scroll or no transcript at all.
- **Its worst failure is a permanently scrolled pane** a user cannot recover,
  and it is slow and exclusive besides — input authority on one Terminal for
  the traversal, so a supervisor polling several agents serializes.
- **The merge can seam** (`seam_count` makes that countable, not provable), it
  fails wherever the agent does not route the wheel, and the gate opens on a
  crashed pane. All three are deliberate.

## Alternatives

**Client-side harvest over `ROUTE_INPUT`.** Zero wire change, rejected anyway:
the restore obligation is unenforceable outside the server — a client killed
mid-traversal leaves the pane scrolled with no live owner — and every step adds
a round trip to a latency-bound loop.

**Gate the harvest to unattached panes.** Rejected: the common shape is a human
attached in the TUI while an agent reads the pane beside them, so this refuses
the main workflow. Acquiring the lease gives the exclusion that refusing
attachment was reaching for, and tells the human besides.

**Trigger implicitly from `--scrollback` on an alt-screen pane.** Rejected: it
makes an unflagged read move a human's viewport, and a sentinel value as the
opt-in is the versioning trap ADR-0061 exists to prevent.

**A new acknowledged command for the traversal**
([ADR-0053](./0053-acknowledged-idempotent-input.md)). Rejected: this is a read
that happens to write, its result is a screen rather than a delivery receipt,
and it has no retry identity. The PRIMARY clause buys the same authority
guarantee for one line.

**Require an observed `working` -> `idle` edge instead of the level.** Rejected:
that is the completion-gate predicate (`L3.md` §3.7) and this is not a
completion gate. An edge would refuse every pane the server has only ever seen
at rest, to prevent a benign failure.
