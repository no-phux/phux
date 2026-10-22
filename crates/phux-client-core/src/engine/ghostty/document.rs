//! The [`EngineDocumentAdapter`] implementation and loaded-history search.

use super::{
    CellWide, DocumentAnchorId, DocumentPoint, DocumentSpace, EngineDocumentAdapter,
    EngineDocumentSelection, EngineHistoryProjection, EngineProjectionOrigin, EngineSearchMatch,
    FormatOptions, GhosttyAdapter, GhosttyEngineError, GhosttyReplica, GhosttyTerminal, Point,
    PointCoordinate, Selection, VecDeque, empty_projection, engine_history_rows,
    grid_ref_graphemes, history_window, rewrap_history_rows, to_ghostty_point, to_ghostty_space,
    trim_projection_rows,
};

impl EngineDocumentAdapter for GhosttyAdapter {
    fn project_history(
        &mut self,
        replica: &mut Self::Replica,
        width: u16,
        origin: EngineProjectionOrigin,
        max_rows: usize,
    ) -> Result<EngineHistoryProjection, Self::Error> {
        let terminal = replica
            .terminal()
            .ok_or(GhosttyEngineError::LiveOutputBeforeReady)?;
        let width = width.max(2);
        if max_rows == 0 {
            return Ok(empty_projection(width, false));
        }
        let history_rows = terminal.scrollback_rows()?;
        let physical_limit = max_rows.saturating_add(1);
        let Some(window) = history_window(replica, terminal, origin, history_rows, physical_limit)?
        else {
            return Ok(empty_projection(width, true));
        };
        let source = engine_history_rows(terminal, window.start, window.end)?;
        let mut rows = rewrap_history_rows(source, width);
        let trimmed_older = trim_projection_rows(&mut rows, max_rows, window.tail);
        Ok(EngineHistoryProjection {
            width,
            rows,
            has_older: window.start > 0 || trimmed_older,
        })
    }

    fn track_document_anchor(
        &mut self,
        replica: &mut Self::Replica,
        point: DocumentPoint,
    ) -> Result<DocumentAnchorId, Self::Error> {
        let tracked = replica
            .terminal()
            .ok_or(GhosttyEngineError::LiveOutputBeforeReady)?
            .track_grid_ref(to_ghostty_point(point))?;
        let id = DocumentAnchorId::from_raw(self.next_anchor_id);
        self.next_anchor_id = self.next_anchor_id.wrapping_add(1).max(1);
        replica.anchors.insert(id, tracked);
        Ok(id)
    }

    fn release_document_anchor(&mut self, replica: &mut Self::Replica, anchor: DocumentAnchorId) {
        replica.anchors.remove(&anchor);
    }

    fn document_anchor_point(
        &self,
        replica: &Self::Replica,
        anchor: DocumentAnchorId,
        space: DocumentSpace,
    ) -> Result<Option<DocumentPoint>, Self::Error> {
        let Some(anchor) = replica.anchors.get(&anchor) else {
            return Ok(None);
        };
        let Some(point) = anchor.point(to_ghostty_space(space))? else {
            return Ok(None);
        };
        Ok(Some(DocumentPoint {
            space,
            x: point.x,
            y: point.y,
        }))
    }

    fn search_loaded(
        &mut self,
        replica: &mut Self::Replica,
        needle: &str,
        max_matches: usize,
    ) -> Result<Vec<EngineSearchMatch>, Self::Error> {
        if needle.is_empty() || max_matches == 0 {
            return Ok(Vec::new());
        }
        let ranges = {
            let terminal = replica
                .terminal()
                .ok_or(GhosttyEngineError::LiveOutputBeforeReady)?;
            scan_history_for_needle(terminal, needle, max_matches, self.search_case_sensitive)?
        };
        self.track_search_matches(replica, ranges)
    }

    fn format_selection(
        &self,
        replica: &Self::Replica,
        selection: EngineDocumentSelection,
    ) -> Result<Option<String>, Self::Error> {
        let terminal = replica
            .terminal()
            .ok_or(GhosttyEngineError::LiveOutputBeforeReady)?;
        let Some(start) = replica.anchors.get(&selection.start) else {
            return Ok(None);
        };
        let Some(end) = replica.anchors.get(&selection.end) else {
            return Ok(None);
        };
        let Some(start) = start.snapshot(terminal)? else {
            return Ok(None);
        };
        let Some(end) = end.snapshot(terminal)? else {
            return Ok(None);
        };
        let selection = Selection::new(start, end, selection.rectangle);
        let formatted = terminal.format_selection_alloc(
            None,
            FormatOptions::new()
                .with_selection(&selection)
                .with_unwrap(true)
                .with_trim(true),
        )?;
        Ok(formatted.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }

    fn format_selection_bounded(
        &self,
        replica: &Self::Replica,
        selection: EngineDocumentSelection,
        max_bytes: usize,
    ) -> Result<crate::engine::BoundedSelectionText, Self::Error> {
        super::selection::format_bounded(replica, selection, max_bytes)
    }
}

/// Rolling window of scalars compared against the search needle.
pub(super) struct NeedleScan<'needle> {
    needle: &'needle [char],
    max_matches: usize,
    window: VecDeque<(char, DocumentPoint)>,
    ranges: Vec<(DocumentPoint, DocumentPoint)>,
    case_sensitive: bool,
}

impl<'needle> NeedleScan<'needle> {
    pub(super) fn new(needle: &'needle [char], max_matches: usize, case_sensitive: bool) -> Self {
        Self {
            needle,
            max_matches,
            window: VecDeque::with_capacity(needle.len()),
            ranges: Vec::new(),
            case_sensitive,
        }
    }

    /// Feed one scalar; reports whether every requested match has been collected.
    pub(super) fn push(&mut self, scalar: char, point: DocumentPoint) -> bool {
        self.window.push_back((scalar, point));
        if self.window.len() > self.needle.len() {
            self.window.pop_front();
        }
        if self.window.len() == self.needle.len()
            && self
                .window
                .iter()
                .zip(self.needle)
                .all(|((value, _), expected)| self.scalar_matches(*value, *expected))
        {
            let (Some((_, start)), Some((_, end))) = (self.window.front(), self.window.back())
            else {
                unreachable!("a matched search window is non-empty");
            };
            self.ranges.push((*start, *end));
        }
        self.ranges.len() == self.max_matches
    }

    pub(super) fn into_ranges(self) -> Vec<(DocumentPoint, DocumentPoint)> {
        self.ranges
    }

    const fn scalar_matches(&self, value: char, expected: char) -> bool {
        if self.case_sensitive {
            value == expected
        } else {
            value.eq_ignore_ascii_case(&expected)
        }
    }
}

/// A history-space document point at one grid coordinate.
pub(super) const fn history_point(x: u16, y: u32) -> DocumentPoint {
    DocumentPoint {
        space: DocumentSpace::History,
        x,
        y,
    }
}

/// Read one history row's soft-wrap flag, or `None` once history runs out.
pub(super) fn history_row_wrapped_at(
    terminal: &GhosttyTerminal<'_, '_>,
    y: u32,
) -> Result<Option<bool>, GhosttyEngineError> {
    let first = match terminal.grid_ref(Point::History(PointCoordinate { x: 0, y })) {
        Ok(value) => value,
        Err(libghostty_vt::Error::InvalidValue) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(first.row()?.is_wrapped()?))
}

/// Feed every scalar of one history row to the scan; reports whether it filled.
fn scan_history_row(
    terminal: &GhosttyTerminal<'_, '_>,
    scan: &mut NeedleScan<'_>,
    cols: u16,
    y: u32,
    wrapped: bool,
) -> Result<bool, GhosttyEngineError> {
    let mut blanks = Vec::new();
    for x in 0..cols {
        let grid_ref = terminal.grid_ref(Point::History(PointCoordinate { x, y }))?;
        let Some(scalars) = search_cell_graphemes(&grid_ref)? else {
            continue;
        };
        if scalars.is_empty() {
            blanks.push(x);
            continue;
        }
        // Textless interior cells are spaces, not adjacency. Delay them so
        // trailing empty padding does not separate text from its newline.
        if scan_blank_columns(scan, &mut blanks, y) {
            return Ok(true);
        }
        for scalar in scalars {
            if scan.push(scalar, history_point(x, y)) {
                return Ok(true);
            }
        }
    }
    // A soft wrap continues the same logical line, including sparse gaps
    // before a wide spacer head. Only hard-line padding is discarded.
    Ok(wrapped && scan_blank_columns(scan, &mut blanks, y))
}

fn scan_blank_columns(scan: &mut NeedleScan<'_>, blanks: &mut Vec<u16>, y: u32) -> bool {
    for blank in blanks.drain(..) {
        if scan.push(' ', history_point(blank, y)) {
            return true;
        }
    }
    false
}

/// Wide continuation cells occupy columns but contribute no searchable text.
fn search_cell_graphemes(
    grid_ref: &libghostty_vt::screen::GridRef<'_>,
) -> Result<Option<Vec<char>>, GhosttyEngineError> {
    if matches!(
        grid_ref.cell()?.wide()?,
        CellWide::SpacerTail | CellWide::SpacerHead
    ) {
        return Ok(None);
    }
    grid_ref_graphemes(grid_ref).map(Some)
}

/// Scan loaded history oldest-first for `needle`, stopping at `max_matches` ranges.
///
/// A row that is not soft-wrapped ends a logical line, so a newline scalar is fed
/// at its end; the needle therefore matches across the engine's own row wrapping.
pub(super) fn scan_history_for_needle(
    terminal: &GhosttyTerminal<'_, '_>,
    needle: &str,
    max_matches: usize,
    case_sensitive: bool,
) -> Result<Vec<(DocumentPoint, DocumentPoint)>, GhosttyEngineError> {
    let needle: Vec<char> = needle.chars().collect();
    let mut scan = NeedleScan::new(&needle, max_matches, case_sensitive);
    let cols = terminal.cols()?;
    let mut y = 0_u32;
    while let Some(wrapped) = history_row_wrapped_at(terminal, y)? {
        if scan_history_row(terminal, &mut scan, cols, y, wrapped)? {
            break;
        }
        if !wrapped && scan.push('\n', history_point(cols.saturating_sub(1), y)) {
            break;
        }
        let Some(next) = y.checked_add(1) else {
            break;
        };
        y = next;
    }
    Ok(scan.into_ranges())
}

pub(super) fn enforce_history_budget(
    replica: &mut GhosttyReplica,
) -> Result<(), GhosttyEngineError> {
    if replica.history_max_bytes.is_some() {
        replica.set_scrollback_max_bytes(replica.history_max_bytes)?;
    }
    if replica.history_max_lines.is_some() {
        replica.set_scrollback_max_lines(replica.history_max_lines)?;
    }
    Ok(())
}
