//! Reconciliation — confirm, contradict, or keep predictions when
//! authoritative state arrives.
//!
//! [`reconcile_terminal_output_per_cell`] walks the prediction queue from
//! the front, reads each prediction's target cell, and partitions it into
//! **confirmed** (drop: the server painted exactly the guess), **pending**
//! (keep: not echoed yet), and **contradicted** (drop it and the whole
//! suffix: the server diverged).
//!
//! ## Confirmation rules
//!
//! | `PredictionKind` | Confirmed when | Pending when | Contradicted when |
//! |---|---|---|---|
//! | `Insert` | cell grapheme cluster == `text` | cell is blank (no grapheme or `" "`) | cell has any other grapheme |
//! | `BackspaceEol` | cell is blank | cell is blank | cell has any grapheme |
//! | `Newline` | `cursor.row > pred.row` | never (instantaneous) | `cursor.row <= pred.row` |
//! | `CursorLeft` / `CursorRight` | `cursor == (pred.row, pred.col)` | cursor is still on `pred.row` and (Left: `cursor.col > pred.col`, Right: `cursor.col < pred.col`) — server hasn't caught up | otherwise |
//!
//! `BackspaceEol`'s "blank or blank" collapse is intentional: a backspace
//! prediction predicts that the cell becomes blank, so a blank cell post-
//! reconcile is equivalent to confirmation; there is no "still pending"
//! state distinguishable from "confirmed" without snapshotting prior
//! contents, which we don't do.

use super::state::{PredictionKind, PredictionState};

/// Summary of a reconcile pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileStats {
    /// Predictions whose cell matched the authoritative grapheme.
    pub confirmed: usize,
    /// Predictions whose cell contradicted the prediction (and all
    /// subsequent predictions, which were dropped as a suffix).
    pub contradicted: usize,
    /// Predictions kept because the server has not yet echoed the cell.
    pub pending: usize,
}

/// Per-cell match reconcile. Walks the prediction queue against the
/// authoritative cell grid (read via the `read_cell` closure) and the
/// fresh cursor position.
///
/// `read_cell(row, col)` returns the cell's full grapheme cluster (so
/// multi-codepoint inserts can confirm), or `None` when blank.
///
/// The cursor estimate resyncs to `(cursor_row, cursor_col)` only when the
/// queue drains; otherwise it stays ahead so later inserts queue correctly.
pub fn reconcile_terminal_output_per_cell<F>(
    state: &mut PredictionState,
    cursor_row: u16,
    cursor_col: u16,
    read_cell: F,
) -> ReconcileStats
where
    F: FnMut(u16, u16) -> Option<String>,
{
    reconcile_terminal_output_per_cell_at(state, cursor_row, cursor_col, 0, read_cell)
}

/// Timestamped per-cell reconcile.
///
/// This is the production entry point for hosts with a monotonic clock. The
/// same clock origin must be used for `now_ms` and the `*_at` prediction call
/// that stamped each queued guess. Confirmed non-blank inserts feed the
/// predictor's smoothed echo RTT; blank inserts, other prediction kinds,
/// missing clocks, and contradictions never do.
pub fn reconcile_terminal_output_per_cell_at<F>(
    state: &mut PredictionState,
    cursor_row: u16,
    cursor_col: u16,
    now_ms: u64,
    mut read_cell: F,
) -> ReconcileStats
where
    F: FnMut(u16, u16) -> Option<String>,
{
    let mut summary = ReconcileStats::default();

    loop {
        let row;
        let col;
        let kind;
        let predicted;
        let queued_at_ms;
        {
            let Some(front) = state.front() else {
                break;
            };
            row = front.row;
            col = front.col;
            kind = front.kind;
            // Clone the predicted cluster so the `read_cell` closure (which
            // mutably borrows the grid) can run without holding `front`.
            predicted = front.text.clone();
            queued_at_ms = front.queued_at_ms;
        }

        let verdict = classify_prediction(
            kind,
            row,
            col,
            &predicted,
            cursor_row,
            cursor_col,
            &mut read_cell,
        );

        match verdict {
            Verdict::Confirmed => {
                summary.confirmed += 1;
                // Only a non-blank insert is echo evidence (ADR-0090): a
                // blank cell also "confirms" in non-echoing apps.
                if kind == PredictionKind::Insert && predicted != " " {
                    state.confirm_echo_at(queued_at_ms, now_ms);
                }
                let _ = state.pop_front();
            }
            Verdict::Pending => {
                summary.pending = state.pending_len();
                break;
            }
            Verdict::Contradicted => {
                // Drop this and every subsequent prediction: the server
                // diverged from our guess, so the suffix is suspect.
                summary.contradicted = state.pending_len();
                state.clear();
                break;
            }
        }
    }

    // Only resync the cursor estimate if we drained the queue. Otherwise
    // the predict-side cursor is *intentionally* ahead — leave it.
    if state.pending_len() == 0 {
        state.set_cursor(cursor_row, cursor_col);
    }

    // Contradicting runs hide the overlay; clean passes lift the lock.
    // Prediction keeps running while tentative: a confirm is the only
    // re-arm signal.
    state.note_reconcile(summary);

    summary
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Confirmed,
    Pending,
    Contradicted,
}

fn classify_prediction<F>(
    kind: PredictionKind,
    row: u16,
    col: u16,
    predicted: &str,
    cursor_row: u16,
    cursor_col: u16,
    read_cell: &mut F,
) -> Verdict
where
    F: FnMut(u16, u16) -> Option<String>,
{
    match kind {
        PredictionKind::Insert => {
            let actual = read_cell(row, col);
            classify_insert(predicted, actual.as_deref())
        }
        PredictionKind::BackspaceEol => {
            let actual = read_cell(row, col);
            classify_backspace(actual.as_deref())
        }
        PredictionKind::Newline => classify_newline(row, cursor_row),
        PredictionKind::CursorLeft => classify_cursor_left(row, col, cursor_row, cursor_col),
        PredictionKind::CursorRight => classify_cursor_right(row, col, cursor_row, cursor_col),
    }
}

fn classify_insert(predicted: &str, actual: Option<&str>) -> Verdict {
    match actual {
        Some(c) if c == predicted => Verdict::Confirmed,
        Some(" ") | None => Verdict::Pending,
        Some(_) => Verdict::Contradicted,
    }
}

fn classify_backspace(actual: Option<&str>) -> Verdict {
    match actual {
        Some(" ") | None => Verdict::Confirmed,
        Some(_) => Verdict::Contradicted,
    }
}

const fn classify_newline(pred_row: u16, cursor_row: u16) -> Verdict {
    if cursor_row > pred_row {
        Verdict::Confirmed
    } else {
        Verdict::Contradicted
    }
}

/// Reconcile a [`PredictionKind::CursorLeft`] prediction. Confirmed when the authoritative
/// cursor matches the predicted target. Pending when the cursor is
/// still on the same row and to the *right* of the predicted target
/// (server has not yet processed the motion). Otherwise contradicted.
const fn classify_cursor_left(
    pred_row: u16,
    pred_col: u16,
    cursor_row: u16,
    cursor_col: u16,
) -> Verdict {
    if cursor_row == pred_row && cursor_col == pred_col {
        Verdict::Confirmed
    } else if cursor_row == pred_row && cursor_col > pred_col {
        Verdict::Pending
    } else {
        Verdict::Contradicted
    }
}

/// Reconcile a [`PredictionKind::CursorRight`] prediction. Symmetric to
/// [`classify_cursor_left`] — pending when the authoritative cursor is
/// still left of where we predicted on the same row.
const fn classify_cursor_right(
    pred_row: u16,
    pred_col: u16,
    cursor_row: u16,
    cursor_col: u16,
) -> Verdict {
    if cursor_row == pred_row && cursor_col == pred_col {
        Verdict::Confirmed
    } else if cursor_row == pred_row && cursor_col < pred_col {
        Verdict::Pending
    } else {
        Verdict::Contradicted
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::predict::state::{PredictionState, PredictiveConfig};
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

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

    /// Cells not listed are blank; values are full grapheme clusters.
    fn row_reader<'a>(
        cells: &'a [((u16, u16), &'a str)],
    ) -> impl FnMut(u16, u16) -> Option<String> + 'a {
        move |r, c| {
            cells
                .iter()
                .find(|((rr, cc), _)| *rr == r && *cc == c)
                .map(|(_, s)| (*s).to_owned())
        }
    }

    enum K {
        T(&'static str),
        N(PhysicalKey),
    }

    struct Case {
        name: &'static str,
        start: Option<(u16, u16)>,
        keys: &'static [K],
        server_cursor: (u16, u16),
        cells: &'static [((u16, u16), &'static str)],
        /// (confirmed, contradicted, pending)
        stats: (usize, usize, usize),
        left: usize,
        cursor_after: Option<(u16, u16)>,
    }

    const FLAG: &str = "\u{1F1FA}\u{1F1F8}";
    const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    const ACCENTED: &str = "e\u{0301}";

    #[test]
    fn per_cell_match_game() {
        use K::{N, T};
        use PhysicalKey::{ArrowLeft, ArrowRight, Backspace, Enter};
        #[rustfmt::skip]
        let cases = [
            Case { name: "all confirmed drains and resyncs", start: None, keys: &[T("h"), T("i")], server_cursor: (0, 2), cells: &[((0, 0), "h"), ((0, 1), "i")], stats: (2, 0, 0), left: 0, cursor_after: Some((0, 2)) },
            // The predict-side cursor stays ahead while predictions remain.
            Case { name: "partial confirm keeps tail", start: None, keys: &[T("h"), T("e"), T("l"), T("l"), T("o")], server_cursor: (0, 2), cells: &[((0, 0), "h"), ((0, 1), "e")], stats: (2, 0, 3), left: 3, cursor_after: Some((0, 5)) },
            Case { name: "contradiction drops suffix", start: None, keys: &[T("a"), T("b"), T("c")], server_cursor: (0, 1), cells: &[((0, 0), "a"), ((0, 1), "X")], stats: (1, 2, 0), left: 0, cursor_after: Some((0, 1)) },
            Case { name: "pending preserves predict anchor", start: None, keys: &[T("a"), T("b"), T("c")], server_cursor: (0, 0), cells: &[], stats: (0, 0, 3), left: 3, cursor_after: Some((0, 3)) },
            Case { name: "empty queue resyncs", start: None, keys: &[], server_cursor: (9, 9), cells: &[], stats: (0, 0, 0), left: 0, cursor_after: Some((9, 9)) },
            // Multi-codepoint clusters compare whole, not by base scalar.
            Case { name: "flag", start: None, keys: &[T(FLAG)], server_cursor: (0, 2), cells: &[((0, 0), FLAG)], stats: (1, 0, 0), left: 0, cursor_after: Some((0, 2)) },
            Case { name: "zwj family", start: None, keys: &[T(FAMILY)], server_cursor: (0, 2), cells: &[((0, 0), FAMILY)], stats: (1, 0, 0), left: 0, cursor_after: Some((0, 2)) },
            Case { name: "combining mark", start: None, keys: &[T(ACCENTED)], server_cursor: (0, 1), cells: &[((0, 0), ACCENTED)], stats: (1, 0, 0), left: 0, cursor_after: Some((0, 1)) },
            Case { name: "bare base contradicts combining", start: None, keys: &[T(ACCENTED)], server_cursor: (0, 1), cells: &[((0, 0), "e")], stats: (0, 1, 0), left: 0, cursor_after: None },
            Case { name: "grapheme pending on blank", start: None, keys: &[T(FLAG)], server_cursor: (0, 0), cells: &[], stats: (0, 0, 1), left: 1, cursor_after: None },
            // A pending insert at the front blocks the backspace behind it.
            Case { name: "backspace behind pending insert", start: None, keys: &[T("a"), N(Backspace)], server_cursor: (0, 0), cells: &[], stats: (0, 0, 2), left: 2, cursor_after: None },
            Case { name: "backspace confirmed by blank", start: Some((0, 6)), keys: &[N(Backspace)], server_cursor: (0, 5), cells: &[], stats: (1, 0, 0), left: 0, cursor_after: None },
            Case { name: "backspace contradicted by glyph", start: Some((0, 6)), keys: &[N(Backspace)], server_cursor: (0, 6), cells: &[((0, 5), "q")], stats: (0, 1, 0), left: 0, cursor_after: None },
            Case { name: "newline confirmed by row advance", start: None, keys: &[T("h"), T("i"), N(Enter)], server_cursor: (1, 0), cells: &[((0, 0), "h"), ((0, 1), "i")], stats: (3, 0, 0), left: 0, cursor_after: Some((1, 0)) },
            Case { name: "newline contradicted when row stays", start: None, keys: &[T("h"), T("i"), N(Enter)], server_cursor: (0, 2), cells: &[((0, 0), "h"), ((0, 1), "i")], stats: (2, 1, 0), left: 0, cursor_after: Some((0, 2)) },
            Case { name: "left confirmed", start: Some((0, 5)), keys: &[N(ArrowLeft)], server_cursor: (0, 4), cells: &[], stats: (1, 0, 0), left: 0, cursor_after: Some((0, 4)) },
            Case { name: "left pending while server lags", start: Some((0, 5)), keys: &[N(ArrowLeft)], server_cursor: (0, 5), cells: &[], stats: (0, 0, 1), left: 1, cursor_after: Some((0, 4)) },
            Case { name: "left contradicted by row jump", start: Some((0, 5)), keys: &[N(ArrowLeft)], server_cursor: (1, 0), cells: &[], stats: (0, 1, 0), left: 0, cursor_after: None },
            Case { name: "right confirmed", start: Some((0, 3)), keys: &[N(ArrowRight)], server_cursor: (0, 4), cells: &[], stats: (1, 0, 0), left: 0, cursor_after: Some((0, 4)) },
            Case { name: "right pending while server lags", start: Some((0, 3)), keys: &[N(ArrowRight)], server_cursor: (0, 3), cells: &[], stats: (0, 0, 1), left: 1, cursor_after: Some((0, 4)) },
        ];
        for case in cases {
            let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
            if let Some((row, col)) = case.start {
                s.set_cursor(row, col);
            }
            for key in case.keys {
                let event = match key {
                    T(text) => key_text(text),
                    N(named) => key_named(*named, ModSet::empty()),
                };
                // Every cell is a known narrow glyph so arrows can predict.
                let outcome = s.predict_key_with_grid(&event, |_, _| Some('a'));
                assert_eq!(outcome, PredictionOutcome::Predicted, "{}", case.name);
            }
            let (row, col) = case.server_cursor;
            let summary =
                reconcile_terminal_output_per_cell(&mut s, row, col, row_reader(case.cells));
            let (confirmed, contradicted, pending) = case.stats;
            assert_eq!(
                summary,
                ReconcileStats {
                    confirmed,
                    contradicted,
                    pending
                },
                "{}",
                case.name
            );
            assert_eq!(s.pending_len(), case.left, "{}", case.name);
            if let Some(cursor) = case.cursor_after {
                assert_eq!(s.cursor(), cursor, "{}", case.name);
            }
        }
    }

    use crate::predict::state::PredictionOutcome;

    // -- adaptive tentative display (ADR-0090) ---

    /// Type one char and reconcile against a cell the server painted
    /// differently — a single contradicting per-cell pass driven entirely
    /// through the production `reconcile_terminal_output_per_cell` path
    /// (so `note_reconcile` is exercised by the real call site).
    fn contradict_one_insert(s: &mut PredictionState) {
        // Re-arm and place the cursor, then predict an insert.
        s.set_cursor(0, 0);
        assert_eq!(s.predict_key(&key_text("h")), PredictionOutcome::Predicted);
        // Server painted 'X' instead of 'h' → the insert is contradicted.
        let summary = reconcile_terminal_output_per_cell(s, 0, 1, row_reader(&[((0, 0), "X")]));
        assert_eq!(summary.contradicted, 1);
    }

    /// Type one char and reconcile against the cell the server confirmed —
    /// a clean productive per-cell pass through the production path.
    fn confirm_one_insert(s: &mut PredictionState) {
        s.set_cursor(0, 0);
        assert_eq!(s.predict_key(&key_text("h")), PredictionOutcome::Predicted);
        let summary = reconcile_terminal_output_per_cell(s, 0, 1, row_reader(&[((0, 0), "h")]));
        assert_eq!(summary.confirmed, 1);
    }

    #[test]
    fn three_contradicting_reconciles_hide_the_overlay() {
        // End-to-end: three contradicting per-cell reconciles via the real
        // reconcile entry point turn the state tentative. Keystrokes keep
        // queueing (the lift signal is a confirmed prediction) but the
        // display policy hides them.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        contradict_one_insert(&mut s);
        contradict_one_insert(&mut s);
        assert!(!s.is_tentative(), "two contradictions: still displaying");
        contradict_one_insert(&mut s);
        assert!(s.is_tentative(), "three contradictions turn tentative");
        assert_eq!(s.predict_key(&key_text("a")), PredictionOutcome::Predicted);
        assert!(!s.should_display(0), "queued but hidden — no ghost painted");
    }

    #[test]
    fn tentative_then_clean_reconciles_lift_through_the_production_path() {
        // The lock lifts entirely through the production path: predicting
        // continues while tentative, so the server confirming two typed
        // characters is observable by note_reconcile and re-shows the
        // overlay. (Under the retired predict-suspend model this was
        // impossible — no predictions meant no confirms, ever.)
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        contradict_one_insert(&mut s);
        contradict_one_insert(&mut s);
        contradict_one_insert(&mut s);
        assert!(s.is_tentative());

        confirm_one_insert(&mut s);
        assert!(s.is_tentative(), "one clean pass is not enough");
        confirm_one_insert(&mut s);
        assert!(!s.is_tentative(), "two clean passes lift the lock");

        s.set_cursor(0, 0);
        s.predict_key(&key_text("a"));
        assert!(s.should_display(0), "overlay displays again");
    }

    // -- ADR-0090: confirmation-gated alt-screen display, app by app ----

    #[test]
    fn alt_screen_echo_confirmation_unlocks_the_pending_suffix() {
        // An agent TUI / vim insert mode: the app echoes. The first
        // confirmed non-blank insert is the evidence; the still-pending
        // tail of the burst becomes displayable at once — the warm-up is
        // paid once per screen session, not per key.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_alt_screen(true);
        s.set_cursor(5, 3);
        for (ch, t) in [("a", 100), ("b", 110), ("c", 120)] {
            assert_eq!(
                s.predict_key_at(&key_text(ch), t),
                PredictionOutcome::Predicted
            );
        }
        assert!(!s.should_display(130), "no evidence yet — hidden");
        // Server echoes 'a'; 'b' and 'c' still in flight.
        let summary =
            reconcile_terminal_output_per_cell(&mut s, 5, 4, row_reader(&[((5, 3), "a")]));
        assert_eq!(summary.confirmed, 1);
        assert_eq!(s.pending_len(), 2);
        assert!(s.should_display(150), "confirmed echo unlocks display");
        assert_eq!(s.displayable(150).count(), 2, "whole tail displayable");
    }

    #[test]
    fn htop_style_silence_never_displays_and_still_reconciles() {
        // htop: keys act (sort order flips) but nothing echoes — the
        // anchor cells stay blank forever. Pending is not evidence.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_alt_screen(true);
        s.set_cursor(3, 7);
        for (ch, t) in [("j", 100), ("j", 110), ("k", 120)] {
            s.predict_key_at(&key_text(ch), t);
        }
        for now in [130, 500, 5_000] {
            assert!(!s.should_display(now), "silence must never display");
        }
        let summary = reconcile_terminal_output_per_cell(&mut s, 3, 7, row_reader(&[]));
        assert_eq!(summary.pending, 3, "blank cells leave the queue pending");
        assert!(!s.should_display(5_000));
    }

    #[test]
    fn htop_style_repaint_contradicts_without_ever_displaying() {
        // htop repaints its meters over the anchor: the guess contradicts.
        // The queue drops, the latch stays locked, and later keys stay
        // hidden — the ghost-glyph regression is structurally impossible.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_alt_screen(true);
        s.set_cursor(0, 0);
        s.predict_key_at(&key_text("q"), 100);
        let summary =
            reconcile_terminal_output_per_cell(&mut s, 0, 0, row_reader(&[((0, 0), "C")]));
        assert_eq!(summary.contradicted, 1); // "CPU" repaint
        s.predict_key_at(&key_text("q"), 200);
        assert!(!s.should_display(210), "contradiction is not evidence");
    }

    #[test]
    fn less_style_blank_confirms_never_earn_evidence() {
        // less: space pages down; the predicted " " matches a blank cell
        // without any echo happening. It must not unlock display for what
        // follows.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_alt_screen(true);
        s.set_cursor(0, 0);
        s.predict_key_at(&key_text(" "), 100);
        let summary =
            reconcile_terminal_output_per_cell(&mut s, 0, 1, row_reader(&[((0, 0), " ")]));
        assert_eq!(summary.confirmed, 1, "blank confirmed and drained");
        assert!(!s.echo_confirmed(), "blank confirm is not evidence");
        s.predict_key_at(&key_text("q"), 200);
        assert!(!s.should_display(210), "still no evidence — still hidden");
    }

    #[test]
    fn vim_contradiction_relocks_the_earned_latch() {
        // vim insert mode earned the latch; the app then diverges (left
        // insert mode, prompt redrew) — display re-locks and must be
        // re-earned before anything shows again.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_alt_screen(true);
        s.set_cursor(0, 0);
        s.predict_key_at(&key_text("a"), 100);
        reconcile_terminal_output_per_cell(&mut s, 0, 1, row_reader(&[((0, 0), "a")]));
        assert!(s.echo_confirmed(), "evidence earned");
        s.predict_key_at(&key_text("b"), 200);
        assert!(s.should_display(210));
        reconcile_terminal_output_per_cell(&mut s, 0, 1, row_reader(&[((0, 1), "X")]));
        assert!(!s.echo_confirmed(), "contradiction killed the evidence");
        s.predict_key_at(&key_text("c"), 300);
        assert!(!s.should_display(310), "re-earn evidence after divergence");
    }

    #[test]
    fn overdue_overlay_recovers_once_authority_catches_up() {
        // Glitch back-off is a hide, not a kill: when the late echo
        // finally confirms, fresh guesses display again immediately.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 0);
        s.predict_key_at(&key_text("a"), 100);
        assert!(!s.should_display(2_000), "overdue — hidden");
        reconcile_terminal_output_per_cell(&mut s, 0, 1, row_reader(&[((0, 0), "a")]));
        s.predict_key_at(&key_text("b"), 2_500);
        assert!(s.should_display(2_510), "fresh front displays again");
    }

    // -- SRTT-adaptive display TTL -------------------------------------

    fn confirm_insert_at(
        state: &mut PredictionState,
        text: &str,
        queued_at_ms: u64,
        confirmed_at_ms: u64,
    ) {
        state.set_cursor(0, 0);
        assert_eq!(
            state.predict_key_at(&key_text(text), queued_at_ms),
            PredictionOutcome::Predicted
        );
        let cells = [((0, 0), text)];
        let result =
            reconcile_terminal_output_per_cell_at(state, 0, 1, confirmed_at_ms, row_reader(&cells));
        assert_eq!(result.confirmed, 1);
    }

    #[test]
    fn slow_confirmed_echo_stretches_the_display_ttl() {
        let mut state = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        confirm_insert_at(&mut state, "a", 100, 1_600);
        assert_eq!(state.display_ttl_ms(), 3_000);
        state.set_cursor(0, 1);
        state.predict_key_at(&key_text("b"), 10_000);
        assert!(state.should_display(13_000));
        assert!(!state.should_display(13_001));
    }

    #[test]
    fn ttl_has_floor_cap_and_one_eighth_ewma_gain() {
        let mut fast = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        confirm_insert_at(&mut fast, "a", 100, 150);
        assert_eq!(fast.display_ttl_ms(), 1_000, "fast sample stays floored");

        let mut capped = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        confirm_insert_at(&mut capped, "a", 100, 60_100);
        assert_eq!(
            capped.display_ttl_ms(),
            5_000,
            "pathological sample is capped"
        );

        let mut smooth = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        confirm_insert_at(&mut smooth, "a", 100, 1_100);
        confirm_insert_at(&mut smooth, "b", 2_000, 4_000);
        assert_eq!(smooth.display_ttl_ms(), 2_250, "SRTT is 1125 ms");
    }

    #[test]
    fn blank_clockless_and_backwards_confirms_do_not_sample() {
        let mut state = PredictionState::new(PredictiveConfig::enabled(), 80, 24);

        confirm_insert_at(&mut state, " ", 100, 9_000);
        confirm_insert_at(&mut state, "a", 0, 9_500);
        confirm_insert_at(&mut state, "b", 10_000, 9_999);

        assert_eq!(state.display_ttl_ms(), 1_000);
    }

    #[test]
    fn srtt_survives_screen_clear_resize_and_contradiction() {
        let mut state = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        confirm_insert_at(&mut state, "a", 100, 2_100);
        assert_eq!(state.display_ttl_ms(), 4_000);

        state.set_alt_screen(true);
        state.clear();
        state.set_viewport(100, 30);
        state.set_cursor(0, 0);
        state.predict_key_at(&key_text("x"), 3_000);
        let _ = reconcile_terminal_output_per_cell_at(
            &mut state,
            0,
            1,
            3_100,
            row_reader(&[((0, 0), "X")]),
        );

        assert_eq!(state.display_ttl_ms(), 4_000);
    }
}
