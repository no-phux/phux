//! Reconciliation — confirm, contradict, or keep predictions when
//! authoritative state arrives.
//!
//! The entry point is [`reconcile_terminal_output_per_cell`], the v1.1
//! per-cell match game (phux-9gw.1.1). It walks the prediction queue
//! from the front, peeks each prediction's target cell via a
//! caller-supplied read closure, and partitions the queue into:
//!
//! - **confirmed** — drop (the server already painted the cell
//!   exactly as predicted, so the overlay can stop decorating it);
//! - **pending** — keep (the cell is still blank, the server
//!   hasn't echoed yet — keep the overlay alive);
//! - **contradicted** — drop this *and* every subsequent prediction
//!   (the server diverged from our guess, so the entire suffix is
//!   suspect).
//!
//! The match game is what eliminates the visual flicker that the retired
//! v0 wholesale-drain policy suffered:
//! every server frame previously dropped all predictions, briefly
//! showing the underline disappear before the renderer caught up. With
//! per-cell match, predictions that the server has already confirmed
//! transition cleanly to authoritative paint, and predictions still
//! ahead of confirmed state keep their decoration.
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

/// Summary of a reconcile pass. Returned for diagnostics and asserted
/// against in the test suite.
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
/// `read_cell(row, col)` returns the full grapheme cluster of the cell at
/// the given coordinates, or `None` if the cell is blank (no grapheme or a
/// `" "` placeholder — callers may treat those equivalently). Returning
/// the whole cluster (not just the base scalar) lets `Insert` reconcile
/// confirm multi-codepoint predictions — flag emoji, ZWJ sequences, base
/// plus combining marks (phux-9gw.1.6).
///
/// The cursor estimate is resynced to `(cursor_row, cursor_col)` if and
/// only if the queue is fully drained. If predictions remain (i.e. the
/// front of the queue is still pending), the predict-side cursor is
/// left ahead of the authoritative cursor so subsequent inserts queue
/// at the right anchor; the renderer will catch up on the next ack.
pub fn reconcile_terminal_output_per_cell<F>(
    state: &mut PredictionState,
    cursor_row: u16,
    cursor_col: u16,
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
        }

        let verdict = match kind {
            PredictionKind::Insert => {
                let actual = read_cell(row, col);
                classify_insert(&predicted, actual.as_deref())
            }
            PredictionKind::BackspaceEol => {
                let actual = read_cell(row, col);
                classify_backspace(actual.as_deref())
            }
            PredictionKind::Newline => classify_newline(row, cursor_row),
            PredictionKind::CursorLeft => classify_cursor_left(row, col, cursor_row, cursor_col),
            PredictionKind::CursorRight => classify_cursor_right(row, col, cursor_row, cursor_col),
        };

        match verdict {
            Verdict::Confirmed => {
                summary.confirmed += 1;
                // ADR-0090: only a NON-BLANK insert confirmation is echo
                // evidence. A confirmed backspace or a confirmed space is
                // trivially satisfiable by a blank cell in a non-echoing
                // app (space is page-down in less, pause in htop), so it
                // must not unlock alt-screen display.
                if kind == PredictionKind::Insert && predicted != " " {
                    state.confirm_echo();
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

    // Feed this pass into the adaptive tentative-display heuristic
    // (phux-pxaj, reshaped by ADR-0090): a run of contradicting passes
    // (vi-mode, a modal app, fast transitions) hides the overlay; clean
    // productive passes afterward lift the lock. Predicting itself keeps
    // running while tentative — a confirmed prediction is the only
    // re-arm signal, so suspending prediction here would make the lock
    // permanent.
    state.note_reconcile(summary);

    summary
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Confirmed,
    Pending,
    Contradicted,
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

    // -- per-cell match game ---------------------------------------------

    /// Build a row read closure backed by an associative slice of
    /// `((row, col), &str)` mappings. The `&str` is the cell's full
    /// grapheme cluster (a single scalar in the common case, a flag /
    /// ZWJ / combining cluster otherwise). Cells not in the slice are
    /// blank.
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

    #[test]
    fn per_cell_all_confirmed_drains_and_resyncs_cursor() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for ch in ["h", "i"] {
            s.predict_key(&key_text(ch));
        }
        assert_eq!(s.pending_len(), 2);
        // Server has caught up: cells 'h' and 'i' painted; cursor at col 2.
        let summary = reconcile_terminal_output_per_cell(
            &mut s,
            0,
            2,
            row_reader(&[((0, 0), "h"), ((0, 1), "i")]),
        );
        assert_eq!(summary.confirmed, 2);
        assert_eq!(summary.contradicted, 0);
        assert_eq!(summary.pending, 0);
        assert_eq!(s.pending_len(), 0);
        assert_eq!(s.cursor(), (0, 2));
    }

    #[test]
    fn per_cell_partial_confirm_keeps_tail_alive() {
        // Predicted "hello", server has echoed only "he" so far.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for ch in ["h", "e", "l", "l", "o"] {
            s.predict_key(&key_text(ch));
        }
        assert_eq!(s.pending_len(), 5);
        let summary = reconcile_terminal_output_per_cell(
            &mut s,
            0,
            2,
            row_reader(&[((0, 0), "h"), ((0, 1), "e")]),
        );
        assert_eq!(summary.confirmed, 2);
        assert_eq!(summary.pending, 3);
        assert_eq!(summary.contradicted, 0);
        // Three predictions still alive; their cells still blank.
        assert_eq!(s.pending_len(), 3);
        let remaining_cols: Vec<u16> = s.pending().map(|p| p.col).collect();
        assert_eq!(remaining_cols, vec![2, 3, 4]);
        // Cursor estimate stays ahead — we have predictions in flight.
        // The predict-side cursor was at (0, 5) and reconcile must not
        // pull it backward to (0, 2).
        assert_eq!(s.cursor(), (0, 5));
    }

    #[test]
    fn per_cell_contradiction_drops_suffix() {
        // Predicted "abc", server painted 'X' at col 1 instead.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for ch in ["a", "b", "c"] {
            s.predict_key(&key_text(ch));
        }
        assert_eq!(s.pending_len(), 3);
        let summary = reconcile_terminal_output_per_cell(
            &mut s,
            0,
            1,
            row_reader(&[((0, 0), "a"), ((0, 1), "X")]),
        );
        // 'a' confirmed, 'b' contradicted (cell is 'X'), 'c' dropped as
        // suffix. The contradicted counter records the size of the
        // dropped suffix (including the contradicting prediction itself).
        assert_eq!(summary.confirmed, 1);
        assert_eq!(summary.contradicted, 2);
        assert_eq!(summary.pending, 0);
        assert_eq!(s.pending_len(), 0);
        assert_eq!(s.cursor(), (0, 1));
    }

    // -- multi-codepoint grapheme reconcile (phux-9gw.1.6) ---------------

    #[test]
    fn per_cell_flag_emoji_confirmed_against_full_cluster() {
        // 🇺🇸 = U+1F1FA U+1F1F8, predicted as one width-2 insert. The
        // server paints the full cluster into the base cell; reconcile
        // must compare the whole cluster, not just the base scalar.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let flag = "\u{1F1FA}\u{1F1F8}";
        assert_eq!(s.predict_key(&key_text(flag)), PredictionOutcome::Predicted);
        assert_eq!(s.pending_len(), 1);
        let summary =
            reconcile_terminal_output_per_cell(&mut s, 0, 2, row_reader(&[((0, 0), flag)]));
        assert_eq!(summary.confirmed, 1);
        assert_eq!(summary.contradicted, 0);
        assert_eq!(s.pending_len(), 0);
        assert_eq!(s.cursor(), (0, 2));
    }

    #[test]
    fn per_cell_zwj_family_emoji_confirmed_against_full_cluster() {
        // 👨‍👩‍👧 — man + ZWJ + woman + ZWJ + girl, one width-2 cell.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(
            s.predict_key(&key_text(family)),
            PredictionOutcome::Predicted
        );
        let summary =
            reconcile_terminal_output_per_cell(&mut s, 0, 2, row_reader(&[((0, 0), family)]));
        assert_eq!(summary.confirmed, 1);
        assert_eq!(s.pending_len(), 0);
        assert_eq!(s.cursor(), (0, 2));
    }

    #[test]
    fn per_cell_combining_mark_cluster_confirmed_against_full_cluster() {
        // "e\u{0301}" — base 'e' plus COMBINING ACUTE ACCENT, one width-1
        // cell. Reconcile confirms only when the cell carries the full
        // two-scalar cluster, not a bare 'e'.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let accented = "e\u{0301}";
        assert_eq!(
            s.predict_key(&key_text(accented)),
            PredictionOutcome::Predicted
        );
        let summary =
            reconcile_terminal_output_per_cell(&mut s, 0, 1, row_reader(&[((0, 0), accented)]));
        assert_eq!(summary.confirmed, 1);
        assert_eq!(s.pending_len(), 0);
        assert_eq!(s.cursor(), (0, 1));
    }

    #[test]
    fn per_cell_combining_mark_cluster_contradicted_by_bare_base() {
        // Prediction is the full "e\u{0301}" cluster, but the server
        // painted only a bare 'e' (combining mark not yet applied). The
        // clusters differ → contradiction, not confirmation.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let accented = "e\u{0301}";
        assert_eq!(
            s.predict_key(&key_text(accented)),
            PredictionOutcome::Predicted
        );
        let summary =
            reconcile_terminal_output_per_cell(&mut s, 0, 1, row_reader(&[((0, 0), "e")]));
        assert_eq!(summary.confirmed, 0);
        assert_eq!(summary.contradicted, 1);
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn per_cell_grapheme_pending_when_cell_blank() {
        // Server hasn't echoed the flag yet — cell blank → keep pending.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let flag = "\u{1F1FA}\u{1F1F8}";
        s.predict_key(&key_text(flag));
        let summary = reconcile_terminal_output_per_cell(&mut s, 0, 0, row_reader(&[]));
        assert_eq!(summary.pending, 1);
        assert_eq!(s.pending_len(), 1);
    }

    #[test]
    fn per_cell_backspace_confirmed_when_cell_blank() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.predict_key(&key_text("a"));
        let bs = key_named(PhysicalKey::Backspace, ModSet::empty());
        s.predict_key(&bs);
        assert_eq!(s.pending_len(), 2);
        // Server confirms: cell at col 0 is blank, cursor at col 0.
        let summary = reconcile_terminal_output_per_cell(&mut s, 0, 0, row_reader(&[]));
        // The 'a' prediction is pending (cell blank → still waiting),
        // so the front-of-queue is pending and we stop. The backspace
        // never gets reconciled because we hit pending first.
        // This is the "Insert prediction is still pending" semantics.
        // For backspace-after-pending-insert: the predict layer made a
        // sequence the server hasn't shown yet; we keep both.
        assert_eq!(summary.confirmed, 0);
        assert_eq!(summary.pending, 2);
        assert_eq!(s.pending_len(), 2);
    }

    #[test]
    fn per_cell_backspace_alone_confirmed_when_blank() {
        // Backspace directly: predict layer thinks col 5 is now blank.
        // Server confirms by painting blank there.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 6);
        let bs = key_named(PhysicalKey::Backspace, ModSet::empty());
        s.predict_key(&bs);
        assert_eq!(s.pending_len(), 1);
        let summary = reconcile_terminal_output_per_cell(&mut s, 0, 5, row_reader(&[]));
        assert_eq!(summary.confirmed, 1);
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn per_cell_backspace_contradicted_when_cell_nonblank() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 6);
        let bs = key_named(PhysicalKey::Backspace, ModSet::empty());
        s.predict_key(&bs);
        // Server painted 'q' there instead — the shell wasn't ready for
        // backspace (e.g. the line had no input to delete).
        let summary =
            reconcile_terminal_output_per_cell(&mut s, 0, 6, row_reader(&[((0, 5), "q")]));
        assert_eq!(summary.contradicted, 1);
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn per_cell_newline_confirmed_when_cursor_advances() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for ch in ["h", "i"] {
            s.predict_key(&key_text(ch));
        }
        let enter = key_named(PhysicalKey::Enter, ModSet::empty());
        assert_eq!(s.predict_key(&enter), PredictionOutcome::Predicted);
        assert_eq!(s.pending_len(), 3);
        // Server has caught up: 'hi' at row 0, cursor advanced to row 1.
        let summary = reconcile_terminal_output_per_cell(
            &mut s,
            1,
            0,
            row_reader(&[((0, 0), "h"), ((0, 1), "i")]),
        );
        assert_eq!(summary.confirmed, 3);
        assert_eq!(s.pending_len(), 0);
        assert_eq!(s.cursor(), (1, 0));
    }

    #[test]
    fn per_cell_newline_contradicted_when_cursor_stayed() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for ch in ["h", "i"] {
            s.predict_key(&key_text(ch));
        }
        let enter = key_named(PhysicalKey::Enter, ModSet::empty());
        s.predict_key(&enter);
        // Server painted 'hi' but did not honor Enter (program intercepted).
        // Cursor still on row 0.
        let summary = reconcile_terminal_output_per_cell(
            &mut s,
            0,
            2,
            row_reader(&[((0, 0), "h"), ((0, 1), "i")]),
        );
        // First two predictions confirmed; Newline contradicted → drop.
        assert_eq!(summary.confirmed, 2);
        assert_eq!(summary.contradicted, 1);
        assert_eq!(s.pending_len(), 0);
        assert_eq!(s.cursor(), (0, 2));
    }

    #[test]
    fn per_cell_empty_queue_resyncs_cursor() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let summary = reconcile_terminal_output_per_cell(&mut s, 9, 9, row_reader(&[]));
        assert_eq!(summary.confirmed, 0);
        assert_eq!(summary.pending, 0);
        assert_eq!(summary.contradicted, 0);
        assert_eq!(s.cursor(), (9, 9));
    }

    #[test]
    fn per_cell_pending_preserves_predict_cursor_anchor() {
        // Regression: when predictions remain (server hasn't caught up),
        // do not overwrite the predict-side cursor with the lagging
        // authoritative cursor — subsequent inserts must continue to
        // queue at the predicted position, not snap backward.
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for ch in ["a", "b", "c"] {
            s.predict_key(&key_text(ch));
        }
        assert_eq!(s.cursor(), (0, 3));
        let _ = reconcile_terminal_output_per_cell(&mut s, 0, 0, row_reader(&[]));
        // Cells blank → all predictions still pending → cursor stays at (0, 3).
        assert_eq!(s.cursor(), (0, 3));
        assert_eq!(s.pending_len(), 3);
    }

    use crate::predict::state::PredictionOutcome;

    // -- cursor-motion arrows (phux-9gw.1.3) ----------------------------

    #[test]
    fn per_cell_cursor_left_confirmed_when_cursor_matches() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 5);
        let arrow = key_named(PhysicalKey::ArrowLeft, ModSet::empty());
        let outcome =
            s.predict_key_with_grid(
                &arrow,
                |r, c| {
                    if (r, c) == (0, 4) { Some('a') } else { None }
                },
            );
        assert_eq!(outcome, PredictionOutcome::Predicted);
        // Server catches up: cursor now at (0, 4).
        let summary = reconcile_terminal_output_per_cell(&mut s, 0, 4, row_reader(&[]));
        assert_eq!(summary.confirmed, 1);
        assert_eq!(s.pending_len(), 0);
        assert_eq!(s.cursor(), (0, 4));
    }

    #[test]
    fn per_cell_cursor_left_pending_when_server_lags() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 5);
        let arrow = key_named(PhysicalKey::ArrowLeft, ModSet::empty());
        s.predict_key_with_grid(
            &arrow,
            |r, c| {
                if (r, c) == (0, 4) { Some('a') } else { None }
            },
        );
        // Server hasn't applied the motion yet — cursor still at (0, 5).
        let summary = reconcile_terminal_output_per_cell(&mut s, 0, 5, row_reader(&[]));
        assert_eq!(summary.pending, 1);
        assert_eq!(s.pending_len(), 1);
        // Predict-side cursor stays at the predicted target.
        assert_eq!(s.cursor(), (0, 4));
    }

    #[test]
    fn per_cell_cursor_left_contradicted_when_cursor_diverges() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 5);
        let arrow = key_named(PhysicalKey::ArrowLeft, ModSet::empty());
        s.predict_key_with_grid(
            &arrow,
            |r, c| {
                if (r, c) == (0, 4) { Some('a') } else { None }
            },
        );
        // Server jumped to a different row (e.g. shell repainted prompt).
        let summary = reconcile_terminal_output_per_cell(&mut s, 1, 0, row_reader(&[]));
        assert_eq!(summary.contradicted, 1);
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn per_cell_cursor_right_confirmed_when_cursor_matches() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 3);
        let arrow = key_named(PhysicalKey::ArrowRight, ModSet::empty());
        s.predict_key_with_grid(
            &arrow,
            |r, c| {
                if (r, c) == (0, 3) { Some('x') } else { None }
            },
        );
        let summary = reconcile_terminal_output_per_cell(&mut s, 0, 4, row_reader(&[]));
        assert_eq!(summary.confirmed, 1);
        assert_eq!(s.cursor(), (0, 4));
    }

    #[test]
    fn per_cell_cursor_right_pending_when_server_lags() {
        let mut s = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        s.set_cursor(0, 3);
        let arrow = key_named(PhysicalKey::ArrowRight, ModSet::empty());
        s.predict_key_with_grid(
            &arrow,
            |r, c| {
                if (r, c) == (0, 3) { Some('x') } else { None }
            },
        );
        // Server hasn't seen the arrow yet — cursor still at (0, 3).
        let summary = reconcile_terminal_output_per_cell(&mut s, 0, 3, row_reader(&[]));
        assert_eq!(summary.pending, 1);
        assert_eq!(s.cursor(), (0, 4));
    }

    // -- adaptive tentative display (phux-pxaj, reshaped by ADR-0090) ---

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
}
