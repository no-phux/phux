//! Predictive local echo — Mosh-class latency hiding for `phux attach`.
//!
//! When the user types over a slow link, naive client/server VT round-trips
//! make every keystroke wait for the server to echo it. Mosh's State
//! Synchronization Protocol paper popularised speculative local rendering:
//! the client applies a *guess* of what the server will do to its own
//! mirror, decorates the guess so the user can tell it apart from
//! authoritative output, and reconciles when the real answer arrives.
//!
//! `phux-9gw.1` lands the third leg of Mosh's value decomposition (per the
//! `mosh-decomposition` bd memory): (1) per-consumer state sync —
//! ADR-0018, server-side, already landed; (2) UDP transport — deferred to
//! the QUIC migration in ADR-0007; (3) **predictive local echo** — this
//! module.
//!
//! # Surface
//!
//! Three small types compose the feature:
//!
//! - [`state::PredictionState`] — the queue of in-flight predictions plus
//!   the client-side cursor estimate used to anchor them.
//! - [`overlay::Overlay`] — writes the prediction layer to the outer
//!   terminal as VT escapes (positioned writes with an
//!   underline SGR attribute).
//! - [`reconcile::reconcile_terminal_output_per_cell`] — the v1.1
//!   match game (phux-9gw.1.1). On each `TerminalOutput`, walks the
//!   prediction queue against the freshly painted authoritative cells
//!   and the new cursor position; drops confirmed predictions, drops
//!   the suffix from any contradiction, and keeps predictions still
//!   ahead of confirmed state.
//!
//! # Visual decoration: underline
//!
//! Mosh ships with underline as the default decoration and the choice
//! survives a decade of field use. We follow suit for two reasons specific
//! to phux:
//!
//! 1. The renderer in `phux_client::attach::render` already emits
//!    `Style::faint` (SGR 2) for any program that asks for dim text —
//!    using dim for predictions would collide with `man`, `less`, vim
//!    "concealed" regions, and any TUI that paints a dimmed status line.
//!    Underline (SGR 4) is rare in interactive content. Mosh made the
//!    same call for the same reason.
//! 2. Underline survives the "no SGR" path in our `emit_sgr_delta` —
//!    re-emitting cells doesn't accidentally lose the prediction bit
//!    because we paint the overlay *after* the renderer flushes, so the
//!    next renderer pass cleanly stomps it on reconciliation.
//!
//! # Safety classes (v1.3+1.4)
//!
//! Five key classes are predicted today:
//!
//! - Single-Unicode-scalar `text` payload (width 1 or 2 per
//!   `unicode-width`), no Ctrl / Alt / Super modifier. The server's
//!   terminal will echo exactly one grapheme of advance for each — the
//!   prediction is a forward step by the grapheme's cell width
//!   (phux-9gw.1.4). Width 0 (combining marks) and multi-scalar
//!   graphemes (ZWJ sequences) are still rejected; they need cluster
//!   awareness which is a separate follow-up.
//! - Backspace (`PhysicalKey::Backspace`) **at end-of-line**, defined as
//!   "the cell to the left of the cursor is non-empty and the cursor is
//!   not at column 0". A naïve backspace prediction over a wrapped line,
//!   the prompt, or after a programmatic SGR change would diverge
//!   visibly. End-of-line is the conservative subset that covers the
//!   "typing then immediately deleting" case which is the bulk of why
//!   users notice latency. A backspace is additionally refused if it
//!   would erase at or below the prompt-boundary anchor (below).
//! - Ctrl-U (`PhysicalKey::U` + CTRL, kill-to-start-of-line) **only when
//!   the prompt boundary is known** for the current row (phux-9gw.1.5).
//!   Erases the typed run from the boundary up to the cursor as a batch
//!   of blank predictions. With an unknown boundary it is refused — the
//!   full-line erase is exactly the case that would otherwise eat the
//!   prompt.
//!
//! ## Prompt boundary (client-side heuristic, phux-9gw.1.5)
//!
//! The predict layer has no OSC-133 shell integration, so it does not
//! know where the prompt ends. Instead it learns a *prompt-boundary
//! anchor* purely from typed input: the column where the user's first
//! [`state::PredictionKind::Insert`] lands on a row marks where typed
//! input begins; everything to the left is prompt (or prior output) that
//! erasure must never touch. The anchor survives a same-row reconcile
//! resync (the server echoing what we typed) but is forgotten on a row
//! change, an Enter, a viewport resize, or a contradicting reconcile —
//! any of which means the typed-input context is no longer trustworthy.
//! This is the strictly-safe subset that ships without server-side
//! plumbing; full prompt-aware Ctrl-U across re-painted prompts would
//! need OSC-133 (`FinalTerm`) shell integration through the server parser,
//! which is out of this layer's scope.
//! - Enter (`PhysicalKey::Enter`) **past column 0**, on any row except
//!   the last. Models a pure cursor jump to `(row+1, 0)` — no cell
//!   paint, just a forward anchor so subsequent inserts queue on the
//!   correct row. The per-cell reconcile confirms the prediction once
//!   the authoritative cursor advances past the original row;
//!   contradicts (drop) if the server stayed put (program intercepted
//!   the keystroke, e.g. password prompt swallow).
//! - `ArrowLeft` / `ArrowRight` **over a known cell on the current line**
//!   (phux-9gw.1.3). The predict layer peeks at the cell grid via
//!   `read_grapheme_at` and advances/retreats the predict cursor by the
//!   stepped-over grapheme's cell width. Skipped if the cell is blank
//!   (no anchor) or if the motion would cross the viewport edge. No
//!   overlay paint — reconcile confirms when the authoritative cursor
//!   matches the predicted target column.
//!
//! Everything else — arrow keys at line boundaries, control chords other
//! than Ctrl-U, function keys, Tab, Alt-chords, IME composition,
//! multi-codepoint graphemes (ZWJ sequences, combining marks), and
//! full-line erasure when the prompt boundary is unknown — is not
//! predicted. They are still sent upstream
//! as normal; only the local echo is skipped. Follow-up tickets widen
//! the safe set further once the reconcile path has miles on it.
//!
//! ## Confirmation-gated alt-screen display (ADR-0090)
//!
//! On the alternate screen (`?1049h`/`?1047h`, as vim/nvim, pagers, and
//! agent TUIs use) predictions queue and reconcile as usual but the
//! overlay is **confirmation-gated**: nothing displays until a reconcile
//! confirms a non-blank insert against authoritative cells — proof the
//! app echoes (vim insert mode, an agent TUI's prompt). Apps that never
//! echo (htop, less, vim normal mode) never display a guess, which is
//! pixel-identical to the retired binary gate that skipped
//! [`PredictionState::predict_key`] entirely in app mode (phux-51n6.1) —
//! while apps people actually type into get their echo back. Mosh's
//! "tentative until validated" model, collapsed onto the single queue:
//!
//! - a contradiction clears the queue *and* re-locks display; evidence is
//!   re-earned on the next confirmed echo;
//! - mode-changing input (Esc, chords, arrows, function keys) kills the
//!   evidence — typing in vim normal mode after Esc can never paint a
//!   ghost on stale evidence;
//! - Enter suspends the burst instead of predicting (in a TUI it submits,
//!   not line-feeds) but keeps the evidence — a submit does not change
//!   who echoes;
//! - a front-of-queue prediction older than the display TTL hides the
//!   overlay until authority catches up (glitch back-off), on either
//!   screen.
//!
//! The driver feeds the screen mode ([`PredictionState::set_alt_screen`],
//! reading the same libghostty `terminal.mode()` query the mouse-tracking
//! and synchronized-output gates use), stamps guesses with a monotonic
//! clock (the `*_at` entry points), and paints via
//! [`PredictionState::should_display`] /
//! [`PredictionState::displayable`].
//!
//! # Off by default
//!
//! Predictive echo is gated behind [`PredictiveConfig::enabled`], wired
//! through `phux_client::attach::run_with_predict_dial`. The default is `false`
//! until the feature has miles on it. The TOML `[experimental]
//! predictive-echo = true` knob is parsed by `phux-config` and converted
//! into `PredictiveConfig { enabled: true, .. }` by the attach command.
//! Repeated contradictions turn the display tentative (the overlay hides,
//! prediction continues) until clean confirmations show typing has
//! normalized.
//!
//! # Reconciliation policy
//!
//! `reconcile_terminal_output_per_cell` is the production path for
//! `TERMINAL_OUTPUT`. It reads each prediction's target cell from the
//! freshly rendered authoritative grid and classifies it as
//! **confirmed** (drop, the server already painted it), **pending**
//! (keep, server hasn't caught up — overlay stays alive), or
//! **contradicted** (drop the prediction *and* every prediction behind
//! it — the server diverged so the suffix is suspect). See the
//! `reconcile` module for the per-`PredictionKind` truth table.

mod overlay;
mod reconcile;
mod state;

pub use overlay::Overlay;
pub use reconcile::{ReconcileStats, reconcile_terminal_output_per_cell};
pub use state::{Prediction, PredictionKind, PredictionOutcome, PredictionState, PredictiveConfig};
