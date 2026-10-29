//! Frontend-width projection of loaded history rows.

use super::{
    CellContentTag, CellWide, DocumentPoint, DocumentSpace, EngineHistoryProjection,
    EngineProjectionOrigin, EngineProjectionRow, GhosttyEngineError, GhosttyReplica,
    GhosttyTerminal, Point, PointCoordinate, PointSpace,
};

pub(super) const fn to_ghostty_point(point: DocumentPoint) -> Point {
    let coordinate = PointCoordinate {
        x: point.x,
        y: point.y,
    };
    match point.space {
        DocumentSpace::History => Point::History(coordinate),
        DocumentSpace::Viewport => Point::Viewport(coordinate),
        DocumentSpace::Active => Point::Active(coordinate),
    }
}

pub(super) const fn to_ghostty_space(space: DocumentSpace) -> PointSpace {
    match space {
        DocumentSpace::History => PointSpace::History,
        DocumentSpace::Viewport => PointSpace::Viewport,
        DocumentSpace::Active => PointSpace::Active,
    }
}

fn history_row_wrapped(
    terminal: &GhosttyTerminal<'_, '_>,
    row: usize,
) -> Result<bool, GhosttyEngineError> {
    let y = u32::try_from(row).unwrap_or(u32::MAX);
    Ok(terminal
        .grid_ref(Point::History(PointCoordinate { x: 0, y }))?
        .row()?
        .is_wrapped()?)
}

pub(super) fn grid_ref_graphemes(
    grid_ref: &libghostty_vt::screen::GridRef<'_>,
) -> Result<Vec<char>, GhosttyEngineError> {
    let mut inline = ['\0'; 8];
    match grid_ref.graphemes(&mut inline) {
        Ok(len) => Ok(inline[..len].to_vec()),
        Err(libghostty_vt::Error::OutOfSpace { required }) => {
            let mut values = vec!['\0'; required];
            let len = grid_ref.graphemes(&mut values)?;
            values.truncate(len);
            Ok(values)
        }
        Err(error) => Err(error.into()),
    }
}
#[derive(Debug)]
pub(super) struct ProjectedCell {
    pub(super) text: String,
    pub(super) width: usize,
    pub(super) empty_default: bool,
}

pub(super) fn engine_history_rows(
    terminal: &GhosttyTerminal<'_, '_>,
    start: usize,
    end: usize,
) -> Result<Vec<(Vec<ProjectedCell>, bool)>, GhosttyEngineError> {
    let cols = terminal.cols()?;
    let mut rows = Vec::with_capacity(end.saturating_sub(start));
    for row in start..end {
        rows.push(engine_history_row(terminal, row, cols)?);
    }
    Ok(rows)
}

/// Project one physical history row into its cells plus its soft-wrap flag.
fn engine_history_row(
    terminal: &GhosttyTerminal<'_, '_>,
    row: usize,
    cols: u16,
) -> Result<(Vec<ProjectedCell>, bool), GhosttyEngineError> {
    let y = u32::try_from(row).unwrap_or(u32::MAX);
    let first = terminal.grid_ref(Point::History(PointCoordinate { x: 0, y }))?;
    let wrapped = first.row()?.is_wrapped()?;
    let mut cells = Vec::with_capacity(usize::from(cols));
    for x in 0..cols {
        if let Some(cell) = project_history_cell(terminal, x, y)? {
            cells.push(cell);
        }
    }
    Ok((cells, wrapped))
}

/// Project one history cell, or `None` for a wide-cell spacer that owns no text.
fn project_history_cell(
    terminal: &GhosttyTerminal<'_, '_>,
    x: u16,
    y: u32,
) -> Result<Option<ProjectedCell>, GhosttyEngineError> {
    let grid_ref = terminal.grid_ref(Point::History(PointCoordinate { x, y }))?;
    let cell = grid_ref.cell()?;
    let wide = cell.wide()?;
    if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
        return Ok(None);
    }
    let text: String = grid_ref_graphemes(&grid_ref)?.into_iter().collect();
    Ok(Some(ProjectedCell {
        empty_default: is_empty_default_cell(cell, &text)?,
        text,
        width: if wide == CellWide::Wide { 2 } else { 1 },
    }))
}

/// Whether a cell carries nothing but untouched default codepoint content.
fn is_empty_default_cell(
    cell: libghostty_vt::screen::Cell,
    text: &str,
) -> Result<bool, GhosttyEngineError> {
    Ok(text.is_empty()
        && cell.codepoint()? == 0
        && cell.content_tag()? == CellContentTag::Codepoint)
}

/// Physical history rows to project, and whether the tail pinned the window.
#[derive(Debug, Clone, Copy)]
pub(super) struct HistoryWindow {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) tail: bool,
}

/// An empty projection at the caller's width.
pub(super) const fn empty_projection(width: u16, has_older: bool) -> EngineHistoryProjection {
    EngineHistoryProjection {
        width,
        rows: Vec::new(),
        has_older,
    }
}

/// Resolve the physical history rows one projection origin selects.
///
/// Returns `None` when an anchored origin no longer resolves to a history
/// point; the caller then reports an empty projection that still has older rows.
pub(super) fn history_window(
    replica: &GhosttyReplica,
    terminal: &GhosttyTerminal<'_, '_>,
    origin: EngineProjectionOrigin,
    history_rows: usize,
    physical_limit: usize,
) -> Result<Option<HistoryWindow>, GhosttyEngineError> {
    let Some((start, tail)) = window_origin(replica, origin, history_rows, physical_limit)? else {
        return Ok(None);
    };
    let start = extend_start_to_logical_line(terminal, start)?;
    let end = extend_end_to_logical_line(terminal, start, history_rows, physical_limit)?;
    Ok(Some(HistoryWindow { start, end, tail }))
}

/// First physical row for one origin, and whether that origin is the tail.
fn window_origin(
    replica: &GhosttyReplica,
    origin: EngineProjectionOrigin,
    history_rows: usize,
    physical_limit: usize,
) -> Result<Option<(usize, bool)>, GhosttyEngineError> {
    match origin {
        EngineProjectionOrigin::Tail => {
            Ok(Some((history_rows.saturating_sub(physical_limit), true)))
        }
        EngineProjectionOrigin::Anchor(anchor) => {
            let Some(anchor) = replica.anchors.get(&anchor) else {
                return Ok(None);
            };
            let Some(point) = anchor.point(PointSpace::History)? else {
                return Ok(None);
            };
            Ok(Some((point.y as usize, false)))
        }
    }
}

/// Walk the window start back over soft-wrapped rows onto a logical line boundary.
fn extend_start_to_logical_line(
    terminal: &GhosttyTerminal<'_, '_>,
    mut start: usize,
) -> Result<usize, GhosttyEngineError> {
    while start > 0 && history_row_wrapped(terminal, start - 1)? {
        start -= 1;
    }
    Ok(start)
}

/// Extend the window end past soft-wrapped rows so it closes on a logical line.
fn extend_end_to_logical_line(
    terminal: &GhosttyTerminal<'_, '_>,
    start: usize,
    history_rows: usize,
    physical_limit: usize,
) -> Result<usize, GhosttyEngineError> {
    let mut end = history_rows.min(start.saturating_add(physical_limit));
    while end < history_rows && end > start && history_row_wrapped(terminal, end - 1)? {
        end += 1;
    }
    Ok(end)
}

/// Trim a rewrapped projection to `max_rows`, dropping the end the origin did not pin.
///
/// Returns whether trimming dropped rows off the front, which leaves older rows behind.
pub(super) fn trim_projection_rows(
    rows: &mut Vec<EngineProjectionRow>,
    max_rows: usize,
    tail: bool,
) -> bool {
    if rows.len() <= max_rows {
        return false;
    }
    if tail {
        rows.drain(..rows.len() - max_rows);
        return true;
    }
    rows.truncate(max_rows);
    false
}

pub(super) fn rewrap_history_rows(
    source: Vec<(Vec<ProjectedCell>, bool)>,
    width: u16,
) -> Vec<EngineProjectionRow> {
    let width = usize::from(width);
    let mut result = Vec::new();
    let mut logical = Vec::new();
    for (mut cells, wrapped) in source {
        logical.append(&mut cells);
        if !wrapped {
            append_rewrapped_line(&mut result, &mut logical, width);
        }
    }
    if !logical.is_empty() {
        append_rewrapped_line(&mut result, &mut logical, width);
    }
    result
}

fn append_rewrapped_line(
    result: &mut Vec<EngineProjectionRow>,
    logical: &mut Vec<ProjectedCell>,
    width: usize,
) {
    while logical.last().is_some_and(|cell| cell.empty_default) {
        logical.pop();
    }
    if logical.is_empty() {
        result.push(EngineProjectionRow {
            text: String::new(),
            soft_wrapped: false,
            page: None,
        });
        return;
    }
    let mut text = String::new();
    let mut cells: usize = 0;
    for cell in std::mem::take(logical) {
        if cells > 0 && cells.saturating_add(cell.width) > width {
            result.push(EngineProjectionRow {
                text: std::mem::take(&mut text),
                soft_wrapped: true,
                page: None,
            });
            cells = 0;
        }
        if cell.text.is_empty() {
            text.push(' ');
        } else {
            text.push_str(&cell.text);
        }
        cells = cells.saturating_add(cell.width);
    }
    result.push(EngineProjectionRow {
        text,
        soft_wrapped: false,
        page: None,
    });
}
