//! Vi cursor motions within one terminal row, shared by every client's
//! keyboard copy mode: the row's cells in, the target column out.

/// A cursor move within one row that depends on the row's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowMotion {
    /// `w`: the start of the next word.
    WordForward,
    /// `b`: the start of this word, or of the one before.
    WordBack,
    /// `e`: the end of this word, or of the next.
    WordEnd,
    /// `$`: the last cell with text.
    LineEnd,
    /// `^`: the first cell with text.
    FirstNonBlank,
}

/// Vi's three character classes: blank, word (`[A-Za-z0-9_]` and any other
/// alphanumeric), punctuation.
fn class(codepoint: u32) -> u8 {
    match char::from_u32(codepoint) {
        None | Some('\0') => 0,
        Some(c) if c.is_whitespace() => 0,
        Some(c) if c.is_alphanumeric() || c == '_' => 1,
        Some(_) => 2,
    }
}

/// The column `motion` lands on from `col` in a row whose cells hold
/// `cells` (one codepoint per cell, `0` for an empty cell).
// ponytail: motions stop at the row's ends rather than wrapping to the
// next or previous row as vi does; `j`/`k` cross rows. Wrap here if asked.
#[must_use]
pub fn row_motion_target(cells: &[u32], col: u16, motion: RowMotion) -> u16 {
    let classes: Vec<u8> = cells.iter().map(|&cell| class(cell)).collect();
    let n = classes.len();
    let at = usize::from(col).min(n.saturating_sub(1));
    let last_text = classes.iter().rposition(|&c| c != 0).unwrap_or(0);
    let target = match motion {
        RowMotion::LineEnd => last_text,
        RowMotion::FirstNonBlank => classes.iter().position(|&c| c != 0).unwrap_or(0),
        RowMotion::WordForward => {
            let mut i = at;
            while i < n && classes[i] == classes[at] {
                i += 1;
            }
            while i < n && classes[i] == 0 {
                i += 1;
            }
            if i < n { i } else { last_text.max(at) }
        }
        RowMotion::WordEnd => {
            let mut i = at + 1;
            while i < n && classes[i] == 0 {
                i += 1;
            }
            if i >= n {
                at
            } else {
                while i + 1 < n && classes[i + 1] == classes[i] {
                    i += 1;
                }
                i
            }
        }
        RowMotion::WordBack => {
            let mut i = at.saturating_sub(1);
            while i > 0 && classes.get(i) == Some(&0) {
                i -= 1;
            }
            while i > 0 && classes[i - 1] == classes[i] && classes[i] != 0 {
                i -= 1;
            }
            i
        }
    };
    u16::try_from(target).unwrap_or(u16::MAX)
}

/// The first and last column of the word (run of one character class) under
/// `col`: what a copy with nothing selected takes.
#[must_use]
pub fn word_span(cells: &[u32], col: u16) -> (u16, u16) {
    let at = usize::from(col);
    let Some(&here) = cells.get(at) else {
        return (col, col);
    };
    let same = |cell: &&u32| class(**cell) == class(here);
    let start = at - cells[..at].iter().rev().take_while(same).count();
    let end = at + cells[at + 1..].iter().take_while(same).count();
    (
        u16::try_from(start).unwrap_or(col),
        u16::try_from(end).unwrap_or(col),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(row: &str) -> Vec<u32> {
        row.chars()
            .map(|c| if c == ' ' { 0 } else { u32::from(c) })
            .collect()
    }

    fn target(row: &str, col: u16, motion: RowMotion) -> u16 {
        row_motion_target(&cells(row), col, motion)
    }

    #[test]
    fn word_motions_follow_vi_word_classes_within_the_row() {
        //         0123456789012345678
        let row = "  foo bar.baz  qux ";
        assert_eq!(target(row, 0, RowMotion::FirstNonBlank), 2);
        assert_eq!(target(row, 0, RowMotion::LineEnd), 17);
        // w: next word start; punctuation is its own word.
        assert_eq!(target(row, 2, RowMotion::WordForward), 6);
        assert_eq!(target(row, 6, RowMotion::WordForward), 9);
        assert_eq!(target(row, 9, RowMotion::WordForward), 10);
        assert_eq!(target(row, 0, RowMotion::WordForward), 2);
        // The last word has nowhere to go but its own end.
        assert_eq!(target(row, 15, RowMotion::WordForward), 17);
        // e: end of this word, then of the next.
        assert_eq!(target(row, 2, RowMotion::WordEnd), 4);
        assert_eq!(target(row, 4, RowMotion::WordEnd), 8);
        assert_eq!(target(row, 17, RowMotion::WordEnd), 17);
        // b: start of this word, then of the one before.
        assert_eq!(target(row, 8, RowMotion::WordBack), 6);
        assert_eq!(target(row, 6, RowMotion::WordBack), 2);
        assert_eq!(target(row, 15, RowMotion::WordBack), 10);
        assert_eq!(target(row, 2, RowMotion::WordBack), 0);
        // A blank or empty row leaves the cursor at the left edge.
        assert_eq!(target("    ", 2, RowMotion::LineEnd), 0);
        assert_eq!(row_motion_target(&[], 3, RowMotion::WordForward), 0);
    }

    #[test]
    fn word_span_covers_the_run_under_the_cursor() {
        let row = cells("  foo bar.baz");
        assert_eq!(word_span(&row, 3), (2, 4));
        assert_eq!(word_span(&row, 9), (9, 9));
        assert_eq!(word_span(&row, 0), (0, 1));
        assert_eq!(word_span(&row, 40), (40, 40));
    }
}
