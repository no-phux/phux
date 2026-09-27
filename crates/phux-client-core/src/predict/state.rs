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

/// Per-client knob for predictive echo (off by default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PredictiveConfig {
    /// Whether to apply predictive local echo at all.
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

/// One in-flight prediction: a visual edit guessed from a keystroke sent
/// upstream but not yet confirmed by server output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prediction {
    /// Row of the cell, 0-indexed from the top of the viewport.
    pub row: u16,
    /// Column of the cell, 0-indexed; for cursor motions, the target
    /// column after the motion.
    pub col: u16,
    /// The grapheme cluster the cell should display; `" "` for backspace
    /// and a placeholder space for cursor motions (which paint nothing).
    pub text: String,
    /// Cell width of [`Self::text`]; `0` when no cell is painted.
    pub width: u8,
    /// Kind of prediction; selects the reconcile rule.
    pub kind: PredictionKind,
    /// Caller-supplied monotonic milliseconds when queued (`0` from the
    /// timeless entry points); feeds [`PredictionState::should_display`].
    pub queued_at_ms: u64,
}

/// What the prediction modelled; see the `reconcile` module's match table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredictionKind {
    /// A printable grapheme cluster.
    Insert,
    /// A backspace at end-of-line: cursor back one column, blank the cell.
    BackspaceEol,
    /// Enter after typing on the current row: cursor to `(row+1, 0)`.
    Newline,
    /// Left arrow over a known cell on the current line.
    CursorLeft,
    /// Right arrow over a known cell on the current line.
    CursorRight,
}

/// What [`PredictionState::predict_key`] decided about a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredictionOutcome {
    /// The key was predicted; the new prediction is at `queue.back()`.
    Predicted,
    /// The key is outside the safe set; it still travels upstream.
    Skipped,
    /// Predictive echo is disabled by config.
    Disabled,
}

/// The client-side prediction state: [`Self::predict_key`] enqueues on the
/// keystroke path; [`super::reconcile_terminal_output_per_cell`] confirms
/// or drops on the server-output path.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "four independent latches")]
pub struct PredictionState {
    cfg: PredictiveConfig,
    /// FIFO of pending predictions, in issue order.
    pending: VecDeque<Prediction>,
    /// Estimated server cursor after every pending prediction applies.
    cursor_row: u16,
    cursor_col: u16,
    /// Viewport size in cells; the last column is never predicted (wrap).
    cols: u16,
    rows: u16,
    /// Where the user's typed input began on this row (the first insert's
    /// column); erasure never goes below it. `None` = unknown: a single
    /// backspace is still predicted, Ctrl-U is refused.
    prompt_boundary: Option<(u16, u16)>,
    /// Predicting is a no-op until [`Self::set_cursor`] re-arms it.
    suspended: bool,
    /// Consecutive contradicting passes; survives clear/suspend.
    contradiction_streak: u32,
    /// Consecutive clean passes since turning tentative; survives
    /// clear/suspend.
    clean_confirm_streak: u32,
    /// Display lock after a contradiction run (ADR-0090). Predicting
    /// continues so a confirm can lift it.
    tentative: bool,
    /// On the alternate screen: display is confirmation-gated and Enter
    /// suspends (ADR-0090).
    alt_screen: bool,
    /// A non-blank insert confirmed since the last screen switch,
    /// contradiction or resync; unlocks alt-screen display.
    echo_confirmed: bool,
    /// Smoothed queue-to-confirm time for non-blank insert predictions, in
    /// milliseconds. This is a link property, so screen switches, clears,
    /// resizes, and contradictions deliberately preserve it.
    srtt_ms: Option<u64>,
}

/// Consecutive contradicting passes that turn the state tentative.
const BACKOFF_THRESHOLD: u32 = 3;

/// Consecutive clean passes that lift the tentative lock.
const REARM_THRESHOLD: u32 = 2;

/// Floor on how long an unconfirmed front prediction stays visible.
const DISPLAY_TTL_MS: u64 = 1_000;

/// Maximum SRTT-derived display lifetime. This bounds a stale or sensitive
/// prediction even on a pathological link.
const DISPLAY_TTL_CAP_MS: u64 = 5_000;

/// RFC 6298's smoothing gain denominator: alpha is one eighth (`0.125`).
const SRTT_GAIN_DENOMINATOR: u64 = 8;

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
            tentative: false,
            alt_screen: false,
            echo_confirmed: false,
            srtt_ms: None,
        }
    }

    /// Drop the queue and suspend prediction until the next
    /// [`Self::set_cursor`]; used when re-anchoring to a pane whose cursor
    /// is unknown, so no ghost is echoed at `(0, 0)`.
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

    /// Update the viewport. Drops the queue, the prompt anchor and the echo
    /// evidence, all anchored to the old geometry.
    pub fn set_viewport(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.pending.clear();
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.prompt_boundary = None;
        self.echo_confirmed = false;
    }

    /// Record which screen is active; a transition drops the queue, the
    /// prompt anchor and the echo evidence.
    pub fn set_alt_screen(&mut self, alt: bool) {
        if alt == self.alt_screen {
            return;
        }
        self.alt_screen = alt;
        self.pending.clear();
        self.prompt_boundary = None;
        self.echo_confirmed = false;
    }

    /// Re-anchor the cursor estimate from authoritative state and re-arm a
    /// suspended predictor. The tentative lock is untouched: it lifts only
    /// through [`Self::note_reconcile`].
    pub fn set_cursor(&mut self, row: u16, col: u16) {
        let new_row = row.min(self.rows.saturating_sub(1));
        // The prompt anchor survives a same-row resync; a row change is a
        // new input context.
        if self
            .prompt_boundary
            .is_some_and(|(brow, _)| brow != new_row)
        {
            self.prompt_boundary = None;
        }
        self.cursor_row = new_row;
        self.cursor_col = col.min(self.cols.saturating_sub(1));
        self.suspended = false;
    }

    /// Feed one reconcile pass into the tentative-display heuristic
    /// (ADR-0090): three consecutive passes with any
    /// contradiction turn the state tentative; two
    /// consecutive clean passes (confirms, no contradiction) lift it.
    /// Pending-only passes move neither streak.
    pub fn note_reconcile(&mut self, stats: ReconcileStats) {
        if stats.contradicted > 0 {
            self.contradiction_streak = self.contradiction_streak.saturating_add(1);
            self.clean_confirm_streak = 0;
            if self.contradiction_streak >= BACKOFF_THRESHOLD {
                self.enter_tentative();
            }
        } else if stats.confirmed > 0 {
            // Clean productive pass.
            self.contradiction_streak = 0;
            self.clean_confirm_streak = self.clean_confirm_streak.saturating_add(1);
            if self.tentative && self.clean_confirm_streak >= REARM_THRESHOLD {
                // Typing normalized — lift the lock and display again.
                self.tentative = false;
                self.clean_confirm_streak = 0;
            }
        }
    }

    /// Drop queued ghosts and hide the overlay; predicting continues.
    fn enter_tentative(&mut self) {
        self.pending.clear();
        self.prompt_boundary = None;
        self.tentative = true;
        // Start counting clean passes afresh for the re-arm decision.
        self.clean_confirm_streak = 0;
    }

    /// Whether the tentative display lock is on.
    #[must_use]
    pub const fn is_tentative(&self) -> bool {
        self.tentative
    }

    /// Number of predictions waiting for confirmation.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Current cursor estimate `(row, col)`.
    #[must_use]
    pub const fn cursor(&self) -> (u16, u16) {
        (self.cursor_row, self.cursor_col)
    }

    /// The whole pending queue; renderers use [`Self::displayable`].
    pub fn pending(&self) -> impl Iterator<Item = &Prediction> {
        self.pending.iter()
    }

    /// The predictions the renderer may paint now: all of them when
    /// [`Self::should_display`], else none.
    pub fn displayable(&self, now_ms: u64) -> impl Iterator<Item = &Prediction> {
        let show = self.should_display(now_ms);
        self.pending.iter().filter(move |_| show)
    }

    /// Whether the overlay should be presented (ADR-0090): not while
    /// tentative, not on the alternate screen without echo evidence, and
    /// not once the front guess outlives [`Self::display_ttl_ms`].
    #[must_use]
    pub fn should_display(&self, now_ms: u64) -> bool {
        let Some(front) = self.pending.front() else {
            return false;
        };
        if self.tentative {
            return false;
        }
        if self.alt_screen && !self.echo_confirmed {
            return false;
        }
        now_ms.saturating_sub(front.queued_at_ms) <= self.display_ttl_ms()
    }

    /// Display lifetime derived from twice the smoothed echo RTT, clamped to
    /// the 1-second safety floor and 5-second ghost bound.
    #[must_use]
    pub fn display_ttl_ms(&self) -> u64 {
        self.srtt_ms.map_or(DISPLAY_TTL_MS, |srtt| {
            srtt.saturating_mul(2)
                .clamp(DISPLAY_TTL_MS, DISPLAY_TTL_CAP_MS)
        })
    }

    /// Whether the app has proven it echoes (ADR-0090).
    #[must_use]
    pub const fn echo_confirmed(&self) -> bool {
        self.echo_confirmed
    }

    /// Record echo evidence: a non-blank insert was confirmed.
    pub(crate) const fn confirm_echo(&mut self) {
        self.echo_confirmed = true;
    }

    /// Record one non-blank insert confirmation at a caller-supplied
    /// monotonic time. Missing clocks (`queued_at_ms == 0`) and clocks that
    /// moved backwards are ignored.
    pub(crate) fn confirm_echo_at(&mut self, queued_at_ms: u64, now_ms: u64) {
        self.confirm_echo();
        if queued_at_ms == 0 || now_ms < queued_at_ms {
            return;
        }
        let sample = now_ms - queued_at_ms;
        self.srtt_ms = Some(self.srtt_ms.map_or(sample, |srtt| {
            if sample >= srtt {
                srtt + (sample - srtt) / SRTT_GAIN_DENOMINATOR
            } else {
                srtt - (srtt - sample) / SRTT_GAIN_DENOMINATOR
            }
        }));
    }

    /// Drop every pending prediction, the prompt anchor derived from them,
    /// and the echo evidence (callers also clear on resync).
    pub fn clear(&mut self) {
        self.pending.clear();
        self.prompt_boundary = None;
        self.echo_confirmed = false;
    }

    /// Current prompt-boundary anchor `(row, col)`, if known.
    #[must_use]
    pub const fn prompt_boundary(&self) -> Option<(u16, u16)> {
        self.prompt_boundary
    }

    /// Drop the prediction at the front of the queue.
    pub(crate) fn pop_front(&mut self) -> Option<Prediction> {
        self.pending.pop_front()
    }

    /// Peek at the prediction at the front of the queue.
    pub(crate) fn front(&self) -> Option<&Prediction> {
        self.pending.front()
    }

    /// Try to predict the visual effect of `event`.
    ///
    /// Predicted: exactly one grapheme cluster of width 1 or 2 with no
    /// Ctrl/Alt/Super (Shift is fine); Backspace past column 0; Enter past
    /// column 0 above the last row; bare Ctrl-U with a known prompt
    /// boundary. Arrows need the grid ([`Self::predict_key_with_grid`]) and
    /// are skipped here. Timeless (`now_ms = 0`).
    pub fn predict_key(&mut self, event: &KeyEvent) -> PredictionOutcome {
        self.predict_key_with_grid_at(event, 0, |_, _| None)
    }

    /// [`Self::predict_key`] with a caller-supplied monotonic clock.
    pub fn predict_key_at(&mut self, event: &KeyEvent, now_ms: u64) -> PredictionOutcome {
        self.predict_key_with_grid_at(event, now_ms, |_, _| None)
    }

    /// [`Self::predict_key`] with a read closure into the authoritative
    /// grid, consulted only for the width of the glyph an arrow steps over.
    pub fn predict_key_with_grid<F>(&mut self, event: &KeyEvent, read_cell: F) -> PredictionOutcome
    where
        F: FnMut(u16, u16) -> Option<char>,
    {
        self.predict_key_with_grid_at(event, 0, read_cell)
    }

    /// [`Self::predict_key_with_grid`] with a caller-supplied monotonic
    /// clock.
    pub fn predict_key_with_grid_at<F>(
        &mut self,
        event: &KeyEvent,
        now_ms: u64,
        mut read_cell: F,
    ) -> PredictionOutcome
    where
        F: FnMut(u16, u16) -> Option<char>,
    {
        if let Some(outcome) = self.reject_unpredictable_context(event) {
            return outcome;
        }
        // Ctrl-U is the one CTRL chord predicted, so it precedes the
        // modifier reject.
        if event.key == PhysicalKey::U && event.mods == ModSet::CTRL {
            return self.predict_kill_to_boundary(now_ms);
        }
        // SHIFT is already baked into `text`.
        let blocking_mods = ModSet::CTRL | ModSet::ALT | ModSet::SUPER;
        if event.mods.intersects(blocking_mods) {
            return PredictionOutcome::Skipped;
        }

        if event.key == PhysicalKey::Backspace {
            return self.predict_backspace_eol(now_ms);
        }
        if event.key == PhysicalKey::Enter {
            return self.predict_enter_unless_alt_screen(now_ms);
        }
        if event.key == PhysicalKey::ArrowLeft {
            return self.predict_arrow_left(&mut read_cell, now_ms);
        }
        if event.key == PhysicalKey::ArrowRight {
            return self.predict_arrow_right(&mut read_cell, now_ms);
        }
        self.predict_printable_cluster(event, now_ms)
    }

    /// `Some(outcome)` when no key can be predicted in this context.
    fn reject_unpredictable_context(&mut self, event: &KeyEvent) -> Option<PredictionOutcome> {
        if !self.cfg.enabled {
            return Some(PredictionOutcome::Disabled);
        }

        if !matches!(event.action, phux_protocol::input::key::KeyAction::Press) {
            return Some(PredictionOutcome::Skipped);
        }
        // Alt-screen mode-changing input (Esc, chords, arrows) kills the
        // echo evidence and the burst, even while suspended (ADR-0090).
        if self.alt_screen && is_mode_changing_input(event) {
            self.suspend();
            self.echo_confirmed = false;
            return Some(PredictionOutcome::Skipped);
        }

        if self.suspended {
            return Some(PredictionOutcome::Skipped);
        }
        None
    }

    /// On the alternate screen Enter submits rather than feeds a line, so
    /// suspend instead; the echo latch survives (ADR-0090).
    fn predict_enter_unless_alt_screen(&mut self, now_ms: u64) -> PredictionOutcome {
        if self.alt_screen {
            self.suspend();
            return PredictionOutcome::Skipped;
        }
        self.predict_enter(now_ms)
    }

    /// Printable insert; a payload of more than one grapheme cluster is
    /// paste-like and skipped.
    fn predict_printable_cluster(&mut self, event: &KeyEvent, now_ms: u64) -> PredictionOutcome {
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
        self.predict_insert(cluster, width, now_ms)
    }

    /// Predict Enter as a jump to `(row+1, 0)`; skipped at column 0 and on
    /// the last row (scroll is not modelled).
    fn predict_enter(&mut self, now_ms: u64) -> PredictionOutcome {
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
            queued_at_ms: now_ms,
        });
        // A new line is a new input context.
        self.cursor_row = self.cursor_row.saturating_add(1);
        self.cursor_col = 0;
        self.prompt_boundary = None;
        PredictionOutcome::Predicted
    }

    fn predict_insert(&mut self, cluster: &str, width: u8, now_ms: u64) -> PredictionOutcome {
        if self.cols == 0 || self.rows == 0 || self.cursor_row >= self.rows {
            return PredictionOutcome::Skipped;
        }
        // Never predict at the rightmost column: wrap behavior is unknown.
        let advance = u16::from(width);
        if advance == 0 {
            return PredictionOutcome::Skipped;
        }
        let end_col = self.cursor_col.saturating_add(advance);
        if end_col >= self.cols {
            return PredictionOutcome::Skipped;
        }
        // The first insert on a row marks where typed input begins.
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
            queued_at_ms: now_ms,
        });
        self.cursor_col = end_col;
        PredictionOutcome::Predicted
    }

    fn predict_backspace_eol(&mut self, now_ms: u64) -> PredictionOutcome {
        // Backspace at column 0 may wrap or no-op depending on the shell.
        if self.cursor_col == 0 {
            return PredictionOutcome::Skipped;
        }
        if self.rows == 0 || self.cursor_row >= self.rows {
            return PredictionOutcome::Skipped;
        }
        // Never erase at or below a known prompt boundary.
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
            queued_at_ms: now_ms,
        });
        self.cursor_col = new_col;
        PredictionOutcome::Predicted
    }

    /// Predict Ctrl-U as erasing the typed run from the cursor down to the
    /// known prompt boundary; refused when the boundary is unknown or
    /// nothing was typed. Reconcile drops any cell the server disagrees with.
    fn predict_kill_to_boundary(&mut self, now_ms: u64) -> PredictionOutcome {
        if self.rows == 0 || self.cursor_row >= self.rows {
            return PredictionOutcome::Skipped;
        }
        let Some((brow, bcol)) = self.prompt_boundary else {
            return PredictionOutcome::Skipped;
        };
        if brow != self.cursor_row || self.cursor_col <= bcol {
            return PredictionOutcome::Skipped;
        }

        let mut col = self.cursor_col;
        while col > bcol {
            col -= 1;
            self.pending.push_back(Prediction {
                row: self.cursor_row,
                col,
                text: " ".to_owned(),
                width: 1,
                kind: PredictionKind::BackspaceEol,
                queued_at_ms: now_ms,
            });
        }
        self.cursor_col = bcol;
        PredictionOutcome::Predicted
    }

    /// Predict a left arrow over a known glyph by its width; skipped over
    /// a blank cell (no anchor) or at column 0.
    fn predict_arrow_left<F>(&mut self, read_cell: &mut F, now_ms: u64) -> PredictionOutcome
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
        // A wide glyph's tail is at `probe_col`; land on its base.
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
            queued_at_ms: now_ms,
        });
        self.cursor_col = new_col;
        PredictionOutcome::Predicted
    }

    /// Predict a right arrow over a known glyph by its width; skipped over
    /// a blank cell or when landing at or past the rightmost column.
    fn predict_arrow_right<F>(&mut self, read_cell: &mut F, now_ms: u64) -> PredictionOutcome
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

        if new_col >= self.cols {
            return PredictionOutcome::Skipped;
        }
        self.pending.push_back(Prediction {
            row: self.cursor_row,
            col: new_col,
            text: " ".to_owned(),
            width: 0,
            kind: PredictionKind::CursorRight,
            queued_at_ms: now_ms,
        });
        self.cursor_col = new_col;
        PredictionOutcome::Predicted
    }
}

/// Whether an alternate-screen key could move the app out of the echoing
/// state (ADR-0090): Esc, modifier chords, and named keys. Backspace and
/// Enter have their own policies; printable text never is.
fn is_mode_changing_input(event: &KeyEvent) -> bool {
    if matches!(event.key, PhysicalKey::Backspace | PhysicalKey::Enter) {
        return false;
    }
    if event
        .mods
        .intersects(ModSet::CTRL | ModSet::ALT | ModSet::SUPER)
    {
        return true;
    }
    match event.text.as_deref() {
        None | Some("") => true,
        Some(text) => text.chars().next().is_some_and(char::is_control),
    }
}

/// ASCII control codes and DEL may or may not echo depending on `stty`.
const fn is_safe_predictable(ch: char) -> bool {
    let c = ch as u32;
    c >= 0x20 && c != 0x7F
}

/// Cell width (1 or 2) of the single scalar an arrow steps over; `None`
/// for non-printable or zero-width scalars.
fn grapheme_width(ch: char) -> Option<u8> {
    let w = UnicodeWidthChar::width(ch)?;
    if w == 0 {
        return None;
    }
    Some(u8::try_from(w.min(2)).unwrap_or(1))
}

/// Whether a cluster's first scalar is safe to predict.
fn is_safe_predictable_cluster(cluster: &str) -> bool {
    cluster.chars().next().is_some_and(is_safe_predictable)
}

/// Display width of a whole grapheme cluster, capped at 2 (the terminal
/// renders wider clusters as one wide cell); `None` when zero.
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
    fn suspended_predict_key_is_a_no_op_until_set_cursor_rearms() {
        // Split-ghost guard: a keystroke before the new pane's cursor is
        // known must not echo at (0, 0).
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        assert_eq!(s.predict_key(&key_text("a")), PredictionOutcome::Predicted);
        s.suspend();
        assert_eq!(s.pending_len(), 0, "suspend clears the queue");
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
    fn multiple_keystrokes_stack_horizontally() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for ch in ["h", "e", "l", "l", "o"] {
            assert_eq!(s.predict_key(&key_text(ch)), PredictionOutcome::Predicted);
        }
        let cells: Vec<_> = s.pending().map(|p| (p.row, p.col, p.kind)).collect();
        let want: Vec<_> = (0..5).map(|c| (0, c, PredictionKind::Insert)).collect();
        assert_eq!(cells, want);
        assert_eq!(s.cursor_col, 5);
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

    // ---- prompt-boundary heuristic + Ctrl-U -------------

    fn key_ctrl_u() -> KeyEvent {
        let mut ev = key_named(PhysicalKey::U, ModSet::CTRL);
        ev.text = None;
        ev
    }

    fn with(mut ev: KeyEvent, edit: impl FnOnce(&mut KeyEvent)) -> KeyEvent {
        edit(&mut ev);
        ev
    }

    /// Type `prior` from `start`, then assert `key` is skipped and leaves the
    /// queue and cursor untouched. `glyph` is what the grid reports.
    fn assert_skipped(
        name: &str,
        (cols, start): (u16, (u16, u16)),
        prior: &[KeyEvent],
        key: &KeyEvent,
        glyph: Option<char>,
    ) {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), cols, 24);
        s.set_cursor(start.0, start.1);
        for ev in prior {
            assert_eq!(s.predict_key(ev), PredictionOutcome::Predicted, "{name}");
        }
        let (pending, cursor) = (s.pending_len(), s.cursor());
        let outcome = s.predict_key_with_grid(key, |_, _| glyph);
        assert_eq!(outcome, PredictionOutcome::Skipped, "{name}");
        assert_eq!((s.pending_len(), s.cursor()), (pending, cursor), "{name}");
    }

    #[test]
    #[rustfmt::skip]
    fn unsafe_keys_are_skipped_without_touching_state() {
        let named = |k| key_named(k, ModSet::empty());
        let (a, bs) = (key_text("a"), named(PhysicalKey::Backspace));
        let at = |row, col| (80, (row, col));
        assert_skipped("ctrl", at(0, 0), &[], &with(a.clone(), |e| e.mods = ModSet::CTRL), None);
        assert_skipped("alt", at(0, 0), &[], &with(a.clone(), |e| e.mods = ModSet::ALT), None);
        assert_skipped("release", at(0, 0), &[], &with(a.clone(), |e| e.action = KeyAction::Release), None);
        assert_skipped("no text", at(0, 0), &[], &with(a.clone(), |e| e.text = None), None);
        assert_skipped("DEL byte", at(0, 0), &[], &key_text("\x7f"), None);
        assert_skipped("tab", at(0, 0), &[], &named(PhysicalKey::Tab), None);
        assert_skipped("enter at col 0", at(0, 0), &[], &named(PhysicalKey::Enter), None);
        assert_skipped("enter on last row", at(23, 5), &[], &named(PhysicalKey::Enter), None);
        assert_skipped("backspace at col 0", at(0, 0), &[], &bs, None);
        assert_skipped("ctrl-u, boundary unknown", at(0, 8), &[], &key_ctrl_u(), None);
        assert_skipped("ctrl-u, nothing typed", at(0, 5), &[a, bs], &key_ctrl_u(), None);
        let ctrl_alt_u = with(key_ctrl_u(), |e| e.mods = ModSet::CTRL | ModSet::ALT);
        assert_skipped("ctrl-alt-u", at(0, 4), &[key_text("h")], &ctrl_alt_u, None);
        assert_skipped("insert at last column", (5, (0, 4)), &[], &key_text("x"), None);
        assert_skipped("wide at edge-1", (10, (0, 8)), &[], &key_text("中"), None);
        assert_skipped("lone combining mark", at(0, 0), &[], &key_text("\u{0301}"), None);
        assert_skipped("two clusters", at(0, 0), &[], &key_text("ab"), None);
        assert_skipped("left over blank", at(0, 5), &[], &named(PhysicalKey::ArrowLeft), None);
        assert_skipped("left at col 0", at(0, 0), &[], &named(PhysicalKey::ArrowLeft), Some('a'));
        assert_skipped("right over blank", at(0, 3), &[], &named(PhysicalKey::ArrowRight), None);
        assert_skipped("right at edge", (10, (0, 9)), &[], &named(PhysicalKey::ArrowRight), Some('x'));
        assert_skipped("right over wide at edge-1", (10, (0, 8)), &[], &named(PhysicalKey::ArrowRight), Some('中'));
    }

    #[test]
    fn single_grapheme_inserts_carry_their_cell_width() {
        for (text, width) in [
            ("A", 1),
            ("é", 1),
            ("e\u{0301}", 1),
            ("中", 2),
            ("\u{1F1FA}\u{1F1F8}", 2),
            ("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", 2),
        ] {
            let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
            let ev = with(key_text(text), |e| e.mods = ModSet::SHIFT);
            assert_eq!(s.predict_key(&ev), PredictionOutcome::Predicted, "{text}");
            let p = s.pending().next().expect("one prediction");
            assert_eq!((p.text.as_str(), p.width), (text, width));
            assert_eq!(s.cursor(), (0, u16::from(width)), "{text}");
        }
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
    fn prompt_boundary_is_forgotten_on_context_changes() {
        type Change = fn(&mut PredictionState);
        let changes: [(&str, Change); 4] = [
            ("row change", |s| s.set_cursor(2, 0)),
            ("enter", |s| {
                let enter = key_named(PhysicalKey::Enter, ModSet::empty());
                assert_eq!(s.predict_key(&enter), PredictionOutcome::Predicted);
            }),
            ("resize", |s| s.set_viewport(100, 30)),
            ("clear", PredictionState::clear),
        ];
        for (name, change) in changes {
            let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
            s.set_cursor(0, 2);
            s.predict_key(&key_text("a"));
            assert_eq!(s.prompt_boundary(), Some((0, 2)), "{name}");
            change(&mut s);
            assert_eq!(s.prompt_boundary(), None, "{name}");
        }
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

    // ---- phux-pxaj (reshaped by ADR-0090): adaptive tentative display -

    fn contradicting_pass() -> ReconcileStats {
        ReconcileStats {
            confirmed: 0,
            contradicted: 1,
            pending: 0,
        }
    }

    /// Streak rules: three consecutive contradicting passes lock; two
    /// consecutive clean passes lift; neutral (pending-only) passes move
    /// neither streak; any contradiction in a pass counts as one.
    #[test]
    fn tentative_lock_follows_the_pass_streaks() {
        // x = contradicted, c = clean confirm, n = neutral, m = mixed.
        for (passes, tentative) in [
            ("xx", false),
            ("xxx", true),
            ("xxxc", true),
            ("xxxcc", false),
            ("xxxcxc", true),
            ("xxxcxcc", false),
            ("xxnnnnn", false),
            ("xxnnnnnx", true),
            ("xxxcnnnnn", true),
            ("xxxcnnnnnc", false),
            ("xcxcxcxcxcxc", false),
            ("mm", false),
            ("mmm", true),
        ] {
            let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
            for pass in passes.chars() {
                let (confirmed, contradicted, pending) = match pass {
                    'x' => (0, 1, 0),
                    'c' => (1, 0, 0),
                    'n' => (0, 0, 2),
                    _ => (2, 1, 0),
                };
                s.note_reconcile(ReconcileStats {
                    confirmed,
                    contradicted,
                    pending,
                });
            }
            assert_eq!(s.is_tentative(), tentative, "{passes}");
        }
    }

    #[test]
    fn three_contradictions_in_a_row_turn_tentative() {
        // The mispredict-storm signal: three consecutive contradicting
        // reconcile passes (vi normal-mode, a modal app, fast transitions)
        // hide the overlay. Two are not enough.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.note_reconcile(contradicting_pass());
        s.note_reconcile(contradicting_pass());
        assert!(!s.is_tentative(), "two contradictions: not yet tentative");
        s.note_reconcile(contradicting_pass());
        assert!(s.is_tentative(), "third contradiction trips the lock");
        assert!(
            !s.is_suspended(),
            "tentative is a display lock, not a predict suspend"
        );
        // Predicting continues (the re-arm signal is a confirmed
        // prediction) but nothing displays.
        assert_eq!(s.predict_key(&key_text("a")), PredictionOutcome::Predicted);
        assert_eq!(s.pending_len(), 1);
        assert!(!s.should_display(0), "tentative hides the overlay");
    }

    #[test]
    fn turning_tentative_drops_the_pending_queue() {
        // The lock must drop in-flight predictions — they are exactly the
        // ghosts the server is contradicting.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.predict_key(&key_text("h"));
        s.predict_key(&key_text("i"));
        assert_eq!(s.pending_len(), 2);
        for _ in 0..BACKOFF_THRESHOLD {
            s.note_reconcile(contradicting_pass());
        }
        assert!(s.is_tentative());
        assert_eq!(s.pending_len(), 0, "turning tentative drops the queue");
    }

    #[test]
    fn tentative_survives_set_cursor() {
        // The re-anchor suspend is cleared by the next authoritative cursor
        // sync; the tentative lock must NOT be — otherwise a single
        // reconcile would re-show the overlay straight back into the
        // mispredict storm.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for _ in 0..BACKOFF_THRESHOLD {
            s.note_reconcile(contradicting_pass());
        }
        assert!(s.is_tentative());
        s.set_cursor(2, 4);
        assert!(s.is_tentative(), "tentative survives a cursor sync");
        s.predict_key(&key_text("a"));
        assert!(!s.should_display(0), "still hidden after the sync");
    }

    // ---- ADR-0090: confirmation-gated alt-screen display --------------

    fn alt_state() -> PredictionState {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_alt_screen(true);
        s
    }

    fn key_esc() -> KeyEvent {
        key_named(PhysicalKey::Escape, ModSet::empty())
    }

    #[test]
    fn alt_screen_queues_but_hides_until_echo_confirms() {
        let mut s = alt_state();
        s.set_cursor(5, 3);
        assert_eq!(
            s.predict_key_at(&key_text("h"), 100),
            PredictionOutcome::Predicted,
            "alt screen predicts as usual — only display is gated"
        );
        assert_eq!(s.pending_len(), 1);
        assert!(!s.should_display(110), "no echo evidence yet — hidden");
        assert_eq!(s.displayable(110).count(), 0);
        // The app echoes: evidence lands via the reconcile path.
        s.confirm_echo();
        assert!(s.should_display(150), "confirmed echo unlocks display");
        assert_eq!(s.displayable(150).count(), 1);
    }

    #[test]
    fn an_overdue_front_hides_the_overlay_until_authority_catches_up() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.predict_key_at(&key_text("a"), 1_000);
        assert!(s.should_display(1_000 + DISPLAY_TTL_MS));
        assert!(
            !s.should_display(1_001 + DISPLAY_TTL_MS),
            "past the TTL the guess is a ghost — hide until authority lands"
        );
        assert_eq!(s.displayable(1_001 + DISPLAY_TTL_MS).count(), 0);
        assert_eq!(s.pending_len(), 1, "the queue still reconciles normally");
    }

    #[test]
    fn ttl_follows_the_front_of_the_queue() {
        // Staleness is measured at the front: dropping the old front
        // re-arms the overlay for the still-fresh suffix.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.predict_key_at(&key_text("a"), 0);
        s.predict_key_at(&key_text("b"), 900);
        assert!(!s.should_display(1_050), "front (t=0) is overdue");
        let _ = s.pop_front();
        assert!(s.should_display(1_050), "front is now the t=900 guess");
    }

    #[test]
    fn alt_screen_enter_suspends_instead_of_predicting() {
        // Enter in a TUI submits (an agent prompt) or executes (vim) — the
        // primary-screen row+1 guess would anchor the burst wrong.
        let mut s = alt_state();
        s.set_cursor(5, 3);
        s.predict_key_at(&key_text("x"), 100);
        assert_eq!(s.pending_len(), 1);
        let enter = key_named(PhysicalKey::Enter, ModSet::empty());
        assert_eq!(s.predict_key_at(&enter, 150), PredictionOutcome::Skipped);
        assert_eq!(s.pending_len(), 0, "the burst dropped with the Enter");
        assert!(s.is_suspended(), "suspended until the next cursor sync");
    }

    #[test]
    fn alt_screen_enter_keeps_the_echo_latch() {
        // A submit does not change who echoes: the next message displays
        // at once (one warm-up per screen session, not per line).
        let mut s = alt_state();
        s.set_cursor(10, 2);
        s.predict_key_at(&key_text("h"), 100);
        s.confirm_echo();
        let enter = key_named(PhysicalKey::Enter, ModSet::empty());
        s.predict_key_at(&enter, 200);
        s.set_cursor(11, 2); // prompt redrawn; driver reseeds
        s.predict_key_at(&key_text("y"), 300);
        assert!(s.should_display(310), "submit does not unlearn the echo");
    }

    #[test]
    fn alt_screen_esc_kills_the_latch_and_the_burst() {
        // vim insert mode echoes (latch earned); Esc leaves insert mode —
        // mode-changing input kills the evidence, so normal-mode motions
        // can never display, even before authority answers them.
        let mut s = alt_state();
        s.set_cursor(4, 0);
        s.predict_key_at(&key_text("a"), 100);
        s.confirm_echo();
        s.predict_key_at(&key_text("b"), 120);
        assert!(s.should_display(130), "insert mode earned the latch");

        assert_eq!(
            s.predict_key_at(&key_esc(), 200),
            PredictionOutcome::Skipped
        );
        assert_eq!(s.pending_len(), 0, "burst dropped with the mode");
        assert!(!s.echo_confirmed(), "Esc killed the evidence");
        s.set_cursor(4, 1); // next keypress reseeds (driver path)
        s.predict_key_at(&key_text("j"), 250);
        assert!(!s.should_display(260), "normal mode must re-earn echo");
    }

    #[test]
    fn alt_screen_mode_changing_kills_the_latch_even_while_suspended() {
        // The suspended guard must not shadow the latch kill: Enter
        // suspends (latch kept), then Esc arrives before any cursor sync —
        // the evidence still dies.
        let mut s = alt_state();
        s.set_cursor(3, 2);
        s.predict_key_at(&key_text("a"), 100);
        s.confirm_echo();
        let enter = key_named(PhysicalKey::Enter, ModSet::empty());
        s.predict_key_at(&enter, 150); // suspended, latch alive
        assert!(s.is_suspended());
        assert!(s.echo_confirmed());
        s.predict_key_at(&key_esc(), 200);
        assert!(!s.echo_confirmed(), "Esc kills the latch mid-suspend");
    }

    #[test]
    fn alt_screen_arrows_and_chords_are_mode_changing() {
        for ev in [
            key_named(PhysicalKey::ArrowLeft, ModSet::empty()),
            key_named(PhysicalKey::ArrowRight, ModSet::empty()),
            key_named(PhysicalKey::Tab, ModSet::empty()),
            key_named(PhysicalKey::U, ModSet::CTRL),
            key_named(PhysicalKey::A, ModSet::ALT),
        ] {
            let mut s = alt_state();
            s.set_cursor(2, 5);
            s.predict_key_at(&key_text("a"), 100);
            s.confirm_echo();
            assert_eq!(s.predict_key_at(&ev, 200), PredictionOutcome::Skipped);
            assert!(
                !s.echo_confirmed(),
                "{:?} must kill the alt-screen latch",
                ev.key
            );
        }
    }

    #[test]
    fn alt_screen_backspace_is_predicted_not_mode_changing() {
        let mut s = alt_state();
        s.set_cursor(2, 5);
        s.predict_key_at(&key_text("a"), 100);
        s.confirm_echo();
        let bs = key_named(PhysicalKey::Backspace, ModSet::empty());
        assert_eq!(s.predict_key_at(&bs, 200), PredictionOutcome::Predicted);
        assert!(s.echo_confirmed(), "backspace leaves the latch alone");
    }

    #[test]
    fn primary_screen_esc_does_not_suspend() {
        // Mode-changing handling is an alt-screen policy; on the primary
        // screen Esc falls through to the ordinary skip (no text payload)
        // without dropping the burst.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.predict_key_at(&key_text("a"), 100);
        assert_eq!(
            s.predict_key_at(&key_esc(), 150),
            PredictionOutcome::Skipped
        );
        assert_eq!(s.pending_len(), 1, "burst survives Esc on the primary");
        assert!(!s.is_suspended());
    }

    #[test]
    fn screen_transitions_drop_the_queue_and_the_evidence() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.predict_key_at(&key_text("a"), 100);
        s.confirm_echo();
        s.set_alt_screen(true);
        assert_eq!(s.pending_len(), 0, "anchors belong to the other screen");
        assert!(!s.echo_confirmed(), "evidence does not cross screens");
        s.set_cursor(0, 3);
        s.predict_key_at(&key_text("c"), 200);
        assert!(!s.should_display(210));
    }

    #[test]
    fn resize_and_clear_both_drop_the_evidence() {
        // Both discontinuities (geometry shift, snapshot replay) must
        // force the latch to be re-earned — fail-safe, one RTT of warmup.
        let mut resized = alt_state();
        resized.confirm_echo();
        resized.set_viewport(81, 24);
        assert!(!resized.echo_confirmed(), "resize dropped the latch");

        let mut cleared = alt_state();
        cleared.confirm_echo();
        cleared.clear();
        assert!(!cleared.echo_confirmed(), "clear dropped the latch");
    }

    #[test]
    fn locked_overlay_still_advances_the_cursor_estimate() {
        // Cursor consistency: while display is locked, the ESTIMATE
        // advances (the queue is ahead) but callers gate presentation on
        // should_display, so the authoritative cursor is what shows.
        let mut s = alt_state();
        s.set_cursor(2, 5);
        s.predict_key_at(&key_text("a"), 100);
        s.predict_key_at(&key_text("b"), 110);
        assert_eq!(s.cursor(), (2, 7), "estimate is ahead");
        assert!(!s.should_display(120), "but must not be presented");
    }
}
