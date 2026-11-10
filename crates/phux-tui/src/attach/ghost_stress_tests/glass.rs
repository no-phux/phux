//! Reading the "glass" terminal (and the pane mirrors) back as text, so a
//! stress step can diff what reached the screen against the reference.

use libghostty_vt::Terminal as GhosttyTerminal;
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};

/// Read a terminal's visible grid as one `String` per row (blank cells as
/// spaces; ASCII-only fixtures, so no wide-glyph spacer handling needed).
/// Uses its own iterators so reading never disturbs a pane renderer's
/// incremental dirty state.
pub(super) fn term_grid(term: &GhosttyTerminal<'_, '_>) -> Vec<String> {
    let mut state = RenderState::new().expect("RenderState");
    let mut rows_it = RowIterator::new().expect("RowIterator");
    let mut cells_it = CellIterator::new().expect("CellIterator");
    let snap = state.update(term).expect("snapshot");
    let total_rows = snap.rows().expect("rows");
    let total_cols = snap.cols().expect("cols");
    let mut grid = Vec::new();
    let mut ri = rows_it.update(&snap).expect("row iter");
    let mut r: u16 = 0;
    while let Some(row) = ri.next() {
        if r >= total_rows {
            break;
        }
        let mut line = String::new();
        let mut ci = cells_it.update(row).expect("cell iter");
        let mut c: u16 = 0;
        while let Some(cell) = ci.next() {
            if c >= total_cols {
                break;
            }
            let graphemes = cell.graphemes().expect("graphemes");
            if graphemes.is_empty() {
                line.push(' ');
            } else {
                for ch in graphemes {
                    line.push(ch);
                }
            }
            c += 1;
        }
        grid.push(line);
        r += 1;
    }
    grid
}

pub(super) fn dump(grid: &[String]) -> String {
    grid.iter()
        .enumerate()
        .map(|(i, l)| format!("{i:2}|{l}|"))
        .collect::<Vec<_>>()
        .join("\n")
}
