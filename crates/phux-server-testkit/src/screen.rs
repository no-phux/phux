//! `Screen`: feed server-emitted VT bytes into a fresh libghostty `Terminal`
//! and read the grid back as right-trimmed plain-text rows, walked through
//! the same `RenderPool` the production client uses. Walk failures degrade
//! to empty rows rather than panicking inside an assertion helper.

use libghostty_vt::Terminal as GhosttyTerminal;
use libghostty_vt::screen::CellWide;
use phux_protocol::render_pool::{RenderPool, RenderWalk};

/// One terminal per `Screen`, so the pool identity never changes.
const POOL_GENERATION: u128 = 0;

/// Construction errors from libghostty.
#[derive(Debug, thiserror::Error)]
pub enum ScreenError {
    #[error("libghostty: {0}")]
    Ghostty(#[from] libghostty_vt::Error),
}

/// A self-contained VT oracle (`!Send`: owns a libghostty `Terminal`).
pub struct Screen {
    terminal: GhosttyTerminal<'static, 'static>,
    pool: RenderPool<'static>,
    cols: u16,
    n_rows: u16,
}

impl Screen {
    /// A fresh `cols x rows` screen with the client's live scrollback budget.
    pub fn new(cols: u16, rows: u16) -> Result<Self, ScreenError> {
        let terminal = {
            let mut terminal = GhosttyTerminal::new(cols, rows)?;
            terminal.set_scrollback_max_lines(Some(100))?;
            terminal
        };
        Ok(Self {
            terminal,
            pool: RenderPool::new()?,
            cols,
            n_rows: rows,
        })
    }

    /// Feed VT bytes; partial escape sequences carry across calls.
    pub fn write(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    /// Row `idx` (0-based), right-trimmed; empty past the viewport.
    pub fn row(&mut self, idx: u16) -> String {
        if idx >= self.n_rows {
            return String::new();
        }
        let rows = self.rows_internal();
        rows.get(usize::from(idx)).cloned().unwrap_or_default()
    }

    /// Every viewport row, right-trimmed.
    pub fn rows(&mut self) -> Vec<String> {
        self.rows_internal()
    }

    /// True if any row's trimmed text contains `needle`.
    pub fn contains(&mut self, needle: &str) -> bool {
        self.rows_internal().iter().any(|r| r.contains(needle))
    }

    /// All rows joined with `\n`.
    pub fn snapshot_text(&mut self) -> String {
        self.rows_internal().join("\n")
    }

    fn rows_internal(&mut self) -> Vec<String> {
        let Ok(RenderWalk {
            snapshot,
            rows,
            cells,
        }) = self.pool.begin(&self.terminal, POOL_GENERATION)
        else {
            return vec![String::new(); usize::from(self.n_rows)];
        };

        let total_rows = snapshot.rows().unwrap_or(self.n_rows);
        let mut out: Vec<String> = Vec::with_capacity(usize::from(total_rows));

        let Ok(mut row_iter) = rows.update(&snapshot) else {
            return vec![String::new(); usize::from(total_rows)];
        };

        let mut row_index: u16 = 0;
        while let Some(row) = row_iter.next() {
            if row_index >= total_rows {
                break;
            }
            let mut buf = String::with_capacity(usize::from(self.cols));
            let Ok(mut cell_iter) = cells.update(row) else {
                out.push(String::new());
                row_index += 1;
                continue;
            };
            while let Some(cell) = cell_iter.next() {
                // Skip wide-cell tails so a wide glyph is not glyph-plus-space.
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
            let trimmed = buf.trim_end().to_owned();
            out.push(trimmed);
            row_index += 1;
        }
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
