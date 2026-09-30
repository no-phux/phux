//! Mouse selection over the painted viewport, and the text it copies.
//!
//! A selection is two cells of the viewport grid, in either order, covering
//! the row-major run between them (a stream selection, as terminals make by
//! dragging). It is local: the server and the program never see it.

use std::ops::Range;

use phux_vt_web::Grid;

/// A selection between two viewport cells, each `(col, row)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    /// Where the drag started.
    pub anchor: (u16, u16),
    /// Where the drag is now.
    pub head: (u16, u16),
}

impl Selection {
    /// A selection that has not yet left its first cell.
    #[must_use]
    pub const fn at(cell: (u16, u16)) -> Self {
        Self {
            anchor: cell,
            head: cell,
        }
    }

    /// Whether the drag never left its first cell: a click, not a selection.
    #[must_use]
    pub fn is_click(&self) -> bool {
        self.anchor == self.head
    }

    /// Row-major indices of the selected cells in a `cols`-wide grid, both
    /// ends included.
    #[must_use]
    pub fn cells(&self, cols: u16) -> Range<usize> {
        let index = |(col, row): (u16, u16)| {
            usize::from(row) * usize::from(cols) + usize::from(col.min(cols.saturating_sub(1)))
        };
        let (a, b) = (index(self.anchor), index(self.head));
        a.min(b)..a.max(b) + 1
    }

    /// The selected text of `grid`: one line per row, trailing blanks
    /// trimmed, joined with `\n`.
    #[must_use]
    pub fn text(&self, grid: &Grid) -> String {
        let cols = usize::from(grid.cols.max(1));
        let cells = self.cells(grid.cols);
        let end = cells.end.min(grid.cells.len());
        let mut lines = Vec::new();
        let mut start = cells.start;
        while start < end {
            let row_end = ((start / cols) + 1) * cols;
            let stop = row_end.min(end);
            let line: String = grid.cells[start..stop]
                .iter()
                .map(|cell| if cell.ch == '\0' { ' ' } else { cell.ch })
                .collect();
            lines.push(line.trim_end().to_owned());
            start = stop;
        }
        lines.join("\n")
    }
}

/// The viewport cell under a point, in canvas client pixels, clamped to the
/// grid so a drag past the edge selects to the edge.
#[must_use]
pub fn cell_at(x: f64, y: f64, cell_w: f64, cell_h: f64, cols: u16, rows: u16) -> (u16, u16) {
    let clamp = |value: f64, cell: f64, count: u16| {
        let index = (value / cell.max(1.0)).floor().max(0.0);
        (index as u16).min(count.saturating_sub(1))
    };
    (clamp(x, cell_w, cols), clamp(y, cell_h, rows))
}
