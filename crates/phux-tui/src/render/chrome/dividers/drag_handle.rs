//! Where a pane's title sits in its rule, for dragging the pane by it.

use super::cells::ChromeCells;
use super::{PaneLabel, TITLE_CHROME_CELLS, draw_one_title};
use crate::layout::Rect;
use crate::render::theme::Theme;

/// The columns `[start, end)` of the rule above `rect` that its title takes,
/// as the title drawing lays it out; `None` when it draws none. A pane is
/// dragged by its title.
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
    (end > start).then_some((start, end))
}
