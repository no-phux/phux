//! The doctor report as text. One renderer draws both the check rows and
//! the "What works here" checklist: a status mark, an aligned name column,
//! the detail, then any hints or fixes indented under the detail. Only the
//! mark is coloured.

use std::fmt::{Display, Write as _};

use anstyle::AnsiColor;

/// A row's status mark and the colour it wears when colour is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Mark {
    glyph: char,
    color: Option<AnsiColor>,
}

impl Mark {
    /// A check that passed, or a capability that works.
    pub(super) const OK: Self = Self::new('✓', Some(AnsiColor::Green));
    /// A check that could not be verified.
    pub(super) const WARN: Self = Self::new('!', Some(AnsiColor::Yellow));
    /// A check that failed.
    pub(super) const FAIL: Self = Self::new('✗', Some(AnsiColor::Red));
    /// A capability that is simply not turned on: neutral, never red.
    pub(super) const OFF: Self = Self::new('–', None);
    /// A capability that will start working without help.
    pub(super) const WAITING: Self = Self::new('…', Some(AnsiColor::Cyan));
    /// A capability that could not be checked.
    pub(super) const UNKNOWN: Self = Self::new('?', Some(AnsiColor::Yellow));

    const fn new(glyph: char, color: Option<AnsiColor>) -> Self {
        Self { glyph, color }
    }

    fn paint(self, color: bool) -> String {
        let Some(ansi) = self.color.filter(|_| color) else {
            return self.glyph.to_string();
        };
        let style = anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(ansi)));
        format!("{style}{}{style:#}", self.glyph)
    }
}

/// One row of a report section.
#[derive(Debug)]
pub(super) struct Row<'a> {
    pub(super) mark: Mark,
    pub(super) name: &'a str,
    pub(super) detail: &'a str,
    /// What each note line is (`hint`, `fix`).
    pub(super) label: &'static str,
    /// One line each, printed under the detail.
    pub(super) notes: Vec<String>,
}

/// `rows` as text, the name column as wide as the longest name.
pub(super) fn rows(rows: &[Row<'_>], color: bool) -> String {
    let width = rows
        .iter()
        .map(|row| row.name.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for row in rows {
        write_row(&mut out, row, width, color);
    }
    out
}

fn write_row(out: &mut String, row: &Row<'_>, width: usize, color: bool) {
    let mark = row.mark.paint(color);
    let _ = writeln!(out, "{mark} {:<width$}  {}", row.name, row.detail);
    // Notes start under the detail: the mark, a space, the name column and
    // its two-space gutter.
    let indent = width + 4;
    for note in &row.notes {
        let _ = writeln!(out, "{:indent$}{}: {}", "", row.label, note.trim_start());
    }
}

/// `n` and the noun that agrees with it: `1 session`, `2 sessions`.
pub(super) fn count<N>(n: N, one: &str, many: &str) -> String
where
    N: Copy + Display + PartialEq + From<u8>,
{
    if n == N::from(1) {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

#[cfg(test)]
mod tests {
    use super::{Mark, Row, count, rows};

    fn row<'a>(mark: Mark, name: &'a str, detail: &'a str, notes: &[&str]) -> Row<'a> {
        Row {
            mark,
            name,
            detail,
            label: "hint",
            notes: notes.iter().map(|note| (*note).to_owned()).collect(),
        }
    }

    #[test]
    fn rows_align_details_and_indent_notes_under_them() {
        let text = rows(
            &[
                row(Mark::OK, "config", "fine", &[]),
                row(Mark::WARN, "workload-authority", "missing", &["init it"]),
            ],
            false,
        );
        let under_detail = " ".repeat("! workload-authority  ".len());
        assert_eq!(
            text,
            format!(
                "✓ config              fine\n\
                 ! workload-authority  missing\n\
                 {under_detail}hint: init it\n"
            )
        );
    }

    #[test]
    fn only_the_mark_is_coloured_and_off_is_never_coloured() {
        let text = rows(
            &[
                row(Mark::FAIL, "a", "broken", &["fix it"]),
                row(Mark::OFF, "b", "not on", &[]),
            ],
            true,
        );
        assert_eq!(
            text,
            "\x1b[31m✗\x1b[0m a  broken\n     hint: fix it\n– b  not on\n"
        );
    }

    #[test]
    fn counts_agree_with_their_noun() {
        assert_eq!(count(0_usize, "pane", "panes"), "0 panes");
        assert_eq!(count(1_u64, "pane", "panes"), "1 pane");
        assert_eq!(count(2_u32, "pane", "panes"), "2 panes");
    }
}
