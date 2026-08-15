---
audience: contributors
stability: stable
last-reviewed: 2026-08-15
---

# 0089 — The sidebar is a bounded attention inbox, not a structural list

**TL;DR.** The window sidebar becomes three zones — a capped cross-session
attention queue, the focused session's windows, and a rolled-up roster of every
other session — ranked by how much each row wants a human rather than by where
it lives. The queue contributes zero rows when nothing is blocked, so the strip
shrinks when the fleet is calm. All of it is a client-side projection over
state the client already receives; no wire surface is added.

Status: Accepted
Date: 2026-08-15

## Context

The strip listed the attached session's windows and, under a second header,
that session's agent panes ordered by
[ADR-0046](./0046-server-side-agent-state-detection.md) lifecycle state. Every
other session on the server was reachable only by summoning a modal — the
session picker, the window picker, or the fleet dashboard — and the peer data
behind those modals was a one-shot `GET_METADATA` sweep taken at attach, which
went stale silently and was documented as doing so.

That shape has a ceiling. A user running agents across several worktrees has
no ambient answer to "what else is on the line?", so the sessions they are not
looking at are the ones they forget. The competing product (herdr) paints the
opposite extreme: every workspace and every agent, always, which is a wall at
nine agents. Both fail the same way — the sidebar is **structural**, so it
shows the same thing whether or not anything wants attention, and ranking is
left to the reader.

phux already had the missing primitive. `attention_rank` orders a pane by how
much it wants a human — blocked, then finished-but-unread, then working, then
settled — with a `seen` decay. It was scoped to the attached session.

## Decision

1. **Three zones, ordered by demand, not by structure.**
   `needs you` (the cross-session attention queue), `here` (the focused
   session's windows, unchanged), `spaces` (one rolled-up line per other
   session). `spaces` moves up a level: it now means what herdr means by it —
   a project — rather than a window.

2. **The queue is capped; the roster is not.** The queue competes for the
   eye, so it takes at most `NEEDS_YOU_CAP` rows plus an honest `+N more`
   that opens the fleet dashboard. The roster is meant to be COMPLETE — it
   answers "which sessions exist?", a question a truncated list answers
   wrongly — and is bounded only by the strip.

3. **Zero rows when calm.** An empty queue contributes no header, no gap, no
   placeholder. This is the load-bearing property, not an optimization: a
   strip that paints the same wall at rest as under load has told the reader
   nothing by being present.

4. **Zone 2 keeps a floor.** The queue is allocated first but clamped so the
   focused session always keeps its header and a window block. A blocked
   fleet must not squeeze the session you are working in off its own strip.

5. **No new wire surface.** Sessions are consumer projections
   ([ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md)), so
   the zones are built from verbs the client already sends: peer layouts and
   per-pane agent records via `SUBSCRIBE_METADATA` on keys it already reads,
   and peer asked-state via the server-wide `SUBSCRIBE_EVENTS { terminal:
   None }` it has always held.

6. **Enumerate, then follow.** Peer subscriptions are established by
   enumerating the session graph and then tracking `AgentEvent::PaneSpawned` /
   `PaneClosed`, which the server broadcasts to every server-scope subscriber.
   This is what makes the sweep race-free without a wildcard-Terminal scope.

7. **The sidebar ships on.** Off by default meant the feature reached only
   users who went looking for it.

## Rationale

The queue and the roster are the same data at two resolutions, and the split
is what makes the strip scale in both directions. Detail decays with distance:
the focused session gets a tree, every other session gets one line, and only
urgency is allowed to cross that boundary — because urgency genuinely ignores
locality, while topology does not.

Capping the queue and not the roster looks inconsistent and is not. They
answer different questions. "Who needs me right now?" is answered well by the
top five and badly by a wall. "What exists?" is answered wrongly by any
truncation, and a roster grows one line per session — slowly, and in a
quantity the user chose.

Zero-rows-when-calm is what a structural sidebar cannot imitate. It requires
the strip to be a projection of demand rather than of shape, which is
precisely the decision recorded here.

## Consequences

- The always-on strip now depends on peer state, so a peer push must raise the
  **chrome** repaint and not only the fleet repaint. The pre-existing foreign
  paths raised only the latter, which was correct while peers were a
  modal-only concern.
- Adopting a broadcast now requires knowing WHOSE it is. The layout arm
  previously matched the key family and adopted whatever it decoded, which was
  safe only while a client subscribed to exactly one layout key; it now
  attributes the key to a session first.
- **Satellite sessions cannot report state.** `SUBSCRIBE_METADATA` on a
  satellite Terminal scope is normatively refused (`docs/spec/L3.md`), so a
  satellite roster row shows a pane count and an explicitly unknown dot. It
  must never render `blocked: 0`, which would be an attention surface lying by
  omission.
- **A peer row is never `seen`.** Marking a pane seen means focusing it, which
  for a peer means switching there. A peer's finished-and-unread agent stays
  on its rung until somebody looks — correct for an inbox, and it means the
  queue does not self-clear from a distance.
- Peer rows have no last-change clock (the clock is keyed by local
  `TerminalId`), so equal-rank peers hold declaration order.
- The default width moves 20 → 28, which moves the strip's yield threshold
  from 60 to 68 columns.

## Alternatives considered

**Keep the strip session-local and improve the modals.** Rejected: a modal
answers only when summoned, and the failure being fixed is that the user does
not know there is anything to summon it for.

**Show everything, herdr-style.** Rejected: it is the wall this design exists
to avoid, and it makes the sidebar useless precisely when the fleet is busiest.

**Add a wildcard-Terminal metadata scope so one subscription covers the
fleet.** Rejected as unnecessary. It is a wire change (bead phux-w7z2.20
assumed one was required), and enumerate-then-follow over the existing
server-wide event stream closes the same gap for this consumer. The wire
change may still be justified for the headless `wait --any` surface, where no
equivalent follow exists; that is a separate decision.

**Persist the queue's order server-side so every client agrees.** Rejected:
ordering depends on `seen`, which is client-local by
[ADR-0049](./0049-client-local-focus-and-advisory-attention.md)'s reasoning — one human's
attention is not another's.
