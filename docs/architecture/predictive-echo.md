---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-07-09
---

# Predictive local echo

**TL;DR.** Predictive echo is implemented as an opt-in client feature. It
renders a conservative set of likely keystroke results with an underline,
keeps the libghostty mirror authoritative, and reconciles each prediction when
real `TERMINAL_OUTPUT` arrives. Contradictions discard the suspect suffix and
repeated misses temporarily disable prediction.

---

## Status and configuration

Predictive echo ships in the attach client and is off by default. Enable it in
the user config:

```toml
[experimental]
predictive-echo = true
```

The config layer maps that key to `PredictiveConfig`, and the attach path
constructs a `PredictionState` for the focused terminal. The feature is
experimental: its key and policy may change before 1.0.

## Why it is client-side

On a slow connection, waiting for the server to echo every keystroke makes the
terminal feel delayed. phux can paint a conservative guess immediately, then
replace it with authoritative output when the round trip completes.

Prediction never changes the terminal mirror. The mirror remains a
libghostty `Terminal` fed by server output; predictions live in a separate
overlay painted after the normal renderer. The overlay uses underline so a
user can distinguish speculation from confirmed terminal content.

This keeps latency hiding independent of transport. The same predictor can run
over a local socket, WebSocket, or QUIC connection without changing the wire.

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
`TERMINAL_OUTPUT` updates the focused terminal, the client compares pending
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

Repeated contradictions trigger adaptive backoff. Prediction pauses after a
short run of misses and resumes only after clean authoritative confirmations.
That prevents a modal editor, vi-mode shell, or fast layout transition from
producing a sustained stream of incorrect local guesses.

## Code map

- `crates/phux-client-core/src/predict/state.rs` owns prediction policy and state.
- `crates/phux-client-core/src/predict/overlay.rs` paints the underlined layer.
- `crates/phux-client-core/src/predict/reconcile.rs` classifies authoritative output.
- `crates/phux-client/src/attach/` connects prediction to input, rendering, and server frames.
