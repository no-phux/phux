//! Where a pane's title sits in its rule, for dragging the pane by it.

use super::cells::ChromeCells;
use super::{PaneLabel, TITLE_CHROME_CELLS, draw_one_title};
use crate::layout::Rect;
use crate::render::theme::Theme;

/// Rule cells an untitled pane is dragged by, where a short title would
/// sit. Fits any pane wide enough for a title rule.
const UNTITLED_HANDLE_CELLS: u16 = TITLE_CHROME_CELLS - 1;

/// The columns `[start, end)` of the rule above `rect` that a pane is
/// dragged by: its title, as the title drawing lays it out.
///
/// An untitled pane keeps the first few cells instead. `None` when the pane
/// has no title rule.
#[must_use]
pub fn title_span(rect: Rect, label: &PaneLabel<'_>, theme: &Theme) -> Option<(u16, u16)> {
    let y = rect.y.checked_sub(1)?;
    if rect.h == 0 || rect.w <= TITLE_CHROME_CELLS {
        return None;
    }
    let mut cells = ChromeCells::default();
    draw_one_title(&mut cells, rect, y, label, theme, false);
    // The title runs on from one cell inside the left edge; the scroll mark
    // sits apart at the right end.
    let start = rect.x + 1;
    let mut end = start;
    while cells.cells.iter().any(|cell| cell.x == end) {
        end += 1;
    }
    if end == start {
        end = start + UNTITLED_HANDLE_CELLS;
    }
    Some((start, end))
}
