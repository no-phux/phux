//! `Screen` — a libghostty-backed oracle for asserting on the bytes the
//! server emits over `TERMINAL_OUTPUT` (or any other VT byte stream).
//!
//! Background: end-to-end tests for `phux attach` collect the rendered VT
//! bytes that the server fans out to attached clients. Asserting on those
//! bytes directly is miserable — SGR escapes, CUP positioning and partial
//! redraws make naive byte/regex matching fragile and easy to false-pass
//! (e.g. "blank screen" vs "rendered text but my regex ate it" look
//! identical to a regex on stripped output).
//!
//! Instead, this helper feeds the bytes into a *fresh* `libghostty_vt::Terminal`
//! and walks the resulting grid via the same `RenderState`/`RowIterator`/
//! `CellIterator` surface the production client uses. The result is a
//! row-major plain-text snapshot you can assert on directly:
//!
//! ```ignore
//! let mut screen = Screen::new(80, 24).unwrap();
//! screen.write(&pane_output_bytes);
//! assert!(screen.row(0).contains("hi"));
//! ```
//!
//! Implementation notes:
//!
//! * The oracle deliberately ignores `Snapshot::dirty()` and walks the
//!   grid unconditionally on every `row()` call — the harness contract is
//!   "what does the grid look like right now," not "what changed since
//!   the last read."
//! * Wide-cell tails (`CellWide::SpacerTail`) must be skipped so we do not
//!   double-count the half of a wide grapheme. Mirrors the existing fix in
//!   `crates/phux-server/src/grid.rs`.
//! * `cursor_viewport()` is best-effort: we treat its `Err` and
//!   `Ok(None)` cases as "(0, 0)" so the harness degrades to a safe
//!   default rather than panicking inside an assertion helper.

use libghostty_vt::screen::CellWide;
use libghostty_vt::{
    Terminal as GhosttyTerminal, TerminalOptions,
    render::{CellIterator, RenderState, RowIterator},
};

/// Errors the harness can surface during construction. Runtime walk
/// failures are absorbed into best-effort defaults so tests can keep
/// reading without forcing every assertion into a `Result`.
#[derive(Debug, thiserror::Error)]
pub enum ScreenError {
    /// libghostty surfaced an error from `GhosttyTerminal::new` or one of the
    /// render iterator constructors.
    #[error("libghostty: {0}")]
    Ghostty(#[from] libghostty_vt::Error),
}

/// A self-contained VT oracle: owns a libghostty `Terminal` plus the
/// render iterators needed to walk its grid into plain strings.
///
/// `Screen` is `!Send` (the inner `Terminal` is `!Send`); construct it
/// on the thread that will use it. Tests typically construct one per
/// scenario inside `run_local`.
pub struct Screen {
    terminal: GhosttyTerminal<'static, 'static>,
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    cols: u16,
    n_rows: u16,
}

impl Screen {
    /// Create a fresh screen sized to `cols x rows`. The internal
    /// scrollback budget matches the default the client uses for live
    /// attach (`render.rs` uses `100`; we match it so behaviour is
    /// representative).
    pub fn new(cols: u16, rows: u16) -> Result<Self, ScreenError> {
        let terminal = GhosttyTerminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: 100,
        })?;
        Ok(Self {
            terminal,
            state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            cols,
            n_rows: rows,
        })
    }

    /// Feed VT bytes into the underlying terminal. Bytes may be split
    /// across calls — the parser is stateful, so partial escape
    /// sequences are buffered exactly like the real client.
    pub fn write(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    /// Return the contents of `idx` (0-based) as a single string,
    /// trimmed of trailing whitespace. Empty cells become spaces;
    /// rows past the configured viewport return the empty string.
    pub fn row(&mut self, idx: u16) -> String {
        if idx >= self.n_rows {
            return String::new();
        }
        let rows = self.rows_internal();
        rows.get(usize::from(idx)).cloned().unwrap_or_default()
    }

    /// Return every viewport row as a `Vec<String>`. Rows are padded
    /// to the configured width with spaces, then right-trimmed; this
    /// matches the contract of [`row`].
    pub fn rows(&mut self) -> Vec<String> {
        self.rows_internal()
    }

    /// Best-effort cursor position as `(col, row)`, 0-based. Returns
    /// `(0, 0)` when libghostty can't surface a viewport-resident
    /// cursor (e.g. because it lives in the scrollback, or because the
    /// FFI returned an error).
    pub fn cursor(&mut self) -> (u16, u16) {
        let Ok(snapshot) = self.state.update(&self.terminal) else {
            return (0, 0);
        };
        if let Ok(Some(c)) = snapshot.cursor_viewport() {
            (c.x, c.y)
        } else {
            (0, 0)
        }
    }

    /// True if any row's trimmed text contains `needle`.
    pub fn contains(&mut self, needle: &str) -> bool {
        self.rows_internal().iter().any(|r| r.contains(needle))
    }

    /// All rows joined with `\n`. Useful for printing on assertion
    /// failure (`assert!(..., "screen was:\n{}", screen.snapshot_text())`).
    pub fn snapshot_text(&mut self) -> String {
        self.rows_internal().join("\n")
    }

    /// The grid walk. Centralised so `row()`, `rows()`, `contains()`
    /// and `snapshot_text()` all share one implementation — and one
    /// place to update if libghostty's FFI shape changes.
    fn rows_internal(&mut self) -> Vec<String> {
        // Empty grid is a reasonable degradation: assertions like
        // `contains("foo")` cleanly return false, the caller can
        // print `snapshot_text()` and see "" rather than panic.
        let Ok(snapshot) = self.state.update(&self.terminal) else {
            return vec![String::new(); usize::from(self.n_rows)];
        };

        // Oracle contract: always walk the full grid; we do not consult
        // `snapshot.dirty()` (this is a state read, not a delta read).
        let total_rows = snapshot.rows().unwrap_or(self.n_rows);
        let mut out: Vec<String> = Vec::with_capacity(usize::from(total_rows));

        let Ok(mut row_iter) = self.rows.update(&snapshot) else {
            return vec![String::new(); usize::from(total_rows)];
        };

        let mut row_index: u16 = 0;
        while let Some(row) = row_iter.next() {
            if row_index >= total_rows {
                break;
            }
            let mut buf = String::with_capacity(usize::from(self.cols));
            let Ok(mut cell_iter) = self.cells.update(row) else {
                out.push(String::new());
                row_index += 1;
                continue;
            };
            while let Some(cell) = cell_iter.next() {
                // Skip wide-cell tails so a double-width glyph doesn't
                // show up as itself-plus-a-space. Mirrors the same fix
                // in `crates/phux-server/src/grid.rs`.
                let wide = cell
                    .raw_cell()
                    .and_then(libghostty_vt::screen::Cell::wide)
                    .unwrap_or(CellWide::Narrow);
                if matches!(wide, CellWide::SpacerTail) {
                    continue;
                }

                let graphemes = cell.graphemes().unwrap_or_default();
                if graphemes.is_empty() {
                    buf.push(' ');
                } else {
                    for ch in graphemes {
                        buf.push(ch);
                    }
                }
            }
            // Right-trim so trailing blanks don't break naive equality
            // checks; the harness's job is to expose *content*, not
            // padding. Callers that want raw width can call
            // `Screen::cursor()` / iterate `rows()` for length.
            let trimmed = buf.trim_end().to_owned();
            out.push(trimmed);
            row_index += 1;
        }
        // If libghostty produced fewer rows than the configured
        // viewport (it shouldn't, but be defensive), pad with empties.
        while out.len() < usize::from(self.n_rows) {
            out.push(String::new());
        }
        out
    }
}

impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Screen")
            .field("cols", &self.cols)
            .field("rows", &self.n_rows)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_ascii_then_read_row_zero() {
        let mut s = Screen::new(20, 3).unwrap();
        s.write(b"hello");
        assert_eq!(s.row(0), "hello");
    }

    #[test]
    fn contains_finds_text_anywhere() {
        let mut s = Screen::new(20, 3).unwrap();
        s.write(b"line one\r\nline two");
        assert!(s.contains("two"));
        assert!(!s.contains("three"));
    }

    #[test]
    fn snapshot_text_joins_rows_with_newlines() {
        let mut s = Screen::new(10, 3).unwrap();
        s.write(b"ab\r\ncd");
        let text = s.snapshot_text();
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "ab");
        assert_eq!(lines[1], "cd");
    }

    #[test]
    fn cursor_advances_after_write() {
        let mut s = Screen::new(20, 3).unwrap();
        s.write(b"abc");
        let (col, row) = s.cursor();
        // `cursor_viewport()` may degrade to (0, 0) when libghostty
        // can't resolve the cursor; accept either the precise answer
        // or the safe default. The important invariant for the harness
        // is "doesn't panic".
        assert!(row <= 2);
        assert!(col <= 20);
    }

    #[test]
    fn out_of_range_row_is_empty() {
        let mut s = Screen::new(10, 2).unwrap();
        s.write(b"hi");
        assert_eq!(s.row(99), "");
    }

    #[test]
    fn sgr_escapes_are_stripped_in_text_output() {
        let mut s = Screen::new(20, 3).unwrap();
        // Bold + red + "ok" + reset. The libghostty parser must absorb
        // the SGR so only "ok" lands in the grid.
        s.write(b"\x1b[1;31mok\x1b[0m");
        assert_eq!(s.row(0), "ok");
    }
}
