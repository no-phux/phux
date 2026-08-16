---
audience: contributors
stability: evolving
last-reviewed: 2026-08-16
---

# 0090 — Predictive echo returns to the alt screen via confirmation-gated display

**TL;DR.** Replace the binary alt-screen gate on predictive echo with mosh's
tentative-display model, in `phux-client-core::predict`: predictions queue and
reconcile on both screens, but on the alternate screen they render only after
the app confirms a non-blank echo, re-lock on any contradiction, and hide on a
one-second display timeout. The adaptive back-off becomes a display lock
instead of a predict suspend, which also fixes it never being able to re-arm.

Status: Accepted
Date: 2026-08-16

## Context

phux-51n6.1 gated predictive echo proactively: when the focused pane is on the
alternate screen, the attach driver did not call `predict_key` at all, because
the ungated predictor painted ghosts in full-screen apps — vim normal mode,
htop, and less do not echo keystrokes, so every underlined guess was garbage
the next authoritative frame reconciled away.

The gate overshot. Agent TUIs (Claude Code, codex) — a primary phux workload —
run entirely on the alternate screen and *do* echo typed characters into their
prompts, as does vim insert mode. With the gate, every keystroke there eats a
full round trip, which is precisely the latency predictive echo exists to
hide. The binary "alt-screen means no echo" heuristic is wrong for the apps
people type into.

phux-mobile hit the identical regression (its v0.3.1 gate) and designed the
fix as its ADR-0019: confirmation-gated tentative display, field-proven in the
mobile app since. Its predictor is a port of this crate's, so the divergence
blocked the mobile repo from adopting `phux-client-core` (mobile ADR-0025)
and, transitively, the Android shared-core path. This ADR upstreams that
design onto the richer upstream state machine (prompt boundary, Ctrl-U,
arrow motion, adaptive back-off).

Separately, the adaptive auto-back-off (phux-pxaj) had a structural bug: it
*suspended predicting* after three contradicting reconcile passes, but its
re-arm condition was two reconcile passes that each confirm a prediction —
predictions that could no longer exist, because predicting was suspended.
The back-off was a one-way kill switch for the rest of the attach session.

## Decision

Adopt mosh's "tentative until validated" display model inside
`phux-client-core::predict`, and have the TUI driver feed it:

1. **Predict everywhere, display conditionally.** The driver's
   `!terminal_in_alt_screen` clause is removed; instead it syncs the screen
   mode into the state (`set_alt_screen`, on the keystroke path and before
   each reconcile) and stamps guesses with a monotonic clock (the `*_at`
   entry points). The display policy (`should_display` / `displayable`):
   - Primary screen: display immediately (unchanged).
   - Alternate screen: display nothing until a reconcile confirms a
     **non-blank insert** against authoritative cells — proof the app
     echoes. Blank confirmations (space, backspace) are trivially
     satisfiable by empty cells in a non-echoing app and never unlock.
   - Any contradiction clears the queue **and re-locks display**; evidence
     is re-earned on the next confirmed echo. Screen switches, resizes,
     resyncs, and pane re-anchors also drop the evidence.
   - A front-of-queue prediction older than `DISPLAY_TTL_MS` (1 s, chosen
     to clear worst observed cellular RTT; an SRTT-derived value is the
     earmarked successor) hides the whole overlay. The queue still
     reconciles.
2. **Alt-screen Enter is never predicted.** In a TUI, Enter submits (an
   agent prompt) or executes (vim); the primary-screen row+1/col-0 guess
   would anchor the rest of the burst wrong. The predictor suspends the
   burst but keeps the latch — a submit does not change who echoes.
3. **Alt-screen mode-changing input kills the evidence.** Esc is exactly
   how vim leaves insert mode; chords, arrows, Tab, and function keys are
   app commands. Any of them drops the burst and the latch, so typing in
   vim normal mode after Esc can never paint a ghost on stale evidence.
4. **The back-off becomes tentative display, not a predict suspend.**
   Three consecutive contradicting passes hide the overlay; predicting and
   reconciling continue silently, so the two clean confirming passes that
   lift the lock can actually occur. This preserves the phux-pxaj intent
   (stop painting during readline vi-mode storms, which the alt-screen
   latch cannot see) while removing the permanent-suspend bug.

Reconcile rules are unchanged: confirm the front on match, hold on pending,
drop the whole suffix on contradiction. The overlay writer is unchanged; the
two paint sites gate on `should_display`.

## Why

The phux-51n6.1 regression was *painting unconfirmable guesses*, not
predicting per se. Confirmation-gating designs against re-introducing it
structurally: in htop/less/vim-normal-mode no non-blank insert ever confirms,
so nothing ever displays — pixel-identical to the binary gate — while agent
TUIs and vim insert mode unlock after the first echoed keystroke (one RTT of
warm-up per screen session, mosh's own trade). Echo is *measured*, not
inferred from mode bits or app identity.

The display timeout additionally bounds the two main-screen cases the gate
commit called un-gatable: a readline vi-mode or no-echo-password mispaint now
survives at most one second even before the tentative lock trips.

Landing this in `phux-client-core` (not the driver) keeps the policy in the
frontend-neutral kernel that `phux-client-ffi` exposes, which is what unblocks
phux-mobile's ADR-0025 adoption and the Android shared core: consumers get the
same latch, TTL, and Enter policy the mobile app already shipped, instead of
each porting it separately.

## Tradeoffs

- First keystroke per alt-screen session (and after every contradiction or
  mode change) still costs a full RTT. Accepted: evidence cannot precede echo.
- A TUI that echoes in one field but not another can display a wrong guess
  for up to one contradiction + TTL. Bounded, self-healing.
- `DISPLAY_TTL_MS` is a constant until SRTT plumbing exists; on links slower
  than 1 s the overlay hides prematurely (the safe direction).
- The TUI paints the overlay on events, so a TTL-expired ghost is removed at
  the next paint rather than by a dedicated timer tick. A repaint tick while
  predictions are pending is a possible follow-up if this shows in practice.

## Alternatives

**Keep the binary gate; special-case known echoing apps.** App fingerprinting
misfires on styled prompts and modal editors, and inverts the design: echo is
observable, so measure it.

**Per-cell prediction epochs (full mosh).** Finer rollback granularity than
the single queue (one contradiction drops the burst, not one epoch). Rejected
for now: the mobile deployment shows the single-queue simplification holds up,
and the epoch machinery would triple the state surface.

**Fix the back-off deadlock by re-arming on a timer.** A timer re-arms into
the same mispredict storm it backed off from; requiring observed clean
confirms (possible once predicting continues while tentative) re-arms on
evidence instead.
