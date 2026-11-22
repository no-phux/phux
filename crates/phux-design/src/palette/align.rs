//! Optimal alignment of one query word inside a title.
//!
//! A greedy left-to-right match lights the first `t`, `a`, `b` it meets, so
//! "tab" scatters over "Toggle a sidebar" instead of landing on "Tabs". This
//! finds the subsequence placement with the best score in `O(word × title)`:
//! letters that start the title or a word, or continue a run, earn bonuses;
//! gaps between them cost.

/// Every matched character.
const MATCH: i32 = 16;
/// A match at the very first character.
const BONUS_START: i32 = 32;
/// A match right after a separator: the start of a word.
const BONUS_BOUNDARY: i32 = 24;
/// A match on a lower-to-upper (or letter-to-digit) hump.
const BONUS_CAMEL: i32 = 20;
/// A match directly after the previous one.
const BONUS_CONSECUTIVE: i32 = 18;
/// Opening a gap between two matched characters.
const GAP_OPEN: i32 = -6;
/// Each further skipped character in that gap.
const GAP_EXTEND: i32 = -1;
/// Each character skipped before the first match, capped by [`LEAD_CAP`].
const LEAD: i32 = -1;
/// The most a late start can cost: a word deep in a title still counts.
const LEAD_CAP: i32 = -8;

/// No placement: low enough to never win, far enough from `i32::MIN` that
/// adding a few bonuses cannot overflow.
const NONE: i32 = i32::MIN / 2;

/// Characters that end a word in a title.
const fn is_separator(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '-' | '_' | '/' | '.' | ':' | '(' | ')' | ',' | '…' | '·' | '@'
        )
}

/// What placing a match at `i` is worth beyond [`MATCH`].
fn position_bonus(chars: &[char], i: usize) -> i32 {
    let Some(prev) = i.checked_sub(1).map(|p| chars[p]) else {
        return BONUS_START;
    };
    let cur = chars[i];
    if is_separator(prev) {
        BONUS_BOUNDARY
    } else if (prev.is_lowercase() && cur.is_uppercase())
        || (prev.is_alphabetic() && cur.is_ascii_digit())
    {
        BONUS_CAMEL
    } else {
        0
    }
}

/// Whether query char `q` matches title char `t`. A query with an upper-case
/// letter is case-sensitive (smart case); otherwise case is ignored.
fn same(q: char, t: char, case_sensitive: bool) -> bool {
    if case_sensitive {
        q == t
    } else {
        q == t || t.to_lowercase().eq(std::iter::once(q))
    }
}

/// The best placement of `word` in `title`: its score and the char indices
/// it lit, in order. `None` when `word` is not a subsequence of `title`.
pub(super) fn align(
    word: &[char],
    title: &[char],
    case_sensitive: bool,
) -> Option<(i32, Vec<usize>)> {
    let (m, n) = (word.len(), title.len());
    if m == 0 {
        return Some((0, Vec::new()));
    }
    if m > n {
        return None;
    }
    // score[j][i]: the best total with word[j] placed at title[i].
    // from[j][i]: where word[j - 1] sat on that best path.
    let mut score = vec![vec![NONE; n]; m];
    let mut from = vec![vec![usize::MAX; n]; m];
    for i in 0..n {
        if same(word[0], title[i], case_sensitive) {
            let lead = (LEAD * i32::try_from(i).unwrap_or(i32::MAX)).max(LEAD_CAP);
            score[0][i] = MATCH + position_bonus(title, i) + lead;
        }
    }
    for j in 1..m {
        // The best predecessor at least one character back, gap cost included:
        // extended by one each step, opened afresh from `i - 2`.
        let mut open = NONE;
        let mut open_at = usize::MAX;
        for i in j..n {
            if i >= 2 {
                let extended = open.saturating_add(GAP_EXTEND);
                let opened = score[j - 1][i - 2].saturating_add(GAP_OPEN);
                if opened >= extended {
                    (open, open_at) = (opened, i - 2);
                } else {
                    open = extended;
                }
            }
            if !same(word[j], title[i], case_sensitive) {
                continue;
            }
            let run = score[j - 1][i - 1].saturating_add(BONUS_CONSECUTIVE);
            let (best, prev) = if run >= open {
                (run, i - 1)
            } else {
                (open, open_at)
            };
            if best <= NONE / 2 {
                continue;
            }
            score[j][i] = best + MATCH + position_bonus(title, i);
            from[j][i] = prev;
        }
    }
    let (mut at, total) = score[m - 1]
        .iter()
        .enumerate()
        .filter(|(_, s)| **s > NONE / 2)
        .max_by(|(ia, a), (ib, b)| a.cmp(b).then(ib.cmp(ia)))
        .map(|(i, s)| (i, *s))?;
    let mut positions = vec![0; m];
    for j in (0..m).rev() {
        positions[j] = at;
        at = from[j][at];
    }
    Some((total, positions))
}
