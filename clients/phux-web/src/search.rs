//! Find in the terminal: the matches of a query over the replica's whole
//! screen (the scrollback it holds and the live area) and which one is
//! current.
//!
//! The native clients search through the runtime's tracked document anchors,
//! which need the native engine; the browser has no such runtime. It searches
//! the engine's own plain-text rendering of the screen instead
//! ([`phux_vt_web::Terminal::screen_rows`]), one row at a time: a match does
//! not span a soft-wrapped line break. Matching ignores ASCII case.

use crate::Mark;

/// The most matches one search keeps; the count reads `N+` past it.
pub const MATCH_LIMIT: usize = 1000;

/// One match: screen row `row` (0 is the oldest scrollback row), cells
/// `start..end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    /// Screen row.
    pub row: u64,
    /// First cell.
    pub start: u16,
    /// One past the last cell.
    pub end: u16,
}

/// Every match of `query` in `rows` (one string per screen row, as the
/// engine formats them), in screen order, and whether [`MATCH_LIMIT`] cut
/// the list short. `width` is the engine's cell width of a non-ASCII
/// character, so a match after a wide one lands on its real cells.
pub fn find_matches(
    rows: &[String],
    query: &str,
    width: impl Fn(char) -> u8,
) -> (Vec<Match>, bool) {
    let needle: Vec<char> = query.chars().collect();
    let mut found = Vec::new();
    if needle.is_empty() {
        return (found, false);
    }
    for (row, text) in (0_u64..).zip(rows) {
        let cells = cell_positions(text, &width);
        let mut at = 0;
        while at + needle.len() <= cells.len() {
            let window = &cells[at..at + needle.len()];
            if !window
                .iter()
                .zip(&needle)
                .all(|((ch, _, _), expected)| ch.eq_ignore_ascii_case(expected))
            {
                at += 1;
                continue;
            }
            if found.len() == MATCH_LIMIT {
                return (found, true);
            }
            let (_, start, _) = window[0];
            let (_, last, last_width) = window[needle.len() - 1];
            found.push(Match {
                row,
                start,
                end: last.saturating_add(u16::from(last_width.max(1))),
            });
            at += needle.len();
        }
    }
    (found, false)
}

/// Each character of a row with the cell it starts in and its width.
fn cell_positions(text: &str, width: &impl Fn(char) -> u8) -> Vec<(char, u16, u8)> {
    let mut col = 0_u16;
    text.chars()
        .map(|ch| {
            let cells = if ch.is_ascii() { 1 } else { width(ch) };
            let at = (ch, col, cells);
            col = col.saturating_add(u16::from(cells));
            at
        })
        .collect()
}

/// The screen row to scroll to the viewport's top so `row` shows centered,
/// or `None` when it is already on screen. `offset` and `len` are the
/// viewport's top row and height; `total` the screen's rows.
#[must_use]
pub fn reveal_row(row: u64, offset: u64, len: u64, total: u64) -> Option<u64> {
    if (offset..offset + len).contains(&row) {
        return None;
    }
    Some(row.saturating_sub(len / 2).min(total.saturating_sub(len)))
}

/// The open search: its query, matches, and the current one.
#[derive(Debug, Default)]
pub struct Search {
    query: String,
    matches: Vec<Match>,
    truncated: bool,
    current: Option<usize>,
}

impl Search {
    /// The query the matches are for.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Take a search's results. A new query starts at the newest match; the
    /// same query (re-run after new output) keeps its current match when it
    /// is still there, or the nearest index when it is not.
    pub fn set_results(&mut self, query: &str, matches: Vec<Match>, truncated: bool) {
        let same_query = self.query == query;
        let previous = self.current();
        self.current = if matches.is_empty() {
            None
        } else if !same_query {
            Some(matches.len() - 1)
        } else {
            previous
                .and_then(|kept| matches.iter().position(|found| *found == kept))
                .or_else(|| Some(self.current.unwrap_or(usize::MAX).min(matches.len() - 1)))
        };
        query.clone_into(&mut self.query);
        self.matches = matches;
        self.truncated = truncated;
    }

    /// Forget the query and its matches.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The current match.
    #[must_use]
    pub fn current(&self) -> Option<Match> {
        self.matches.get(self.current?).copied()
    }

    /// Move to the next match `older` (up the screen) or newer, wrapping at
    /// either end, and return it.
    pub fn step(&mut self, older: bool) -> Option<Match> {
        let len = self.matches.len();
        let current = self.current?;
        self.current = Some(if older {
            current.checked_sub(1).unwrap_or(len - 1)
        } else {
            (current + 1) % len
        });
        self.current()
    }

    /// The status line: `3 of 17`, `No matches`, or empty with no query.
    #[must_use]
    pub fn label(&self) -> String {
        if self.query.is_empty() {
            return String::new();
        }
        let Some(current) = self.current else {
            return "No matches".to_owned();
        };
        let more = if self.truncated { "+" } else { "" };
        format!("{} of {}{more}", current + 1, self.matches.len())
    }

    /// Highlights for the matches on a viewport of `rows` x `cols` cells
    /// whose top is screen row `top`.
    #[must_use]
    pub fn marks(&self, top: u64, rows: u16, cols: u16) -> Vec<Mark> {
        let visible = top..top + u64::from(rows);
        let current = self.current();
        self.matches
            .iter()
            .filter(|found| visible.contains(&found.row))
            .map(|found| {
                let base = usize::try_from(found.row - top).unwrap_or(0) * usize::from(cols);
                let end = found.end.min(cols);
                Mark {
                    cells: base + usize::from(found.start.min(end))..base + usize::from(end),
                    current: current == Some(*found),
                }
            })
            .collect()
    }
}
