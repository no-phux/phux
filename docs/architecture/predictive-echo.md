---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-12
---

# Predictive local echo

**TL;DR.** Predictive echo is an experimental client feature. It
renders a conservative set of likely keystroke results with an underline,
keeps the libghostty mirror authoritative, and reconciles each prediction when
real `RESOURCE_OUTPUT` arrives. Contradictions discard the suspect suffix and
repeated misses temporarily hide the overlay. On the alternate screen the
display is confirmation-gated: nothing paints until the app proves it echoes
([ADR-0090](../adr/0090-confirmation-gated-predictive-echo.md)).

---

## Configuration

`[experimental] predictive-echo` is unset by default, which enables
prediction only for an attach that leaves the machine; `true` / `false`
force it ([`../reference/config.md`](../reference/config.md)). The attach
path builds a `PredictionState` for the focused terminal.

## Why it is client-side

Prediction never changes the terminal mirror; guesses live in a separate
underlined overlay painted after the normal renderer, so latency hiding is
independent of transport and needs no wire change.

## What the client predicts

The safe set is deliberately narrow:

- Printable grapheme insertion when the cursor and viewport give the client a
  credible anchor.
- Backspace at the end of the current input run without crossing the learned
  prompt boundary.
- `Ctrl-U` only when that prompt boundary is known.
- Enter when the next-row cursor position is safe to estimate.
- Left and right cursor motion over known cells on the current row.

Other keys still travel to the server normally; they simply receive no local
prediction. Modal applications, line wrapping, unknown prompt boundaries, and
viewport edges all bias the policy toward skipping a guess.

## Reconciliation

Each pending prediction records its target cell, text, width, and kind. When
`RESOURCE_OUTPUT` updates the focused terminal, the client compares pending
predictions with the freshly rendered authoritative cells and cursor:

| Result | Action |
|---|---|
| Confirmed | Remove the prediction; the server has painted the same result. |
| Pending | Keep the overlay; authoritative output has not reached that prediction yet. |
| Contradicted | Remove that prediction and every prediction behind it. |

`BOOTSTRAP_TOMBSTONE` clears pending predictions for the invalidated
generation. The replacement remains staged until `BOOTSTRAP_READY`, whose
atomic publication replaces the viewport and clears the overlay. Reconciliation
follows authoritative terminal output, not acknowledgements. `FRAME_ACK` exists
only for `SynthesizedVtStateSync`, after applying its transition; native and
synthesized-raw streams never send it.

Repeated contradictions make the display tentative (mosh's term): after a
short run of misses, the overlay hides while predictions keep queueing and
reconciling. It returns only after clean authoritative confirmations. This
limits incorrect guesses during modal editing or layout transitions without
stopping the predictions needed to establish recovery.

## Alternate-screen display policy (ADR-0090)

Full-screen apps split into two populations: those that echo typed text (vim
insert mode, an agent TUI's prompt) and those that treat keys as commands
(htop, less, vim normal mode). The client cannot tell them apart from mode
bits, so it measures: on the alternate screen predictions queue and reconcile
as usual, but the overlay stays hidden until a reconcile confirms a
**non-blank insert** against authoritative cells — proof the app echoes.
Blank confirmations never count (space is page-down in less), any
contradiction re-locks the display, and mode-changing input (Esc, chords,
arrows, function keys) kills the evidence. Enter on the alternate screen
suspends the burst instead of predicting a newline — in a TUI it submits.
A one-second display TTL on the front-of-queue prediction bounds any
unconfirmed overlay on either screen.

## Code map

- `crates/phux-client-core/src/predict/state.rs` owns prediction policy and state.
- `crates/phux-client-core/src/predict/overlay.rs` paints the underlined layer.
- `crates/phux-client-core/src/predict/reconcile.rs` classifies authoritative output.
- `crates/phux-tui/src/attach/` connects prediction to input, rendering, and server frames.

## Status

No remaining target-versus-shipped gaps.

| Gap | Today | Owner | Tracked |
|---|---|---|---|
