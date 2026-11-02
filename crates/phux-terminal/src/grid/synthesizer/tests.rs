use super::*;
use libghostty_vt::Terminal as GhosttyTerminal;

fn fresh(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
    {
        let mut terminal = GhosttyTerminal::new(cols, rows).expect("Terminal::new");
        terminal
            .set_scrollback_max_lines(Some(100))
            .expect("Terminal::new");
        terminal
    }
}

/// Each viewport row as a string, skipping wide-cell tails.
fn render_grid(t: &GhosttyTerminal<'_, '_>) -> Vec<String> {
    let mut rs = RenderState::new().expect("RenderState::new");
    let snap = rs.update(t).expect("update");
    let rows_n = snap.rows().expect("rows");
    let mut row_iter_storage = RowIterator::new().expect("RowIterator::new");
    let mut cell_iter_storage = CellIterator::new().expect("CellIterator::new");
    let mut row_iter = row_iter_storage.update(&snap).expect("row update");
    let mut grid: Vec<String> = Vec::with_capacity(usize::from(rows_n));
    let mut i: u16 = 0;
    while let Some(row) = row_iter.next() {
        if i >= rows_n {
            break;
        }
        let mut line = String::new();
        let mut cell_iter = cell_iter_storage.update(row).expect("cell update");
        while let Some(cell) = cell_iter.next() {
            let wide = cell.raw_cell().expect("raw_cell").wide().expect("wide");
            if matches!(wide, CellWide::SpacerTail) {
                continue;
            }
            let graphemes = cell.graphemes().expect("graphemes");
            if graphemes.is_empty() {
                line.push(' ');
            } else {
                for ch in &graphemes {
                    line.push(*ch);
                }
            }
        }
        grid.push(line);
        i += 1;
    }
    grid
}

mod incremental;
mod kitty;
mod modes;
mod rendered;
mod screen_state;
mod scrollback;
mod snapshot;
mod styling;
