//! Prediction state — the queue of in-flight predictions and the policy
//! that decides which keystrokes are safe to predict.
//!
//! The prediction state is *cursor-anchored*, not *cell-anchored*: each
//! prediction records the (row, col) at which it expects to paint, plus
//! the character to paint there. The state machine maintains a small
//! cursor estimate that walks forward by one column per printable
//! prediction and backward by one column per backspace prediction, so
//! consecutive predictions stack into a horizontal run.
//!
//! See the module-level docs in [`super`] for the rationale on which key
//! classes are predicted and why the visual decoration is underline.

use std::collections::VecDeque;

use phux_protocol::input::key::{KeyEvent, ModSet, PhysicalKey};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::reconcile::ReconcileStats;

/// Per-client knob for predictive echo.
///
/// Wire to `phux_client::attach::run_with_predict_dial`. Default is `enabled: false`
/// — predictive echo is off until field-proven. Future config keys
/// (timeout, decoration choice, RTT-adaptive predict policy) belong here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PredictiveConfig {
    /// Whether to apply predictive local echo at all. `false` ⇒ the
    /// attach loop bypasses the prediction layer entirely.
    pub enabled: bool,
}

impl PredictiveConfig {
    /// Convenience constructor — predictive echo on.
    #[must_use]
    pub const fn enabled() -> Self {
        Self { enabled: true }
    }

    /// Convenience constructor — predictive echo off (the default).
    #[must_use]
    pub const fn disabled() -> Self {
        Self { enabled: false }
    }
}

/// One in-flight prediction: a single-cell visual edit guessed from a
/// keystroke that has been sent upstream but not yet confirmed by a
/// `TerminalOutput` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prediction {
    /// Row of the cell, 0-indexed from the top of the viewport.
    pub row: u16,
    /// Column of the cell, 0-indexed. For cursor-motion predictions
    /// ([`PredictionKind::CursorLeft`] / [`PredictionKind::CursorRight`])
    /// this is the *target* column after the motion — the predict-side
    /// cursor has been moved there already and reconcile confirms when
    /// the authoritative cursor catches up to it.
    pub col: u16,
    /// What the prediction wants the cell to display. Usually a single
    /// scalar, but may be a multi-codepoint grapheme cluster — a flag
    /// emoji, a ZWJ family sequence, or a base plus combining marks
    /// (phux-9gw.1.6). For backspace predictions this is a single space
    /// (`" "`) — visually "erase". For cursor-motion predictions this is
    /// a placeholder space; the overlay paints no cell for them.
    pub text: String,
    /// Cell width of [`Self::text`], in columns. `1` for ASCII and most
    /// scripts, `2` for CJK / wide emoji and many ZWJ clusters
    /// (phux-9gw.1.4, phux-9gw.1.6). Cursor-motion and `Newline`
    /// predictions store `0` — they paint no cell.
    pub width: u8,
    /// Kind of prediction, for reconciliation diagnostics + the
    /// per-class confirm/contradict bookkeeping.
    pub kind: PredictionKind,
}

/// What the prediction was modelling. The per-cell reconcile path branches
/// on this to decide what counts as "confirmed" vs "contradicted" — see
/// the sibling `reconcile` module for the match table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredictionKind {
    /// A printable grapheme cluster: forward cursor, paint the cluster.
    /// Reconcile confirms when the authoritative cell at `(row, col)`
    /// has `text` as its grapheme cluster.
    Insert,
    /// A backspace at end-of-line: cursor back one column, blank the cell.
    /// Reconcile confirms when the authoritative cell at `(row, col)` is
    /// blank (no grapheme or a single space).
    BackspaceEol,
    /// An Enter keystroke after the user typed onto the current row:
    /// cursor jumps to `(row+1, 0)`. Paints no overlay cell — the
    /// prediction is purely a cursor-motion estimate so subsequent
    /// inserts on the next row anchor correctly. Reconcile confirms
    /// when the authoritative cursor has advanced past `pred.row`.
    Newline,
    /// Left arrow over a known cell on the current line (phux-9gw.1.3).
    /// Paints no overlay; reconcile confirms when the authoritative
    /// cursor matches `(pred.row, pred.col)`.
    CursorLeft,
    /// Right arrow over a known cell on the current line (phux-9gw.1.3).
    /// Paints no overlay; reconcile confirms when the authoritative
    /// cursor matches `(pred.row, pred.col)`.
    CursorRight,
}

/// What [`PredictionState::predict_key`] decided about a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredictionOutcome {
    /// The key was predicted; the new prediction is at `queue.back()`.
    Predicted,
    /// The key was outside the safe set — no prediction was made, but the
    /// key still travels upstream as normal. This is the "fall through"
    /// path that keeps the slow-but-correct behaviour for everything we
    /// don't yet feel safe predicting.
    Skipped,
    /// Predictive echo is disabled by config; the caller should treat this
    /// like `Skipped`.
    Disabled,
}

/// The client-side prediction state.
///
/// The state is updated in two places:
///
/// - [`Self::predict_key`] is called on the keystroke path, before the
///   `INPUT_KEY` frame is sent. It either enqueues a prediction (and
///   advances the cursor estimate) or returns [`PredictionOutcome::Skipped`].
/// - [`super::reconcile_terminal_output_per_cell`] is called on the
///   server-frame path when a `TerminalOutput` arrives. It confirms,
///   keeps, or drops predictions cell by cell and resyncs the cursor
///   estimate from authoritative state once the queue drains.
#[derive(Debug, Default)]
pub struct PredictionState {
    cfg: PredictiveConfig,
    /// FIFO of pending predictions. Ordered by issue time so the overlay
    /// can paint them left-to-right.
    pending: VecDeque<Prediction>,
    /// Best estimate of where the server's cursor will be after applying
    /// every pending prediction. Updated immediately by `predict_key`;
    /// re-synced from authoritative state by `reconcile_*`.
    cursor_row: u16,
    cursor_col: u16,
    /// Viewport size, in cells. The cursor estimate is clamped to
    /// `0..cols` and `0..rows` — we conservatively skip predicting at the
    /// last column to avoid guessing wrap behaviour.
    cols: u16,
    rows: u16,
    /// Client-side prompt-boundary heuristic (phux-9gw.1.5).
    ///
    /// `(row, col)` marks the column on `row` at which the *user's* typed
    /// input begins — everything to the left is prompt (or earlier output)
    /// that the predict layer must never erase. It is set to the cursor
    /// column at the moment of the first [`PredictionKind::Insert`] on a
    /// fresh input row, i.e. "the column where the user's first input
    /// landed". Backspace and Ctrl-U erasure predictions are clamped to
    /// stop *at* this column, never below it.
    ///
    /// `None` means the boundary is unknown for the current input row
    /// (nothing typed yet, or it was invalidated by a row change /
    /// viewport resize / contradiction). In that case the conservative
    /// fallbacks apply: single end-of-line backspace stays predicted, but
    /// full-line Ctrl-U erasure is refused rather than risk eating the
    /// prompt. Without OSC-133 shell integration this typed-input anchor
    /// is the safe signal available entirely client-side.
    prompt_boundary: Option<(u16, u16)>,
    /// When `true`, [`Self::predict_key`] is a no-op until an authoritative
    /// cursor sync ([`Self::set_cursor`]) re-arms it.
    ///
    /// Set by [`Self::suspend`] when the driver re-anchors to a pane whose
    /// cursor is not yet known — a freshly split pane that has not rendered.
    /// Without this, the re-anchor would fall back to `(0, 0)` and a quick
    /// keystroke would echo a predicted glyph at the screen's top-left, where
    /// the real pane never overwrites it — a lingering ghost after a fast
    /// split / focus change (phux-7ry0 follow-up). Defaults to `false` (armed).
    suspended: bool,
    /// Consecutive reconcile passes that contradicted a prediction, for
    /// the adaptive auto-back-off heuristic (phux-pxaj). When this reaches
    /// [`BACKOFF_THRESHOLD`] the state auto-suspends predicting: vi-mode
    /// shells, modal apps, and fast layout transitions all manifest as a
    /// run of contradictions where the server keeps painting cells we did
    /// not predict. Reset to `0` by a clean productive reconcile pass, not
    /// by [`Self::clear`] / [`Self::suspend`] (those run on every
    /// contradiction and must not erase the running tally).
    contradiction_streak: u32,
    /// Consecutive clean productive reconcile passes (something confirmed,
    /// nothing contradicted) observed *since* an auto-back-off suspend.
    /// When this reaches [`REARM_THRESHOLD`] the back-off is lifted and
    /// predicting resumes — typing has normalized. Reset to `0` by any
    /// contradiction. Like [`Self::contradiction_streak`], it survives
    /// [`Self::clear`] / [`Self::suspend`].
    clean_confirm_streak: u32,
    /// Distinguishes an auto-back-off suspend ([`Self::note_reconcile`])
    /// from the re-anchor suspend ([`Self::suspend`]). The re-anchor
    /// suspend is cleared by the very next authoritative cursor sync
    /// ([`Self::set_cursor`]); a back-off must *survive* cursor/snapshot
    /// syncs and only lift via the clean-confirm path, otherwise a single
    /// reconcile would re-arm predicting straight back into the mispredict
    /// storm it just backed off from (phux-pxaj).
    auto_backed_off: bool,
}

/// Consecutive contradicting reconcile passes that trip auto-back-off.
///
/// Hardcoded for this slice; doc-earmarked as a future `PredictiveConfig`
/// field (alongside the timeout / decoration / RTT knobs noted at
/// [`PredictiveConfig`]). Three in a row is enough to separate a genuine
/// mispredict mode (vi normal-mode, a modal app, a fast transition) from
/// an isolated one-off contradiction that the per-cell suffix-drop already
/// handles cheaply.
const BACKOFF_THRESHOLD: u32 = 3;

/// Consecutive clean productive reconcile passes that lift auto-back-off.
///
/// Hardcoded for this slice; future `PredictiveConfig` field. Two clean
/// productive passes (a prediction confirmed with nothing contradicted)
/// are required so a single fluke confirm during a mispredict storm does
/// not prematurely re-arm.
const REARM_THRESHOLD: u32 = 2;

impl PredictionState {
    /// New state with predictive echo configured per `cfg` and an
    /// initial viewport of `cols × rows`.
    #[must_use]
    pub const fn new(cfg: PredictiveConfig, cols: u16, rows: u16) -> Self {
        Self {
            cfg,
            pending: VecDeque::new(),
            cursor_row: 0,
            cursor_col: 0,
            cols,
            rows,
            prompt_boundary: None,
            suspended: false,
            contradiction_streak: 0,
            clean_confirm_streak: 0,
            auto_backed_off: false,
        }
    }

    /// Suspend prediction until the next authoritative cursor sync.
    ///
    /// Drops the pending queue and the prompt anchor, and makes
    /// [`Self::predict_key`] a no-op until [`Self::set_cursor`] re-arms it.
    /// The driver calls this when it re-anchors to a pane with no known
    /// cursor (a freshly split pane that has not rendered): predicting then
    /// would echo at a guessed `(0, 0)` and strand a ghost glyph at the
    /// screen's top-left.
    pub fn suspend(&mut self) {
        self.pending.clear();
        self.prompt_boundary = None;
        self.suspended = true;
    }

    /// Whether prediction is currently suspended (see [`Self::suspend`]).
    #[must_use]
    pub const fn is_suspended(&self) -> bool {
        self.suspended
    }

    /// Whether predictive echo is currently on.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.cfg.enabled
    }

    /// Update the cached viewport. Called on SIGWINCH / snapshot frames
    /// so the safe-prediction guard (skip last column, clamp rows)
    /// tracks the actual size.
    ///
    /// Resizing clears the prediction queue: the predicted cells'
    /// coordinates were anchored to the previous viewport and we'd
    /// rather drop them than risk painting at a stale position.
    pub fn set_viewport(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.pending.clear();
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        // A resize reflows the grid; the typed-input anchor no longer
        // describes the new layout. Forget it (phux-9gw.1.5).
        self.prompt_boundary = None;
    }

    /// Re-anchor the cursor estimate from authoritative state. Called
    /// from the reconcile path after a `TerminalOutput` has been
    /// applied to the libghostty Terminal — the renderer's `RenderState`
    /// has the post-apply cursor, and we copy it here.
    pub fn set_cursor(&mut self, row: u16, col: u16) {
        let new_row = row.min(self.rows.saturating_sub(1));
        // The prompt-boundary anchor (phux-9gw.1.5) is bound to a single
        // input row. A same-row resync (the common "server echoed the
        // bytes we just typed" case, which drains the queue and lands
        // here) keeps the anchor alive so a follow-up backspace/Ctrl-U
        // still knows where the prompt ends. A row change means a new
        // input context — forget the anchor and re-learn it from the next
        // insert. Holding a stale anchor for a row we are not on is the
        // dangerous case, so we drop it.
        if self
            .prompt_boundary
            .is_some_and(|(brow, _)| brow != new_row)
        {
            self.prompt_boundary = None;
        }
        self.cursor_row = new_row;
        self.cursor_col = col.min(self.cols.saturating_sub(1));
        // An authoritative cursor (snapshot render or output reconcile) means
        // we now know where the focused pane's cursor is — re-arm prediction
        // if a re-anchor had suspended it. An adaptive auto-back-off suspend
        // (phux-pxaj) is the exception: it must survive cursor/snapshot syncs
        // and lift only via the clean-confirm path in
        // [`Self::note_reconcile`], otherwise a single reconcile would re-arm
        // predicting straight back into the mispredict storm it backed off
        // from.
        if !self.auto_backed_off {
            self.suspended = false;
        }
    }

    /// Feed the outcome of one per-cell reconcile pass into the adaptive
    /// auto-back-off heuristic (phux-pxaj).
    ///
    /// Predictive echo mispredicts hard in a few recurring modes: vi-mode
    /// shells (normal-mode `h`/`j`/`k`/`l`/`x` are commands, not inserts),
    /// modal full-screen apps, and fast layout/screen transitions. Each
    /// manifests the same way — the server keeps *contradicting* what we
    /// predicted. This heuristic watches the contradiction rate and:
    ///
    /// - after `BACKOFF_THRESHOLD` consecutive contradicting passes,
    ///   auto-suspends predicting (drops the queue, stops echoing) so the
    ///   user stops seeing ghosts the server immediately overwrites;
    /// - after `REARM_THRESHOLD` consecutive *clean productive* passes
    ///   (something confirmed, nothing contradicted) following a back-off,
    ///   lifts the suspend and resumes — typing has normalized.
    ///
    /// A pass is classified from `stats`:
    /// - `contradicted > 0` ⇒ a contradicting pass (regardless of any
    ///   confirms in the same pass — a confirmed prefix followed by a
    ///   contradicted suffix still means we guessed wrong);
    /// - `contradicted == 0 && confirmed > 0` ⇒ a clean productive pass;
    /// - `confirmed == 0 && contradicted == 0` (only pendings, or an empty
    ///   queue) ⇒ **neutral** — leaves both streaks untouched so a quiet
    ///   link neither trips nor lifts the back-off.
    ///
    /// Called from [`super::reconcile_terminal_output_per_cell`] just before
    /// it returns, so every per-cell reconcile caller gets back-off for
    /// free.
    pub fn note_reconcile(&mut self, stats: ReconcileStats) {
        if stats.contradicted > 0 {
            self.contradiction_streak = self.contradiction_streak.saturating_add(1);
            self.clean_confirm_streak = 0;
            if self.contradiction_streak >= BACKOFF_THRESHOLD {
                self.auto_suspend();
            }
        } else if stats.confirmed > 0 {
            // Clean productive pass.
            self.contradiction_streak = 0;
            self.clean_confirm_streak = self.clean_confirm_streak.saturating_add(1);
            if self.auto_backed_off && self.clean_confirm_streak >= REARM_THRESHOLD {
                // Typing normalized — lift the back-off and re-arm.
                self.auto_backed_off = false;
                self.suspended = false;
                self.clean_confirm_streak = 0;
            }
        }
        // Neutral pass (nothing confirmed, nothing contradicted): leave
        // both streaks alone.
    }

    /// Suspend predicting because the server has been contradicting our
    /// guesses (phux-pxaj). Unlike the re-anchor [`Self::suspend`], this
    /// sets [`Self::auto_backed_off`] so the suspend survives authoritative
    /// cursor syncs and only lifts via the clean-confirm path in
    /// [`Self::note_reconcile`].
    fn auto_suspend(&mut self) {
        self.pending.clear();
        self.prompt_boundary = None;
        self.suspended = true;
        self.auto_backed_off = true;
        // Start counting clean passes afresh for the re-arm decision.
        self.clean_confirm_streak = 0;
    }

    /// Whether predicting is currently suspended by adaptive auto-back-off
    /// (phux-pxaj), as opposed to the re-anchor suspend. Exposed for
    /// diagnostics and tests.
    #[must_use]
    pub const fn is_auto_backed_off(&self) -> bool {
        self.auto_backed_off
    }

    /// Number of predictions waiting for confirmation.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Current cursor estimate `(row, col)`. Updated by `predict_key`
    /// and `set_cursor`; exposed for diagnostics and tests.
    #[must_use]
    pub const fn cursor(&self) -> (u16, u16) {
        (self.cursor_row, self.cursor_col)
    }

    /// Read-only access to the pending queue (used by the overlay).
    pub fn pending(&self) -> impl Iterator<Item = &Prediction> {
        self.pending.iter()
    }

    /// Drop every pending prediction. Called by the reconcile path.
    ///
    /// A `clear` from the reconcile path means the server contradicted our
    /// guesses: the queue suffix is suspect, so the typed-input anchor we
    /// derived from those guesses is suspect too. Forget it rather than
    /// erase to a column the server may not agree with (phux-9gw.1.5).
    pub fn clear(&mut self) {
        self.pending.clear();
        self.prompt_boundary = None;
    }

    /// Current prompt-boundary anchor `(row, col)`, if known for the
    /// current input row. Exposed for diagnostics and tests
    /// (phux-9gw.1.5).
    #[must_use]
    pub const fn prompt_boundary(&self) -> Option<(u16, u16)> {
        self.prompt_boundary
    }

    /// Drop the prediction at the front of the queue. Used by the
    /// per-cell reconcile to consume confirmed predictions one at a
    /// time. Returns the dropped prediction so callers can log.
    pub(crate) fn pop_front(&mut self) -> Option<Prediction> {
        self.pending.pop_front()
    }

    /// Peek at the prediction at the front of the queue.
    pub(crate) fn front(&self) -> Option<&Prediction> {
        self.pending.front()
    }

    /// Try to predict the visual effect of `event`. Returns the outcome
    /// so the caller can record it (and, in tests, assert on the
    /// classification decision).
    ///
    /// Safe classes (see module docs):
    ///
    /// - A `text` payload that is exactly one grapheme cluster (a single
    ///   printable scalar, or a multi-scalar cluster such as a flag emoji,
    ///   a ZWJ family sequence, or a base plus combining marks) with cell
    ///   width 1 or 2 per `unicode-width`, no Ctrl / Alt / Super modifier
    ///   active. SHIFT is fine (it's part of producing the character).
    /// - `PhysicalKey::Backspace`, no modifier, when the cursor is past
    ///   column 0 and below the last column.
    /// - `PhysicalKey::Enter`, no modifier, past column 0 and on a row
    ///   that is not the last viewport row.
    ///
    /// Cursor-motion arrow predictions ([`PhysicalKey::ArrowLeft`] /
    /// [`PhysicalKey::ArrowRight`]) require a peek at the cell grid so
    /// the predict layer knows the width of the grapheme being stepped
    /// over; they are routed through [`Self::predict_key_with_grid`].
    /// This entry point skips arrows.
    pub fn predict_key(&mut self, event: &KeyEvent) -> PredictionOutcome {
        self.predict_key_with_grid(event, |_, _| None)
    }

    /// Same as [`Self::predict_key`] but with a read closure into the
    /// authoritative cell grid. Used by the attach driver, which has
    /// the libghostty `Terminal` + `TerminalRenderer` on hand and can
    /// supply a per-cell grapheme lookup via `read_grapheme_at`.
    ///
    /// The closure is invoked at most once per call, only when a
    /// cursor-motion arrow needs to know the width of the grapheme it
    /// would step over.
    pub fn predict_key_with_grid<F>(
        &mut self,
        event: &KeyEvent,
        mut read_cell: F,
    ) -> PredictionOutcome
    where
        F: FnMut(u16, u16) -> Option<char>,
    {
        if !self.cfg.enabled {
            return PredictionOutcome::Disabled;
        }
        // Suspended after a re-anchor to a pane with no known cursor: predict
        // nothing until an authoritative cursor sync re-arms us, so a fast
        // keystroke after a split can't echo a ghost at the guessed (0, 0).
        if self.suspended {
            return PredictionOutcome::Skipped;
        }
        // Reject any non-Press action — repeats and releases produce
        // their own server-side echo path we don't model yet.
        if !matches!(event.action, phux_protocol::input::key::KeyAction::Press) {
            return PredictionOutcome::Skipped;
        }
        // Ctrl-U (kill-to-start-of-line) is the one CTRL chord we predict
        // (phux-9gw.1.5). It carries CTRL, so it must be handled *before*
        // the generic command-modifier reject below. We only predict it
        // when the prompt boundary is known for the current row — the
        // full-line erase is exactly the case that risks eating the
        // prompt, so an unknown boundary means refuse.
        if event.key == PhysicalKey::U && event.mods == ModSet::CTRL {
            return self.predict_kill_to_boundary();
        }
        // Reject keystrokes with any "command-y" modifier active. SHIFT
        // is OK because it's already baked into `text` for letters.
        let blocking_mods = ModSet::CTRL | ModSet::ALT | ModSet::SUPER;
        if event.mods.intersects(blocking_mods) {
            return PredictionOutcome::Skipped;
        }

        if event.key == PhysicalKey::Backspace {
            return self.predict_backspace_eol();
        }
        if event.key == PhysicalKey::Enter {
            return self.predict_enter();
        }
        if event.key == PhysicalKey::ArrowLeft {
            return self.predict_arrow_left(&mut read_cell);
        }
        if event.key == PhysicalKey::ArrowRight {
            return self.predict_arrow_right(&mut read_cell);
        }

        // Printable insert. The `text` payload is one grapheme cluster in
        // the common case, but may carry several Unicode scalars that form
        // a single visual cell: a flag emoji (U+1F1FA U+1F1F8), a ZWJ
        // family sequence, or a base plus combining marks (phux-9gw.1.6).
        // Split into clusters and predict only when the payload is exactly
        // one cluster — a multi-cluster payload is paste-like and outside
        // the single-keystroke model we predict.
        let Some(text) = event.text.as_deref() else {
            return PredictionOutcome::Skipped;
        };
        let mut clusters = text.graphemes(true);
        let (Some(cluster), None) = (clusters.next(), clusters.next()) else {
            // Empty `text`, or more than one grapheme cluster (paste-like).
            return PredictionOutcome::Skipped;
        };
        if !is_safe_predictable_cluster(cluster) {
            return PredictionOutcome::Skipped;
        }
        let Some(width) = cluster_width(cluster) else {
            return PredictionOutcome::Skipped;
        };
        self.predict_insert(cluster, width)
    }

    /// Predict an Enter keystroke as a cursor jump to `(row+1, 0)`.
    ///
    /// Conservative gate: only predict when (a) the cursor is past
    /// column 0 *and* there is already an `Insert` prediction queued on
    /// the current row, or (b) the cursor is past column 0 outright.
    /// Condition (a) is a strong proxy for "the user just typed text on
    /// this line, so the next Enter is line submission" — the bulk of
    /// the latency-hiding win. Without (a) we still predict, because
    /// the cell-match reconcile will drop the Newline cheaply on
    /// contradiction.
    ///
    /// Refuses to predict at the last row (would need to model scroll)
    /// or at column 0 (would need to know whether the shell discards a
    /// bare Enter or echoes a fresh prompt).
    fn predict_enter(&mut self) -> PredictionOutcome {
        if self.cursor_col == 0 {
            return PredictionOutcome::Skipped;
        }
        if self.rows == 0 || self.cursor_row.saturating_add(1) >= self.rows {
            return PredictionOutcome::Skipped;
        }
        let pred_row = self.cursor_row;
        self.pending.push_back(Prediction {
            row: pred_row,
            col: self.cursor_col,
            text: "\n".to_owned(),
            width: 0,
            kind: PredictionKind::Newline,
        });
        // Advance the cursor estimate so subsequent inserts queue on the
        // next row at column 0. The next line is a new input context, so
        // forget the current prompt-boundary anchor (phux-9gw.1.5) — it
        // is re-learned from the first insert on the new row.
        self.cursor_row = self.cursor_row.saturating_add(1);
        self.cursor_col = 0;
        self.prompt_boundary = None;
        PredictionOutcome::Predicted
    }

    fn predict_insert(&mut self, cluster: &str, width: u8) -> PredictionOutcome {
        if self.cols == 0 || self.rows == 0 || self.cursor_row >= self.rows {
            return PredictionOutcome::Skipped;
        }
        // Conservative: refuse to predict at or past the rightmost column.
        // The server may wrap, may scroll, may stay (DECAWM off) — we
        // don't know which from here. For width-2 graphemes we also need
        // the *next* column to fit. The next reconcile will resync.
        let advance = u16::from(width);
        if advance == 0 {
            return PredictionOutcome::Skipped;
        }
        let end_col = self.cursor_col.saturating_add(advance);
        if end_col >= self.cols {
            return PredictionOutcome::Skipped;
        }
        // Learn the prompt boundary (phux-9gw.1.5): the column where the
        // user's first input lands on this row marks where typed input
        // begins. Everything to the left is prompt that erasure must never
        // touch. Record it only when there is no anchor yet for this row —
        // later inserts on the same line must not advance it.
        if self
            .prompt_boundary
            .is_none_or(|(brow, _)| brow != self.cursor_row)
        {
            self.prompt_boundary = Some((self.cursor_row, self.cursor_col));
        }
        self.pending.push_back(Prediction {
            row: self.cursor_row,
            col: self.cursor_col,
            text: cluster.to_owned(),
            width,
            kind: PredictionKind::Insert,
        });
        self.cursor_col = end_col;
        PredictionOutcome::Predicted
    }

    fn predict_backspace_eol(&mut self) -> PredictionOutcome {
        // Backspace at column 0 is "move to end of previous line" on most
        // shells (or no-op). We refuse to guess either way.
        if self.cursor_col == 0 {
            return PredictionOutcome::Skipped;
        }
        if self.rows == 0 || self.cursor_row >= self.rows {
            return PredictionOutcome::Skipped;
        }
        // Prompt-boundary guard (phux-9gw.1.5): if we know where the
        // user's typed input begins on this row, never predict erasing at
        // or below it — that cell is the prompt. When the boundary is
        // unknown we keep the conservative end-of-line behaviour (erase
        // one cell): without OSC-133 there is no safer signal, and this
        // single-cell case is the "typing then immediately deleting" path
        // that motivated the feature.
        if let Some((brow, bcol)) = self.prompt_boundary
            && brow == self.cursor_row
            && self.cursor_col <= bcol
        {
            return PredictionOutcome::Skipped;
        }
        let new_col = self.cursor_col - 1;
        self.pending.push_back(Prediction {
            row: self.cursor_row,
            col: new_col,
            text: " ".to_owned(),
            width: 1,
            kind: PredictionKind::BackspaceEol,
        });
        self.cursor_col = new_col;
        PredictionOutcome::Predicted
    }

    /// Predict Ctrl-U (kill-to-start-of-line) by erasing every typed cell
    /// from the prompt boundary up to the cursor (phux-9gw.1.5).
    ///
    /// This is the full-line backspace the ticket targets. It is only safe
    /// when we know where the user's typed input begins on this row: the
    /// prompt-boundary anchor learned from the first insert. Each erased
    /// cell becomes a [`PredictionKind::BackspaceEol`] prediction (a blank
    /// paint), and the cursor estimate retreats to the boundary column —
    /// exactly the row's prompt-end, never past it.
    ///
    /// Refuses (falls back to no prediction) when:
    /// - the boundary is unknown for the current row — without OSC-133 we
    ///   cannot tell prompt from typed input, so guessing would risk
    ///   erasing the prompt;
    /// - the cursor is already at or left of the boundary — nothing typed
    ///   to erase;
    /// - the viewport is degenerate.
    ///
    /// Note this does not model Ctrl-U's full readline semantics (some
    /// shells kill to start-of-line regardless of cursor position, others
    /// kill from the cursor backwards). We predict the conservative
    /// "erase the typed run we know about" subset; the per-cell reconcile
    /// drops any cell the server disagrees with.
    fn predict_kill_to_boundary(&mut self) -> PredictionOutcome {
        if self.rows == 0 || self.cursor_row >= self.rows {
            return PredictionOutcome::Skipped;
        }
        let Some((brow, bcol)) = self.prompt_boundary else {
            // Unknown boundary: refuse rather than risk eating the prompt.
            return PredictionOutcome::Skipped;
        };
        if brow != self.cursor_row || self.cursor_col <= bcol {
            return PredictionOutcome::Skipped;
        }
        // Erase typed cells right-to-left, blanking each column from the
        // cursor down to (but not below) the boundary.
        let mut col = self.cursor_col;
        while col > bcol {
            col -= 1;
            self.pending.push_back(Prediction {
                row: self.cursor_row,
                col,
                text: " ".to_owned(),
                width: 1,
                kind: PredictionKind::BackspaceEol,
            });
        }
        self.cursor_col = bcol;
        PredictionOutcome::Predicted
    }

    /// Predict a left-arrow keystroke (phux-9gw.1.3).
    ///
    /// Reads the cell immediately to the left of the predict cursor via
    /// `read_cell`. If that cell has a known grapheme we retreat by its
    /// cell width; if the cell is blank we refuse to predict — we have
    /// no anchor to know whether the cursor would land on the prompt,
    /// on a wide grapheme's tail, or on blank text. Refuses at column 0.
    fn predict_arrow_left<F>(&mut self, read_cell: &mut F) -> PredictionOutcome
    where
        F: FnMut(u16, u16) -> Option<char>,
    {
        if self.cursor_col == 0 {
            return PredictionOutcome::Skipped;
        }
        if self.rows == 0 || self.cursor_row >= self.rows {
            return PredictionOutcome::Skipped;
        }
        let probe_col = self.cursor_col - 1;
        let Some(ch) = read_cell(self.cursor_row, probe_col) else {
            return PredictionOutcome::Skipped;
        };
        let Some(width) = grapheme_width(ch) else {
            return PredictionOutcome::Skipped;
        };
        let advance = u16::from(width);
        if advance == 0 {
            return PredictionOutcome::Skipped;
        }
        // Width-2 case: the grapheme's base cell is at `probe_col - 1`
        // and the tail occupies `probe_col`. We want to land the cursor
        // on the base. Refuse if that would go below 0.
        let new_col = if advance == 1 {
            probe_col
        } else if self.cursor_col >= advance {
            self.cursor_col - advance
        } else {
            return PredictionOutcome::Skipped;
        };
        self.pending.push_back(Prediction {
            row: self.cursor_row,
            col: new_col,
            text: " ".to_owned(),
            width: 0,
            kind: PredictionKind::CursorLeft,
        });
        self.cursor_col = new_col;
        PredictionOutcome::Predicted
    }

    /// Predict a right-arrow keystroke (phux-9gw.1.3).
    ///
    /// Reads the cell at the predict cursor. If that cell has a known
    /// grapheme we advance by its cell width; blank → refuse. Refuses
    /// past the right edge.
    fn predict_arrow_right<F>(&mut self, read_cell: &mut F) -> PredictionOutcome
    where
        F: FnMut(u16, u16) -> Option<char>,
    {
        if self.cols == 0 || self.cursor_col >= self.cols {
            return PredictionOutcome::Skipped;
        }
        if self.rows == 0 || self.cursor_row >= self.rows {
            return PredictionOutcome::Skipped;
        }
        let Some(ch) = read_cell(self.cursor_row, self.cursor_col) else {
            return PredictionOutcome::Skipped;
        };
        let Some(width) = grapheme_width(ch) else {
            return PredictionOutcome::Skipped;
        };
        let advance = u16::from(width);
        if advance == 0 {
            return PredictionOutcome::Skipped;
        }
        let new_col = self.cursor_col.saturating_add(advance);
        // Refuse to land at or past the rightmost column — same
        // wrap-conservatism as `predict_insert`.
        if new_col >= self.cols {
            return PredictionOutcome::Skipped;
        }
        self.pending.push_back(Prediction {
            row: self.cursor_row,
            col: new_col,
            text: " ".to_owned(),
            width: 0,
            kind: PredictionKind::CursorRight,
        });
        self.cursor_col = new_col;
        PredictionOutcome::Predicted
    }
}

/// Reject control codes (anything below 0x20 except space, plus DEL).
/// Everything above U+007E is delegated to [`grapheme_width`] — the
/// `unicode-width` lookup correctly classifies combining marks as
/// width 0 (skipped) and CJK / wide emoji as width 2.
const fn is_safe_predictable(ch: char) -> bool {
    let c = ch as u32;
    // ASCII control range (0x00..=0x1F) and DEL (0x7F) are not safe to
    // predict; the shell may or may not echo them depending on stty
    // settings, and even space-as-character is fine because c == 0x20.
    if c < 0x20 {
        return false;
    }
    if c == 0x7F {
        return false;
    }
    true
}

/// Cell width of a single Unicode scalar value, in terminal columns.
/// Returns `None` for non-printable code points (`UnicodeWidthChar`
/// returns `None` for control codes).
///
/// Width is capped at 2 — `unicode-width` never returns more than 2
/// for a single scalar, but the explicit cap defends against a future
/// crate-rev change.
///
/// Used by the cursor-motion arrow predictions, which step over a single
/// cell whose base scalar is read from the authoritative grid. Multi-
/// scalar inserts go through [`cluster_width`] instead.
fn grapheme_width(ch: char) -> Option<u8> {
    let w = UnicodeWidthChar::width(ch)?;
    if w == 0 {
        // A lone combining mark, zero-width joiner, or variation selector
        // has no base to attach to — refuse rather than paint at width 0.
        return None;
    }
    Some(u8::try_from(w.min(2)).unwrap_or(1))
}

/// Whether a grapheme cluster is safe to predict. Rejects clusters whose
/// first scalar is an ASCII control code or DEL — the shell may or may
/// not echo those depending on `stty`, so we never guess. Everything
/// else (printable ASCII, Latin-1, CJK, emoji and ZWJ sequences) is
/// delegated to [`cluster_width`] for the width decision.
fn is_safe_predictable_cluster(cluster: &str) -> bool {
    cluster.chars().next().is_some_and(is_safe_predictable)
}

/// Cell width of a whole grapheme cluster, in terminal columns.
///
/// A cluster may span several scalars (flag emoji, ZWJ family sequence,
/// base plus combining marks) whose combined display width is not the
/// width of any single scalar: `UnicodeWidthStr::width` measures the
/// string, so a base-plus-combining cluster stays width 1 while a flag
/// or ZWJ emoji is width 2. Returns `None` when the measured width is 0
/// (a cluster with no advancing scalar — nothing to anchor a prediction
/// to). Width is capped at 2: the terminal renders any wider cluster as
/// a width-2 emoji cell, and predicting more columns than the grid will
/// paint risks a contradicting reconcile.
fn cluster_width(cluster: &str) -> Option<u8> {
    let w = UnicodeWidthStr::width(cluster);
    if w == 0 {
        return None;
    }
    Some(u8::try_from(w.min(2)).unwrap_or(1))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_protocol::input::key::KeyAction;

    fn key_text(s: &str) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::A,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: Some(s.to_owned()),
            unshifted_codepoint: s.chars().next().map(u32::from),
        }
    }

    fn key_named(k: PhysicalKey, mods: ModSet) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key: k,
            mods,
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        }
    }

    #[test]
    fn disabled_config_skips_all_predictions() {
        let mut s = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
        let r = s.predict_key(&key_text("a"));
        assert_eq!(r, PredictionOutcome::Disabled);
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn printable_ascii_is_predicted_and_advances_cursor() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let r = s.predict_key(&key_text("a"));
        assert_eq!(r, PredictionOutcome::Predicted);
        assert_eq!(s.pending_len(), 1);
        let p = s.pending().next().expect("one prediction");
        assert_eq!(p.text, "a");
        assert_eq!(p.col, 0);
        assert_eq!(p.row, 0);
        assert_eq!(p.kind, PredictionKind::Insert);
        assert_eq!(s.cursor_col, 1);
    }

    #[test]
    fn suspended_predict_key_is_a_no_op_until_set_cursor_rearms() {
        // The split-ghost guard: after a re-anchor to a not-yet-rendered pane
        // the driver suspends; a quick keystroke must NOT queue a prediction
        // (which would otherwise echo at the guessed (0,0)). The first
        // authoritative cursor sync re-arms.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.suspend();
        assert!(s.is_suspended());
        assert_eq!(s.predict_key(&key_text("a")), PredictionOutcome::Skipped);
        assert_eq!(s.pending_len(), 0, "suspended: nothing queued, no ghost");

        // An authoritative cursor sync re-arms prediction.
        s.set_cursor(3, 5);
        assert!(!s.is_suspended());
        assert_eq!(s.predict_key(&key_text("a")), PredictionOutcome::Predicted);
        let p = s.pending().next().expect("one prediction");
        assert_eq!(
            (p.row, p.col),
            (3, 5),
            "echo at the synced cursor, not (0,0)"
        );
    }

    #[test]
    fn suspend_drops_any_pending_predictions() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        assert_eq!(s.predict_key(&key_text("a")), PredictionOutcome::Predicted);
        assert_eq!(s.pending_len(), 1);
        s.suspend();
        assert_eq!(s.pending_len(), 0, "suspend clears the queue");
        assert!(s.is_suspended());
    }

    #[test]
    fn multiple_keystrokes_stack_horizontally() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for ch in ["h", "e", "l", "l", "o"] {
            assert_eq!(s.predict_key(&key_text(ch)), PredictionOutcome::Predicted);
        }
        let cols: Vec<u16> = s.pending().map(|p| p.col).collect();
        assert_eq!(cols, vec![0, 1, 2, 3, 4]);
        assert_eq!(s.cursor_col, 5);
    }

    #[test]
    fn ctrl_modifier_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let mut ev = key_text("a");
        ev.mods = ModSet::CTRL;
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn alt_modifier_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let mut ev = key_text("a");
        ev.mods = ModSet::ALT;
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
    }

    #[test]
    fn non_press_action_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let mut ev = key_text("a");
        ev.action = KeyAction::Release;
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
    }

    #[test]
    fn arrow_key_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let ev = key_named(PhysicalKey::ArrowLeft, ModSet::empty());
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
    }

    #[test]
    fn enter_at_col_zero_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let ev = key_named(PhysicalKey::Enter, ModSet::empty());
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn enter_after_insert_predicts_newline_and_advances_row() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for ch in ["h", "i"] {
            assert_eq!(s.predict_key(&key_text(ch)), PredictionOutcome::Predicted);
        }
        assert_eq!(s.cursor(), (0, 2));
        let enter = key_named(PhysicalKey::Enter, ModSet::empty());
        assert_eq!(s.predict_key(&enter), PredictionOutcome::Predicted);
        assert_eq!(s.cursor(), (1, 0));
        assert_eq!(s.pending_len(), 3);
        let last = s.pending().last().expect("three predictions");
        assert_eq!(last.kind, PredictionKind::Newline);
        assert_eq!(last.row, 0);
        assert_eq!(last.col, 2);
    }

    #[test]
    fn enter_on_last_row_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        // Park the cursor on the last row, past col 0.
        s.set_cursor(23, 5);
        let enter = key_named(PhysicalKey::Enter, ModSet::empty());
        assert_eq!(s.predict_key(&enter), PredictionOutcome::Skipped);
    }

    #[test]
    fn tab_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let ev = key_named(PhysicalKey::Tab, ModSet::empty());
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
    }

    #[test]
    fn backspace_at_col_zero_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let ev = key_named(PhysicalKey::Backspace, ModSet::empty());
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
    }

    #[test]
    fn backspace_after_insert_is_predicted_and_decrements_cursor() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        assert_eq!(s.predict_key(&key_text("a")), PredictionOutcome::Predicted);
        assert_eq!(s.cursor_col, 1);
        let bs = key_named(PhysicalKey::Backspace, ModSet::empty());
        assert_eq!(s.predict_key(&bs), PredictionOutcome::Predicted);
        assert_eq!(s.cursor_col, 0);
        assert_eq!(s.pending_len(), 2);
        let last = s.pending().last().expect("two predictions");
        assert_eq!(last.kind, PredictionKind::BackspaceEol);
        assert_eq!(last.col, 0);
        assert_eq!(last.text, " ");
    }

    // ---- phux-9gw.1.5: prompt-boundary heuristic + Ctrl-U -------------

    fn key_ctrl_u() -> KeyEvent {
        let mut ev = key_named(PhysicalKey::U, ModSet::CTRL);
        ev.text = None;
        ev
    }

    #[test]
    fn first_insert_records_prompt_boundary_at_cursor() {
        // The user starts typing at col 5 (a prompt occupies cols 0..5).
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 5);
        assert_eq!(s.prompt_boundary(), None);
        s.predict_key(&key_text("a"));
        assert_eq!(s.prompt_boundary(), Some((0, 5)));
        // A second insert on the same row does not advance the anchor.
        s.predict_key(&key_text("b"));
        assert_eq!(s.prompt_boundary(), Some((0, 5)));
    }

    #[test]
    fn backspace_predicts_within_typed_input() {
        // Prompt ends at col 3; type two chars (cols 3, 4), then backspace.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 3);
        s.predict_key(&key_text("x"));
        s.predict_key(&key_text("y"));
        assert_eq!(s.cursor(), (0, 5));
        let bs = key_named(PhysicalKey::Backspace, ModSet::empty());
        assert_eq!(s.predict_key(&bs), PredictionOutcome::Predicted);
        assert_eq!(s.cursor(), (0, 4));
        // Second backspace lands on the boundary (col 3) — still typed.
        assert_eq!(s.predict_key(&bs), PredictionOutcome::Predicted);
        assert_eq!(s.cursor(), (0, 3));
    }

    #[test]
    fn backspace_stops_at_prompt_boundary() {
        // Prompt ends at col 3; type one char then backspace twice. The
        // first backspace lands on the boundary (col 3); the second would
        // erase the prompt cell at col 2 — it must be refused.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 3);
        s.predict_key(&key_text("x"));
        assert_eq!(s.cursor(), (0, 4));
        let bs = key_named(PhysicalKey::Backspace, ModSet::empty());
        assert_eq!(s.predict_key(&bs), PredictionOutcome::Predicted);
        assert_eq!(s.cursor(), (0, 3));
        // Now at the boundary: cursor_col (3) <= bcol (3) → refuse.
        assert_eq!(s.predict_key(&bs), PredictionOutcome::Skipped);
        assert_eq!(s.cursor(), (0, 3));
    }

    #[test]
    fn backspace_without_known_boundary_keeps_eol_fallback() {
        // No typed input recorded yet (boundary unknown). The cursor sits
        // past col 0 — the conservative single end-of-line backspace is
        // still predicted (the shipped behaviour the feature must not
        // regress).
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 6);
        assert_eq!(s.prompt_boundary(), None);
        let bs = key_named(PhysicalKey::Backspace, ModSet::empty());
        assert_eq!(s.predict_key(&bs), PredictionOutcome::Predicted);
        assert_eq!(s.cursor(), (0, 5));
    }

    #[test]
    fn ctrl_u_erases_typed_run_down_to_boundary() {
        // Prompt ends at col 4; type "hi" (cols 4, 5). Ctrl-U erases both
        // typed cells and parks the cursor on the boundary.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 4);
        s.predict_key(&key_text("h"));
        s.predict_key(&key_text("i"));
        assert_eq!(s.cursor(), (0, 6));
        let before = s.pending_len();
        assert_eq!(s.predict_key(&key_ctrl_u()), PredictionOutcome::Predicted);
        assert_eq!(s.cursor(), (0, 4));
        // Two erase predictions appended (cols 5 then 4), blanking the
        // typed run, never the prompt.
        let erased: Vec<(u16, &str)> = s
            .pending()
            .skip(before)
            .map(|p| (p.col, p.text.as_str()))
            .collect();
        assert_eq!(erased, vec![(5, " "), (4, " ")]);
        for p in s.pending().skip(before) {
            assert_eq!(p.kind, PredictionKind::BackspaceEol);
            assert!(p.col >= 4, "never erase below the prompt boundary");
        }
    }

    #[test]
    fn ctrl_u_without_known_boundary_is_not_predicted() {
        // Boundary unknown: predicting a full-line erase risks eating the
        // prompt. Refuse — fall through to the server (phux-9gw.1.5).
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 8);
        assert_eq!(s.prompt_boundary(), None);
        assert_eq!(s.predict_key(&key_ctrl_u()), PredictionOutcome::Skipped);
        assert_eq!(s.pending_len(), 0);
        assert_eq!(s.cursor(), (0, 8));
    }

    #[test]
    fn ctrl_u_at_boundary_with_nothing_typed_is_not_predicted() {
        // Boundary known but cursor already sits on it (the typed run was
        // already erased). Nothing left to kill → refuse.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 5);
        s.predict_key(&key_text("a"));
        let bs = key_named(PhysicalKey::Backspace, ModSet::empty());
        s.predict_key(&bs); // back to boundary col 5
        assert_eq!(s.cursor(), (0, 5));
        assert_eq!(s.predict_key(&key_ctrl_u()), PredictionOutcome::Skipped);
    }

    #[test]
    fn prompt_boundary_survives_same_row_reconcile_resync() {
        // The server echoes what we typed; reconcile resyncs the cursor on
        // the same row. The anchor must persist so a follow-up Ctrl-U
        // still knows where the prompt ends.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 3);
        s.predict_key(&key_text("a"));
        assert_eq!(s.prompt_boundary(), Some((0, 3)));
        // Same-row resync (e.g. drain after server echo at col 4).
        s.set_cursor(0, 4);
        assert_eq!(s.prompt_boundary(), Some((0, 3)));
    }

    #[test]
    fn prompt_boundary_forgotten_on_row_change() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 3);
        s.predict_key(&key_text("a"));
        assert_eq!(s.prompt_boundary(), Some((0, 3)));
        s.set_cursor(2, 0); // different row → new input context
        assert_eq!(s.prompt_boundary(), None);
    }

    #[test]
    fn enter_resets_prompt_boundary() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 2);
        s.predict_key(&key_text("a"));
        assert_eq!(s.prompt_boundary(), Some((0, 2)));
        let enter = key_named(PhysicalKey::Enter, ModSet::empty());
        assert_eq!(s.predict_key(&enter), PredictionOutcome::Predicted);
        assert_eq!(s.prompt_boundary(), None);
    }

    #[test]
    fn set_viewport_clears_prompt_boundary() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 2);
        s.predict_key(&key_text("a"));
        assert_eq!(s.prompt_boundary(), Some((0, 2)));
        s.set_viewport(100, 30);
        assert_eq!(s.prompt_boundary(), None);
    }

    #[test]
    fn clear_forgets_prompt_boundary() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 2);
        s.predict_key(&key_text("a"));
        assert_eq!(s.prompt_boundary(), Some((0, 2)));
        s.clear();
        assert_eq!(s.prompt_boundary(), None);
    }

    #[test]
    fn ctrl_u_with_other_modifier_is_not_predicted() {
        // Ctrl-Shift-U or Ctrl-Alt-U is not the kill-line chord — only a
        // bare CTRL+U is handled. Anything else falls to the generic
        // command-modifier reject.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 4);
        s.predict_key(&key_text("h"));
        let mut ev = key_named(PhysicalKey::U, ModSet::CTRL | ModSet::ALT);
        ev.text = None;
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
    }

    #[test]
    fn rightmost_column_insert_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 5, 24);
        // Push to col 4 (last column on a 5-wide viewport).
        for _ in 0..4 {
            s.predict_key(&key_text("x"));
        }
        assert_eq!(s.cursor_col, 4);
        // 5th press would land at col 4 (the last); refuse.
        assert_eq!(s.predict_key(&key_text("x")), PredictionOutcome::Skipped);
    }

    #[test]
    fn set_viewport_clears_predictions() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.predict_key(&key_text("a"));
        s.predict_key(&key_text("b"));
        assert_eq!(s.pending_len(), 2);
        s.set_viewport(100, 30);
        assert_eq!(s.pending_len(), 0);
        assert_eq!(s.cols, 100);
        assert_eq!(s.rows, 30);
    }

    #[test]
    fn set_cursor_clamps_to_viewport() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(50, 200);
        assert_eq!(s.cursor_row, 23);
        assert_eq!(s.cursor_col, 79);
    }

    #[test]
    fn shift_modifier_is_fine_for_uppercase() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let mut ev = key_text("A");
        ev.mods = ModSet::SHIFT;
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Predicted);
    }

    #[test]
    fn single_scalar_latin1_is_predicted_width_one() {
        // U+00E9 — single Unicode scalar, cell width 1 per
        // `unicode-width`. phux-9gw.1.4: predict it as a single-cell insert.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let ev = key_text("é");
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Predicted);
        let p = s.pending().next().expect("one prediction");
        assert_eq!(p.text, "é");
        assert_eq!(p.width, 1);
        assert_eq!(s.cursor(), (0, 1));
    }

    #[test]
    fn cjk_codepoint_is_predicted_width_two() {
        // U+4E2D 中 — CJK Unified Ideograph, cell width 2.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let ev = key_text("中");
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Predicted);
        let p = s.pending().next().expect("one prediction");
        assert_eq!(p.text, "中");
        assert_eq!(p.width, 2);
        // Width-2 grapheme advances the cursor by two columns.
        assert_eq!(s.cursor(), (0, 2));
    }

    #[test]
    fn cjk_at_right_edge_minus_one_is_not_predicted() {
        // Width-2 needs two free cells. Park the predict cursor one
        // before the last column on a 10-wide viewport: cols 0..9, last
        // col is 9; cursor at 8 → end_col would be 10, which is past
        // the rightmost column we permit (insert refuses at `>= cols`).
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 10, 24);
        s.set_cursor(0, 8);
        let ev = key_text("中");
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
    }

    #[test]
    fn combining_mark_alone_is_not_predicted() {
        // U+0301 COMBINING ACUTE ACCENT — a one-cluster payload of width
        // 0, no base to anchor on. Reject: there is nothing to paint.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let ev = key_text("\u{0301}");
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
    }

    #[test]
    fn base_plus_combining_mark_is_predicted_width_one() {
        // "e\u{0301}" — base 'e' plus COMBINING ACUTE ACCENT, one
        // grapheme cluster of cell width 1 (phux-9gw.1.6). Predict it as
        // a single-cell insert carrying the full two-scalar cluster.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let ev = key_text("e\u{0301}");
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Predicted);
        let p = s.pending().next().expect("one prediction");
        assert_eq!(p.text, "e\u{0301}");
        assert_eq!(p.width, 1);
        assert_eq!(s.cursor(), (0, 1));
    }

    #[test]
    fn flag_emoji_is_predicted_width_two() {
        // 🇺🇸 — U+1F1FA REGIONAL INDICATOR U + U+1F1F8 REGIONAL
        // INDICATOR S, one grapheme cluster of cell width 2
        // (phux-9gw.1.6). Two scalars, single visual cell pair.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let ev = key_text("\u{1F1FA}\u{1F1F8}");
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Predicted);
        let p = s.pending().next().expect("one prediction");
        assert_eq!(p.text, "\u{1F1FA}\u{1F1F8}");
        assert_eq!(p.width, 2);
        assert_eq!(s.cursor(), (0, 2));
    }

    #[test]
    fn zwj_family_emoji_is_predicted_width_two() {
        // 👨‍👩‍👧 — man + ZWJ + woman + ZWJ + girl, one grapheme cluster
        // rendered as a single width-2 emoji cell (phux-9gw.1.6).
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let cluster = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let ev = key_text(cluster);
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Predicted);
        let p = s.pending().next().expect("one prediction");
        assert_eq!(p.text, cluster);
        assert_eq!(p.width, 2);
        assert_eq!(s.cursor(), (0, 2));
    }

    #[test]
    fn two_grapheme_clusters_in_text_are_not_predicted() {
        // A paste-like `text` payload carrying two distinct clusters
        // ("ab") is outside the single-keystroke model — refuse.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let ev = key_text("ab");
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn arrow_left_over_known_cell_advances_predict_cursor() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 5);
        // Cell at (0, 4) is 'a' (width 1).
        let ev = key_named(PhysicalKey::ArrowLeft, ModSet::empty());
        let outcome =
            s.predict_key_with_grid(&ev, |r, c| if (r, c) == (0, 4) { Some('a') } else { None });
        assert_eq!(outcome, PredictionOutcome::Predicted);
        assert_eq!(s.cursor(), (0, 4));
        let p = s.pending().next().expect("one prediction");
        assert_eq!(p.kind, PredictionKind::CursorLeft);
        assert_eq!(p.col, 4);
        assert_eq!(p.row, 0);
    }

    #[test]
    fn arrow_left_over_blank_cell_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 5);
        let ev = key_named(PhysicalKey::ArrowLeft, ModSet::empty());
        let outcome = s.predict_key_with_grid(&ev, |_, _| None);
        assert_eq!(outcome, PredictionOutcome::Skipped);
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn arrow_left_at_col_zero_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let ev = key_named(PhysicalKey::ArrowLeft, ModSet::empty());
        let outcome = s.predict_key_with_grid(&ev, |_, _| Some('a'));
        assert_eq!(outcome, PredictionOutcome::Skipped);
    }

    #[test]
    fn arrow_right_over_known_cell_advances_predict_cursor() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 3);
        let ev = key_named(PhysicalKey::ArrowRight, ModSet::empty());
        let outcome =
            s.predict_key_with_grid(&ev, |r, c| if (r, c) == (0, 3) { Some('x') } else { None });
        assert_eq!(outcome, PredictionOutcome::Predicted);
        assert_eq!(s.cursor(), (0, 4));
        let p = s.pending().next().expect("one prediction");
        assert_eq!(p.kind, PredictionKind::CursorRight);
        assert_eq!(p.col, 4);
    }

    #[test]
    fn arrow_right_over_blank_cell_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 3);
        let ev = key_named(PhysicalKey::ArrowRight, ModSet::empty());
        let outcome = s.predict_key_with_grid(&ev, |_, _| None);
        assert_eq!(outcome, PredictionOutcome::Skipped);
    }

    #[test]
    fn arrow_right_at_right_edge_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 10, 24);
        s.set_cursor(0, 9);
        let ev = key_named(PhysicalKey::ArrowRight, ModSet::empty());
        // Cursor at col 9 (== cols-1) with cells 0..9 valid; advancing
        // by 1 would land at 10 (past the right edge) — refuse.
        let outcome = s.predict_key_with_grid(&ev, |_, _| Some('x'));
        assert_eq!(outcome, PredictionOutcome::Skipped);
    }

    #[test]
    fn arrow_right_over_wide_at_right_edge_minus_one_is_not_predicted() {
        // Width-2 advance needs two cells to the right. Cursor at col 7
        // with cols=10: end_col would be 9 (allowed for plain insert
        // but the wide-tail would land at col 8 — that's still within
        // bounds). The real refusal case is cursor at col 8: end_col
        // would be 10 (past the right edge for a width-2 grapheme).
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 10, 24);
        s.set_cursor(0, 8);
        let ev = key_named(PhysicalKey::ArrowRight, ModSet::empty());
        let outcome = s.predict_key_with_grid(&ev, |_, _| Some('中'));
        assert_eq!(outcome, PredictionOutcome::Skipped);
    }

    #[test]
    fn arrow_right_over_wide_grapheme_advances_by_two() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 3);
        let ev = key_named(PhysicalKey::ArrowRight, ModSet::empty());
        let outcome =
            s.predict_key_with_grid(&ev, |r, c| if (r, c) == (0, 3) { Some('中') } else { None });
        assert_eq!(outcome, PredictionOutcome::Predicted);
        assert_eq!(s.cursor(), (0, 5));
        let p = s.pending().next().expect("one prediction");
        assert_eq!(p.col, 5);
    }

    #[test]
    fn arrow_via_predict_key_default_grid_is_skipped() {
        // The plain `predict_key` entry point routes arrows through a
        // grid closure that always returns `None` — arrows are only
        // useful when the driver passes the live grid via
        // `predict_key_with_grid`. Verify the conservative default
        // even when the cursor is past col 0.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 5);
        let ev = key_named(PhysicalKey::ArrowLeft, ModSet::empty());
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
        let ev = key_named(PhysicalKey::ArrowRight, ModSet::empty());
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
    }

    #[test]
    fn empty_text_is_not_predicted() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let mut ev = key_text("a");
        ev.text = None;
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
    }

    #[test]
    fn del_byte_is_not_predicted_as_printable() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let ev = key_text("\x7f");
        assert_eq!(s.predict_key(&ev), PredictionOutcome::Skipped);
    }

    // ---- phux-pxaj: adaptive auto-back-off ----------------------------

    fn contradicting_pass() -> ReconcileStats {
        ReconcileStats {
            confirmed: 0,
            contradicted: 1,
            pending: 0,
        }
    }

    fn clean_productive_pass() -> ReconcileStats {
        ReconcileStats {
            confirmed: 1,
            contradicted: 0,
            pending: 0,
        }
    }

    fn neutral_pass() -> ReconcileStats {
        ReconcileStats {
            confirmed: 0,
            contradicted: 0,
            pending: 2,
        }
    }

    #[test]
    fn three_contradictions_in_a_row_auto_back_off() {
        // The mispredict-storm signal: three consecutive contradicting
        // reconcile passes (vi normal-mode, a modal app, fast transitions)
        // trip the back-off. Two are not enough.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.note_reconcile(contradicting_pass());
        s.note_reconcile(contradicting_pass());
        assert!(!s.is_suspended(), "two contradictions: not yet backed off");
        assert!(!s.is_auto_backed_off());
        s.note_reconcile(contradicting_pass());
        assert!(s.is_suspended(), "third contradiction trips the back-off");
        assert!(s.is_auto_backed_off());
        // While backed off, predict_key is a no-op.
        assert_eq!(s.predict_key(&key_text("a")), PredictionOutcome::Skipped);
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn auto_back_off_drops_pending_queue() {
        // The back-off must drop in-flight predictions, like the re-anchor
        // suspend — they are exactly the ghosts the server is contradicting.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.predict_key(&key_text("h"));
        s.predict_key(&key_text("i"));
        assert_eq!(s.pending_len(), 2);
        for _ in 0..BACKOFF_THRESHOLD {
            s.note_reconcile(contradicting_pass());
        }
        assert!(s.is_auto_backed_off());
        assert_eq!(s.pending_len(), 0, "back-off drops the queue");
    }

    #[test]
    fn back_off_survives_set_cursor() {
        // The re-anchor suspend is cleared by the next authoritative cursor
        // sync; an auto-back-off must NOT be — otherwise a single reconcile
        // would re-arm straight back into the mispredict storm.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for _ in 0..BACKOFF_THRESHOLD {
            s.note_reconcile(contradicting_pass());
        }
        assert!(s.is_suspended());
        assert!(s.is_auto_backed_off());
        s.set_cursor(2, 4);
        assert!(s.is_suspended(), "back-off survives a cursor sync");
        assert!(s.is_auto_backed_off());
        assert_eq!(s.predict_key(&key_text("a")), PredictionOutcome::Skipped);
    }

    #[test]
    fn two_clean_confirms_re_arm_after_back_off() {
        // Once typing normalizes — two consecutive clean productive passes —
        // the back-off lifts and prediction resumes.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for _ in 0..BACKOFF_THRESHOLD {
            s.note_reconcile(contradicting_pass());
        }
        assert!(s.is_auto_backed_off());
        s.note_reconcile(clean_productive_pass());
        assert!(
            s.is_suspended(),
            "one clean confirm is not enough to re-arm"
        );
        assert!(s.is_auto_backed_off());
        s.note_reconcile(clean_productive_pass());
        assert!(!s.is_suspended(), "two clean confirms re-arm");
        assert!(!s.is_auto_backed_off());
        // A cursor sync provides an anchor, then prediction works again.
        s.set_cursor(0, 0);
        assert_eq!(s.predict_key(&key_text("a")), PredictionOutcome::Predicted);
    }

    #[test]
    fn a_contradiction_resets_the_clean_confirm_streak() {
        // A fluke clean confirm mid-storm must not count toward re-arm: a
        // following contradiction resets the clean streak, so it takes two
        // *consecutive* clean passes to lift.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for _ in 0..BACKOFF_THRESHOLD {
            s.note_reconcile(contradicting_pass());
        }
        s.note_reconcile(clean_productive_pass()); // clean streak = 1
        s.note_reconcile(contradicting_pass()); // resets clean streak
        s.note_reconcile(clean_productive_pass()); // clean streak = 1 again
        assert!(s.is_suspended(), "still backed off: not two in a row");
        s.note_reconcile(clean_productive_pass()); // clean streak = 2
        assert!(!s.is_suspended(), "now two in a row → re-armed");
    }

    #[test]
    fn empty_passes_do_not_move_streaks() {
        // Neutral passes (only pendings, or an empty queue) must not trip
        // the back-off nor lift it. Two contradictions then a run of neutral
        // passes then a third contradiction still trips: the contradiction
        // streak is preserved across the neutral passes.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.note_reconcile(contradicting_pass());
        s.note_reconcile(contradicting_pass());
        for _ in 0..5 {
            s.note_reconcile(neutral_pass());
        }
        assert!(!s.is_suspended(), "neutral passes do not trip back-off");
        s.note_reconcile(contradicting_pass());
        assert!(
            s.is_suspended(),
            "contradiction streak survived neutral passes"
        );
    }

    #[test]
    fn neutral_passes_do_not_re_arm_during_back_off() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for _ in 0..BACKOFF_THRESHOLD {
            s.note_reconcile(contradicting_pass());
        }
        s.note_reconcile(clean_productive_pass()); // clean streak = 1
        for _ in 0..5 {
            s.note_reconcile(neutral_pass());
        }
        // Neutral passes left the clean streak at 1; one more clean confirm
        // reaches the threshold of 2 and re-arms.
        assert!(s.is_suspended(), "neutral passes do not advance re-arm");
        s.note_reconcile(clean_productive_pass());
        assert!(!s.is_suspended());
    }

    #[test]
    fn intermittent_contradictions_do_not_trip_back_off() {
        // A single contradiction broken up by clean confirms is the normal
        // wrong-guess-then-correct rhythm — it must not back off.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for _ in 0..6 {
            s.note_reconcile(contradicting_pass());
            s.note_reconcile(clean_productive_pass());
        }
        assert!(
            !s.is_suspended(),
            "isolated contradictions never accumulate"
        );
        assert!(!s.is_auto_backed_off());
    }

    #[test]
    fn reanchor_suspend_is_independent_of_auto_back_off() {
        // The re-anchor suspend() leaves auto_backed_off untouched, so a
        // single set_cursor re-arms it (the phux-cdvr behaviour), unlike a
        // back-off.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.suspend();
        assert!(s.is_suspended());
        assert!(
            !s.is_auto_backed_off(),
            "re-anchor suspend is not a back-off"
        );
        s.set_cursor(1, 1);
        assert!(!s.is_suspended(), "re-anchor suspend clears on cursor sync");
    }

    #[test]
    fn confirmed_with_contradiction_in_same_pass_counts_as_contradiction() {
        // A pass that confirmed a prefix but contradicted the suffix means
        // we still guessed wrong — it counts toward back-off, not re-arm.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let mixed = ReconcileStats {
            confirmed: 2,
            contradicted: 1,
            pending: 0,
        };
        s.note_reconcile(mixed);
        s.note_reconcile(mixed);
        assert!(!s.is_suspended());
        s.note_reconcile(mixed);
        assert!(
            s.is_suspended(),
            "contradicted>0 trips regardless of confirms"
        );
    }
}
