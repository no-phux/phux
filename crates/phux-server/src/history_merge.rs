//! Viewport alignment and upward merge for alternate-screen history harvest.
//!
//! # What this is
//!
//! An alternate-screen agent — Claude Code, opencode, and every other
//! full-screen TUI — keeps its transcript in its *own* buffer. Rows that
//! scroll off the alternate screen never enter the emulator's history, so no
//! value of `--scrollback` recovers them
//! ([ADR-0078](../../../ADR/0078-alternate-screen-history.md)). The only
//! mechanism that reaches those rows is the application's own scrollback,
//! driven by synthesized wheel events — a read that writes.
//!
//! That traversal is a multi-week subsystem. This module is the part of it
//! that needs no PTY at all: given the viewport before a wheel-up and the
//! viewport after it, decide **how far the screen actually moved** and which
//! rows are genuinely new. It is a pure function over rows of text, and it is
//! the piece most likely to be subtly wrong, so it is built and tested first.
//!
//! # What this is not, and why nothing calls it
//!
//! The driver — the phase machine that settles the screen, probes the bottom,
//! sends wheel events, restores the viewport, and owns the restore obligation
//! on every exit path — is **deliberately not built here**. ADR-0078 is
//! `Status: Proposed` and carries an unresolved contradiction with
//! [`docs/spec/L1.md`](../../../docs/spec/L1.md) §6.1, which today calls
//! `GET_SCREEN` side-effect-free. Nothing may scroll a live pane before that
//! sentence is amended and the ADR is accepted.
//!
//! So this module is intentionally unused: the whole file carries an
//! `allow(dead_code)` rather than being wired into the terminal actor
//! prematurely. When ADR-0078 is accepted, the driver lands beside it (the
//! design sketch places both under a `transcript/` module, this file as
//! `transcript/merge.rs`) and the allow comes off.
//!
//! # The algorithm
//!
//! Rows are the right-trimmed strings
//! [`ScreenState::lines`](phux_core::screen::ScreenState) already produces.
//! Two predicates and one search:
//!
//! - **`comparable(a, b)`** — at least one of the two rows is non-empty. A
//!   screen that is mostly blank must not score a perfect match trivially.
//! - **[`viewports_similar`]** — of the comparable row positions, at least
//!   [`SIMILAR_MIN_PERCENT`] match exactly. This is what tolerates a braille
//!   spinner and an elapsed-time counter: `worked for 2s` and `worked for 3s`
//!   are two mismatched rows on an otherwise identical screen, and a whole
//!   screen is allowed a minority of such rows before it counts as a
//!   different screen. No row is fuzzy-matched; the tolerance lives entirely
//!   in the threshold.
//! - **[`sticky_prefix`]** — the leading run of rows identical at the *same*
//!   index. A pinned header or tab bar repaints at index 0 no matter how far
//!   the content scrolled. It is excluded from both scoring and splicing,
//!   because it otherwise inflates the shift-0 agreement and, worse, gets
//!   spliced into the transcript once per step. That is precisely the case
//!   naive alignment gets wrong.
//! - **[`merge_scrolled_up`]** — search every shift for the one whose overlap
//!   agrees best, require [`ALIGN_MIN_PERCENT`] agreement, and splice only
//!   `shift` rows off the top of the new viewport.
//!
//! Ties break toward the *smallest* shift. A large shift with an equal score
//! is the "we jumped a page and got lucky on a one-row overlap" case; the
//! small shift is the conservative read. Under-splicing yields a truncated
//! transcript, which is recoverable; over-splicing fabricates rows, which is
//! not.
//!
//! # Provenance of the constants
//!
//! 0.70 and 0.30 are herdr's, adopted because they are tuned to the *agents'*
//! repaint behaviour — spinners, elapsed counters, sticky headers — which is
//! byte-identical for phux, not to herdr's architecture. Inventing different
//! numbers would be inventing without data. ADR-0046's Tradeoffs record what
//! writing thresholds against an imagined TUI cost last time, so the same
//! discipline applies here: **a threshold is only as good as the captured
//! sequence that justifies it.** The tests below are hand-built sequences that
//! pin the *behaviour* of each constant; a captured corpus from the real agent
//! CLIs is owed before ADR-0078 is accepted, and any change to a constant must
//! re-run that corpus. A number nobody can restate the provenance of gets
//! deleted, not guessed.
//!
//! # Known limitations, stated rather than hidden
//!
//! - **A repainting header is not sticky.** [`sticky_prefix`] matches exactly,
//!   so a header carrying its own spinner scores as changed content and can be
//!   spliced once per step. The merge cannot tell that row from real output.
//! - **A pinned footer costs agreement.** Rows below the sticky prefix are all
//!   assumed to scroll. A status line pinned to the bottom row therefore
//!   contributes one guaranteed mismatch to every overlap, which is why the
//!   seam flag exists rather than a claim of correctness.
//! - **The merge cannot prove it spliced correctly.** `seam` marks a splice
//!   accepted below [`SEAM_CONFIDENT_PERCENT`]; ADR-0078 decision 6 returns the
//!   count of those to the caller, because countable is the strongest honest
//!   claim available.

#![allow(
    dead_code,
    reason = "pure core of the ADR-0078 alt-screen harvest; the driver that calls it is deferred until that ADR is accepted, and nothing may scroll a live pane before then"
)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "private server module intended for the sibling terminal-actor modules"
)]

/// Minimum agreement, in percent of comparable row positions, for two
/// viewports to count as the same screen.
///
/// Used by the settle and restore phases ("wait for two consecutive similar
/// captures") and, here, to recognise that a wheel-up moved nothing.
pub(crate) const SIMILAR_MIN_PERCENT: usize = 70;

/// Minimum agreement, in percent of comparable row positions in the overlap,
/// for an alignment shift to be believed at all.
///
/// Below this the merge reports [`MergeOutcome::Unaligned`] and splices
/// nothing. Guessing here fabricates transcript rows.
pub(crate) const ALIGN_MIN_PERCENT: usize = 30;

/// Agreement at or above this makes a splice confident; below it the splice is
/// still taken but flagged as a seam.
pub(crate) const SEAM_CONFIDENT_PERCENT: usize = 60;

/// How many row positions were comparable, and how many of those matched.
///
/// Kept as an exact fraction rather than a float so that threshold tests and
/// tie-breaks are integer comparisons with no rounding to argue about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowAgreement {
    /// Row positions where both rows were equal.
    pub(crate) matched: usize,
    /// Row positions where at least one of the two rows was non-empty.
    pub(crate) compared: usize,
}

impl RowAgreement {
    /// True when `matched / compared >= percent / 100`.
    ///
    /// A zero-`compared` agreement is vacuously at least anything: there was
    /// no evidence against, which is the reading [`viewports_similar`] wants
    /// for a pair of blank screens. Callers that need *positive* evidence
    /// (the alignment search) never construct one, because a shift with no
    /// comparable position is skipped outright.
    #[must_use]
    pub(crate) const fn at_least(self, percent: usize) -> bool {
        self.matched * 100 >= self.compared * percent
    }

    /// True when `self` is a strictly better agreement than `other`.
    ///
    /// Cross-multiplied, so `3/3` and `1/1` compare equal and the caller's
    /// iteration order decides — which is how ties break toward the smallest
    /// shift in [`merge_scrolled_up`].
    #[must_use]
    pub(crate) const fn stronger_than(self, other: Self) -> bool {
        self.matched * other.compared > other.matched * self.compared
    }

    /// Agreement as a truncated percentage, for logs and payloads.
    #[must_use]
    pub(crate) const fn percent(self) -> usize {
        if self.compared == 0 {
            100
        } else {
            (self.matched * 100) / self.compared
        }
    }
}

/// What one wheel-up step yielded.
///
/// The driver's control flow depends on telling these three apart: `Advanced`
/// continues the traversal, `Unchanged` means the top of the application's own
/// scrollback was reached and the traversal is done, and `Unaligned` is a
/// failure to reconcile that must be counted toward a bounded give-up rather
/// than guessed through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MergeOutcome {
    /// The screen scrolled and `rows` are genuinely new, oldest first.
    ///
    /// `rows` is exactly the run spliced onto the front of the reconstructed
    /// history; its length is the alignment shift.
    Advanced {
        /// New rows, in screen order, to prepend to the history.
        rows: Vec<String>,
        /// Agreement over the overlap that justified this shift.
        agreement: RowAgreement,
        /// The splice was accepted below [`SEAM_CONFIDENT_PERCENT`]; the
        /// reconstruction may have a seam here.
        seam: bool,
    },
    /// The screen did not move: the two viewports are the same screen modulo
    /// spinners and counters. The application is at the top of its own
    /// scrollback (or does not route the wheel at all).
    Unchanged,
    /// No shift reconciles the two viewports. Nothing is spliced. The driver
    /// counts this and gives up after a bounded number of consecutive
    /// occurrences, returning what it has as truncated.
    Unaligned,
}

impl MergeOutcome {
    /// Rows this step contributes to the history: empty unless `Advanced`.
    #[must_use]
    pub(crate) fn new_rows(&self) -> &[String] {
        match self {
            Self::Advanced { rows, .. } => rows,
            Self::Unchanged | Self::Unaligned => &[],
        }
    }

    /// How far the screen moved, in rows. Zero unless `Advanced`.
    #[must_use]
    pub(crate) fn shift(&self) -> usize {
        self.new_rows().len()
    }

    /// True when this step was spliced with low confidence.
    #[must_use]
    pub(crate) const fn is_seam(&self) -> bool {
        matches!(self, Self::Advanced { seam: true, .. })
    }
}

/// Normalise one row for comparison.
///
/// Rows arriving from `ScreenState::lines` are already right-trimmed; trimming
/// again is cheap and keeps the module honest about what it compares when a
/// caller hands it rows from somewhere else.
fn row<S: AsRef<str> + ?Sized>(value: &S) -> &str {
    value.as_ref().trim_end()
}

/// A position counts only if at least one side has content there.
///
/// Without this, two mostly-blank screens agree perfectly and every shift
/// scores 1.0.
const fn comparable(a: &str, b: &str) -> bool {
    !a.is_empty() || !b.is_empty()
}

/// Agreement between two row runs, position by position, or `None` if no
/// position was comparable at all.
///
/// Zipping stops at the shorter run; every caller here supplies runs of equal
/// length.
fn row_agreement<S: AsRef<str>>(
    left: impl Iterator<Item = S>,
    right: impl Iterator<Item = S>,
) -> Option<RowAgreement> {
    let mut matched = 0usize;
    let mut compared = 0usize;
    for (a, b) in left.zip(right) {
        let a = row(&a);
        let b = row(&b);
        if !comparable(a, b) {
            continue;
        }
        compared += 1;
        if a == b {
            matched += 1;
        }
    }
    (compared > 0).then_some(RowAgreement { matched, compared })
}

/// Are these two captures of the same screen?
///
/// True when at least [`SIMILAR_MIN_PERCENT`] of the comparable row positions
/// match exactly, which is what lets a spinner glyph or an elapsed-seconds
/// counter change without the screen counting as different. Two blank
/// viewports are the same screen. Two viewports of different heights are not:
/// a resize mid-traversal is a give-up, not a match.
#[must_use]
pub(crate) fn viewports_similar<S: AsRef<str>>(a: &[S], b: &[S]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    row_agreement(a.iter(), b.iter()).is_none_or(|found| found.at_least(SIMILAR_MIN_PERCENT))
}

/// Length of the leading run of rows that are identical at the same index.
///
/// This is the pinned header / tab bar: it repaints at a fixed index no matter
/// how far the content below it scrolled. [`merge_scrolled_up`] excludes it
/// from both scoring and splicing.
#[must_use]
pub(crate) fn sticky_prefix<S: AsRef<str>>(a: &[S], b: &[S]) -> usize {
    a.iter()
        .zip(b.iter())
        .take_while(|(left, right)| row(left) == row(right))
        .count()
}

/// Reconcile `next` — a viewport captured after scrolling *up* from `prev` —
/// against `prev`, and report only the rows that are genuinely new.
///
/// `prev` and `next` must be full viewports of the same height; anything else
/// is [`MergeOutcome::Unaligned`]. The returned rows are right-trimmed and in
/// screen order, ready to prepend to the reconstructed history with
/// [`splice_front`].
///
/// This function has no side effects and touches no history: the caller owns
/// the accumulator, so a step that turns out to be wrong costs nothing.
#[must_use]
pub(crate) fn merge_scrolled_up<S: AsRef<str>>(prev: &[S], next: &[S]) -> MergeOutcome {
    if prev.is_empty() || next.is_empty() || prev.len() != next.len() {
        return MergeOutcome::Unaligned;
    }
    if viewports_similar(prev, next) {
        return MergeOutcome::Unchanged;
    }

    let rows = prev.len();
    let sticky = sticky_prefix(prev, next);
    if sticky >= rows {
        // Unreachable in practice: an all-identical pair is `similar` above.
        return MergeOutcome::Unchanged;
    }

    // `shift` rows scrolled in at the top of `next`, below the sticky prefix.
    // The remaining `overlap` rows of `next` should reproduce the top of
    // `prev`. Ascending order plus a strict comparison breaks ties toward the
    // smallest shift.
    let mut best: Option<(RowAgreement, usize)> = None;
    for shift in 1..=(rows - sticky) {
        let overlap = rows - sticky - shift;
        if overlap == 0 {
            continue;
        }
        let Some(found) = row_agreement(
            next[sticky + shift..sticky + shift + overlap].iter(),
            prev[sticky..sticky + overlap].iter(),
        ) else {
            // Every position in this overlap was blank on both sides. That is
            // no evidence at all, not perfect evidence.
            continue;
        };
        if best.is_none_or(|(current, _)| found.stronger_than(current)) {
            best = Some((found, shift));
        }
    }

    let Some((agreement, shift)) = best else {
        return MergeOutcome::Unaligned;
    };
    if !agreement.at_least(ALIGN_MIN_PERCENT) {
        return MergeOutcome::Unaligned;
    }

    MergeOutcome::Advanced {
        rows: next[sticky..sticky + shift]
            .iter()
            .map(|value| row(value).to_owned())
            .collect(),
        agreement,
        seam: !agreement.at_least(SEAM_CONFIDENT_PERCENT),
    }
}

/// Prepend a step's new rows to the reconstructed history, preserving order.
///
/// The traversal walks *backwards* through the transcript, so each step's rows
/// belong in front of everything collected so far while staying in screen
/// order among themselves.
pub(crate) fn splice_front(history: &mut Vec<String>, rows: Vec<String>) {
    history.splice(0..0, rows);
}

#[cfg(test)]
mod tests {
    use super::{
        MergeOutcome, RowAgreement, merge_scrolled_up, splice_front, sticky_prefix,
        viewports_similar,
    };

    /// Hand-built viewports are written as `&[&str]`; the merge operates on
    /// anything `AsRef<str>`, so no conversion is needed on the way in.
    fn owned(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|row| (*row).to_owned()).collect()
    }

    // ---- similarity -------------------------------------------------------

    #[test]
    fn similar_tolerates_a_spinner_row() {
        let before = ["header", "one", "two", "three", "* thinking"];
        let after = ["header", "one", "two", "three", "- thinking"];
        assert!(viewports_similar(&before, &after));
    }

    #[test]
    fn similar_tolerates_an_elapsed_counter() {
        let before = ["header", "one", "two", "worked for 2s", "esc to interrupt"];
        let after = ["header", "one", "two", "worked for 3s", "esc to interrupt"];
        assert!(viewports_similar(&before, &after));
    }

    #[test]
    fn similar_rejects_a_screen_that_actually_scrolled() {
        let before = ["one", "two", "three", "four"];
        let after = ["minus", "zero", "one", "two"];
        assert!(!viewports_similar(&before, &after));
    }

    #[test]
    fn similar_rejects_a_change_past_the_threshold() {
        // Two of five rows change: 60 percent agreement, below 70.
        let before = ["a", "b", "c", "d", "e"];
        let after = ["a", "b", "c", "X", "Y"];
        assert!(!viewports_similar(&before, &after));
    }

    #[test]
    fn similar_accepts_a_change_at_the_threshold() {
        // Three of ten rows change: exactly 70 percent agreement.
        let before = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"];
        let after = ["a", "b", "c", "d", "e", "f", "g", "X", "Y", "Z"];
        assert!(viewports_similar(&before, &after));
    }

    #[test]
    fn similar_treats_two_blank_viewports_as_the_same_screen() {
        let before = ["", "", ""];
        let after = ["", "", ""];
        assert!(viewports_similar(&before, &after));
    }

    #[test]
    fn similar_ignores_positions_blank_on_both_sides() {
        // Only index 0 is comparable, and it matches, so the trailing blanks
        // neither help nor hurt.
        let before = ["only", "", "", ""];
        let after = ["only", "", "", ""];
        assert!(viewports_similar(&before, &after));
    }

    #[test]
    fn similar_rejects_a_resize() {
        let before = ["a", "b", "c"];
        let after = ["a", "b", "c", "d"];
        assert!(!viewports_similar(&before, &after));
    }

    #[test]
    fn similar_ignores_trailing_whitespace() {
        let before = ["header   ", "body"];
        let after = ["header", "body  "];
        assert!(viewports_similar(&before, &after));
    }

    // ---- sticky prefix ----------------------------------------------------

    #[test]
    fn sticky_prefix_counts_only_the_leading_identical_run() {
        let before = ["tab bar", "path: /x", "one", "two"];
        let after = ["tab bar", "path: /x", "minus", "zero"];
        assert_eq!(sticky_prefix(&before, &after), 2);
    }

    #[test]
    fn sticky_prefix_is_zero_when_the_first_row_differs() {
        let before = ["one", "two"];
        let after = ["zero", "one"];
        assert_eq!(sticky_prefix(&before, &after), 0);
    }

    #[test]
    fn sticky_prefix_does_not_run_past_a_reappearing_match() {
        // Row 2 matches again by coincidence; the run stopped at row 1.
        let before = ["hdr", "one", "same", "two"];
        let after = ["hdr", "zero", "same", "one"];
        assert_eq!(sticky_prefix(&before, &after), 1);
    }

    // ---- the merge --------------------------------------------------------

    #[test]
    fn clean_scroll_splices_exactly_the_new_rows() {
        let prev = ["l03", "l04", "l05", "l06", "l07"];
        let next = ["l00", "l01", "l02", "l03", "l04"];
        let outcome = merge_scrolled_up(&prev, &next);
        assert_eq!(outcome.new_rows(), owned(&["l00", "l01", "l02"]).as_slice());
        assert_eq!(outcome.shift(), 3);
        assert!(!outcome.is_seam());
        assert!(matches!(
            outcome,
            MergeOutcome::Advanced {
                agreement: RowAgreement {
                    matched: 2,
                    compared: 2
                },
                ..
            }
        ));
    }

    #[test]
    fn sticky_header_is_not_duplicated_into_history() {
        // The header repaints at index 0 on every page. A merge that scored
        // and spliced from index 0 would emit it as a new row every step.
        let prev = ["== claude ==", "l03", "l04", "l05", "l06"];
        let next = ["== claude ==", "l00", "l01", "l02", "l03"];
        let outcome = merge_scrolled_up(&prev, &next);
        assert_eq!(outcome.new_rows(), owned(&["l00", "l01", "l02"]).as_slice());
        assert!(
            !outcome.new_rows().contains(&"== claude ==".to_owned()),
            "the sticky header must never be spliced as transcript content"
        );
    }

    #[test]
    fn sticky_header_case_survives_repeated_steps_without_growing_a_header_run() {
        // Four steps down the same page; the header appears in none of them.
        let pages = [
            ["== claude ==", "l09", "l10", "l11", "l12"],
            ["== claude ==", "l06", "l07", "l08", "l09"],
            ["== claude ==", "l03", "l04", "l05", "l06"],
            ["== claude ==", "l00", "l01", "l02", "l03"],
        ];
        let mut history: Vec<String> = Vec::new();
        for pair in pages.windows(2) {
            let outcome = merge_scrolled_up(&pair[0], &pair[1]);
            assert_eq!(outcome.shift(), 3, "expected a three-row step");
            splice_front(&mut history, outcome.new_rows().to_vec());
        }
        assert_eq!(
            history,
            owned(&[
                "l00", "l01", "l02", "l03", "l04", "l05", "l06", "l07", "l08"
            ])
        );
    }

    #[test]
    fn multi_row_sticky_prefix_is_excluded_from_scoring_and_splicing() {
        let prev = ["== claude ==", "cwd: /repo", "l04", "l05", "l06", "l07"];
        let next = ["== claude ==", "cwd: /repo", "l02", "l03", "l04", "l05"];
        let outcome = merge_scrolled_up(&prev, &next);
        assert_eq!(outcome.new_rows(), owned(&["l02", "l03"]).as_slice());
    }

    #[test]
    fn spinner_only_change_is_unchanged_not_advanced() {
        // The pane is at the top of its own scrollback: the wheel moved
        // nothing and only the spinner repainted.
        let prev = ["header", "l00", "l01", "l02", "* working"];
        let next = ["header", "l00", "l01", "l02", "- working"];
        assert_eq!(merge_scrolled_up(&prev, &next), MergeOutcome::Unchanged);
    }

    #[test]
    fn elapsed_counter_only_change_is_unchanged() {
        let prev = ["header", "l00", "l01", "worked for 12s (esc to interrupt)"];
        let next = ["header", "l00", "l01", "worked for 13s (esc to interrupt)"];
        assert_eq!(merge_scrolled_up(&prev, &next), MergeOutcome::Unchanged);
    }

    #[test]
    fn identical_viewports_are_unchanged() {
        let prev = ["a", "b", "c"];
        assert_eq!(merge_scrolled_up(&prev, &prev), MergeOutcome::Unchanged);
    }

    #[test]
    fn completely_unaligned_pair_gives_up() {
        let prev = ["alpha", "bravo", "charlie", "delta"];
        let next = ["one", "two", "three", "four"];
        assert_eq!(merge_scrolled_up(&prev, &next), MergeOutcome::Unaligned);
    }

    #[test]
    fn a_full_repaint_with_no_overlap_gives_up_rather_than_guessing() {
        // A modal opened over the pane: nothing survives from the old screen.
        let prev = ["l00", "l01", "l02", "l03", "l04", "l05"];
        let next = [
            "+--------+",
            "| choose |",
            "| a      |",
            "| b      |",
            "+--------+",
            "",
        ];
        assert_eq!(merge_scrolled_up(&prev, &next), MergeOutcome::Unaligned);
    }

    #[test]
    fn single_row_overlap_still_aligns() {
        // The application jumped almost a full page: exactly one row of `prev`
        // survives at the bottom of `next`.
        let prev = ["l05", "l06", "l07", "l08", "l09", "l10"];
        let next = ["l00", "l01", "l02", "l03", "l04", "l05"];
        let outcome = merge_scrolled_up(&prev, &next);
        assert_eq!(
            outcome.new_rows(),
            owned(&["l00", "l01", "l02", "l03", "l04"]).as_slice()
        );
        assert_eq!(outcome.shift(), 5);
    }

    #[test]
    fn ties_break_toward_the_smallest_shift() {
        // Periodic content: shift 1 agrees 3/3 and shift 3 agrees 1/1. Both
        // are ratio 1.0; the conservative read is the small shift, which
        // under-splices rather than fabricating four rows.
        let prev = ["A", "B", "A", "B"];
        let next = ["C", "A", "B", "A"];
        let outcome = merge_scrolled_up(&prev, &next);
        assert_eq!(outcome.new_rows(), owned(&["C"]).as_slice());
    }

    #[test]
    fn low_agreement_splice_is_flagged_as_a_seam() {
        // Two of five overlap rows agree: 40 percent, above ALIGN_MIN (30) and
        // below SEAM_CONFIDENT (60).
        let prev = ["l00", "l01", "l02", "l03", "l04", "l05"];
        let next = ["new", "l00", "l01", "X", "Y", "Z"];
        let outcome = merge_scrolled_up(&prev, &next);
        assert_eq!(outcome.new_rows(), owned(&["new"]).as_slice());
        assert!(outcome.is_seam());
        assert!(matches!(
            outcome,
            MergeOutcome::Advanced {
                agreement: RowAgreement {
                    matched: 2,
                    compared: 5
                },
                ..
            }
        ));
    }

    #[test]
    fn confident_splice_is_not_flagged_as_a_seam() {
        let prev = ["l02", "l03", "l04", "l05", "l06"];
        let next = ["l00", "l01", "l02", "l03", "l04"];
        let outcome = merge_scrolled_up(&prev, &next);
        assert_eq!(outcome.shift(), 2);
        assert!(!outcome.is_seam());
    }

    #[test]
    fn agreement_below_align_min_splices_nothing() {
        // The best shift agrees 1 of 5: 20 percent, under ALIGN_MIN.
        let prev = ["l00", "l01", "l02", "l03", "l04", "l05"];
        let next = ["new", "l00", "P", "Q", "R", "S"];
        assert_eq!(merge_scrolled_up(&prev, &next), MergeOutcome::Unaligned);
    }

    #[test]
    fn all_blank_overlap_is_skipped_not_scored_as_a_perfect_match() {
        // At shift 2 the overlap is blank on both sides. Scoring that as 1.0
        // would splice two fabricated rows off the top of `next`.
        let prev = ["", "", "x", "y"];
        let next = ["p", "q", "", ""];
        assert_eq!(merge_scrolled_up(&prev, &next), MergeOutcome::Unaligned);
    }

    #[test]
    fn blank_rows_alone_never_justify_an_alignment() {
        let prev = ["a", "", "", ""];
        let next = ["b", "", "", ""];
        assert_eq!(merge_scrolled_up(&prev, &next), MergeOutcome::Unaligned);
    }

    #[test]
    fn mismatched_viewport_heights_are_unaligned() {
        let prev = ["a", "b", "c"];
        let next = ["z", "a", "b", "c"];
        assert_eq!(merge_scrolled_up(&prev, &next), MergeOutcome::Unaligned);
    }

    #[test]
    fn empty_viewports_are_unaligned() {
        let empty: [&str; 0] = [];
        let full = ["a"];
        assert_eq!(merge_scrolled_up(&empty, &empty), MergeOutcome::Unaligned);
        assert_eq!(merge_scrolled_up(&empty, &full), MergeOutcome::Unaligned);
        assert_eq!(merge_scrolled_up(&full, &empty), MergeOutcome::Unaligned);
    }

    #[test]
    fn advanced_rows_are_right_trimmed() {
        let prev = ["l02", "l03", "l04"];
        let next = ["l00   ", "l01\t", "l02"];
        let outcome = merge_scrolled_up(&prev, &next);
        assert_eq!(outcome.new_rows(), owned(&["l00", "l01"]).as_slice());
    }

    #[test]
    fn pinned_footer_costs_agreement_and_is_reported_as_a_seam() {
        // A status line pinned to the bottom row does not scroll, so it lands
        // in the overlap as a guaranteed mismatch. The shift is still right;
        // the confidence is honestly lower.
        let prev = ["== claude ==", "l16", "l17", "l18", "l19", "worked for 4s"];
        let next = ["== claude ==", "l13", "l14", "l15", "l16", "worked for 5s"];
        let outcome = merge_scrolled_up(&prev, &next);
        assert_eq!(outcome.new_rows(), owned(&["l13", "l14", "l15"]).as_slice());
        assert!(
            outcome.is_seam(),
            "one of two overlap rows agreeing is a 50 percent seam"
        );
    }

    // ---- accumulation -----------------------------------------------------

    #[test]
    fn splice_front_prepends_in_screen_order() {
        let mut history = owned(&["l03", "l04"]);
        splice_front(&mut history, owned(&["l00", "l01", "l02"]));
        assert_eq!(history, owned(&["l00", "l01", "l02", "l03", "l04"]));
    }

    #[test]
    fn splice_front_of_nothing_is_a_no_op() {
        let mut history = owned(&["l00"]);
        splice_front(&mut history, Vec::new());
        assert_eq!(history, owned(&["l00"]));
    }

    #[test]
    fn a_whole_traversal_reconstructs_the_transcript_in_order() {
        // Eight-row viewport: a sticky header plus seven content rows, walked
        // upward three rows at a time over a twenty-line transcript, ending
        // with the top page repeated because the wheel has nowhere left to go.
        let transcript: Vec<String> = (0..20).map(|n| format!("l{n:02}")).collect();
        let header = "== claude ==".to_owned();
        let page = |top: usize| -> Vec<String> {
            let mut rows = vec![header.clone()];
            rows.extend_from_slice(&transcript[top..top + 7]);
            rows
        };

        let captures = [
            page(13),
            page(10),
            page(7),
            page(4),
            page(1),
            page(0),
            page(0),
        ];

        // The driver seeds history with the settle capture minus its sticky
        // header, then prepends each step's new rows.
        let mut history: Vec<String> = captures[0][1..].to_vec();
        let mut outcomes = Vec::new();
        for pair in captures.windows(2) {
            let outcome = merge_scrolled_up(pair[0].as_slice(), pair[1].as_slice());
            splice_front(&mut history, outcome.new_rows().to_vec());
            outcomes.push(outcome);
        }

        assert_eq!(history, transcript, "the full transcript, in order");
        let shifts: Vec<usize> = outcomes.iter().map(MergeOutcome::shift).collect();
        assert_eq!(shifts, vec![3, 3, 3, 3, 1, 0]);
        assert_eq!(
            outcomes.last(),
            Some(&MergeOutcome::Unchanged),
            "the repeated top page is how the driver learns it is done"
        );
        assert!(
            !outcomes.iter().any(MergeOutcome::is_seam),
            "a clean traversal should splice with confidence throughout"
        );
    }

    #[test]
    fn an_unaligned_step_contributes_nothing_to_history() {
        let mut history = owned(&["l05", "l06"]);
        let outcome = merge_scrolled_up(&["l05", "l06", "l07"], &["totally", "different", "rows"]);
        assert_eq!(outcome, MergeOutcome::Unaligned);
        splice_front(&mut history, outcome.new_rows().to_vec());
        assert_eq!(history, owned(&["l05", "l06"]));
    }

    // ---- agreement arithmetic --------------------------------------------

    #[test]
    fn agreement_thresholds_are_inclusive_and_exact() {
        let seventy = RowAgreement {
            matched: 7,
            compared: 10,
        };
        assert!(seventy.at_least(70));
        assert!(!seventy.at_least(71));
        assert_eq!(seventy.percent(), 70);

        let two_thirds = RowAgreement {
            matched: 2,
            compared: 3,
        };
        assert!(two_thirds.at_least(66));
        assert!(!two_thirds.at_least(67), "no rounding up at the threshold");
        assert_eq!(two_thirds.percent(), 66);
    }

    #[test]
    fn agreement_with_nothing_comparable_is_vacuously_satisfied() {
        let nothing = RowAgreement {
            matched: 0,
            compared: 0,
        };
        assert!(nothing.at_least(100));
        assert_eq!(nothing.percent(), 100);
    }

    #[test]
    fn agreement_ordering_prefers_the_higher_ratio_and_calls_equal_ratios_a_tie() {
        let three_of_three = RowAgreement {
            matched: 3,
            compared: 3,
        };
        let one_of_one = RowAgreement {
            matched: 1,
            compared: 1,
        };
        let two_of_three = RowAgreement {
            matched: 2,
            compared: 3,
        };
        assert!(!three_of_three.stronger_than(one_of_one));
        assert!(!one_of_one.stronger_than(three_of_three));
        assert!(three_of_three.stronger_than(two_of_three));
        assert!(!two_of_three.stronger_than(three_of_three));
    }
}
