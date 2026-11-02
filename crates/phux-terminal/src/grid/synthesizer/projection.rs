use super::{
    CellColor, CellInfo, CellIteration, CellIterator, CellSemanticContent, CellStyle, CellWide,
    CursorState, GRAPHEME_INLINE, GhosttyTerminal, GridRef, Point, PointCoordinate, RenderWalk,
    RgbColor, RowIteration, SCHEMA_VERSION, SCROLLBACK_ALL, ScreenState, ScrollbackWindow,
    SemanticContent, SnapshotSynthesizer, SoftWrap, StyleColor, SynthesisError,
    TRUNCATED_ROW_WINDOW, ViewportProjection, grid_dims, viewport_row_is_wrapped,
    walk_viewport_rows,
};

impl<'alloc> SnapshotSynthesizer<'alloc> {
    /// Project the viewport into a structured [`ScreenState`] (ADR-0022 §2)
    /// for agents: text rows and cursor, no VT bytes, no side effects.
    pub fn screen_state(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        pane: u32,
    ) -> Result<ScreenState, SynthesisError> {
        self.screen_state_with_scrollback(terminal, pane, None, false)
    }

    /// Like [`Self::screen_state`], plus up to `scrollback` history rows
    /// (`None`, [`SCROLLBACK_ALL`], or `Some(n)` as in
    /// [`Self::synthesize_with_scrollback`]). History is read via
    /// `Point::History`, which neither scrolls nor mutates the terminal.
    ///
    /// With `cells`, also collects a sparse [`ScreenState::cells`] of
    /// non-default-style or OSC-133-marked cells (see `collect_cell`).
    ///
    /// Reads through the tick's pooled render state without clearing its
    /// dirty flags (see `project_viewport`), so the next tick stays
    /// incremental.
    pub fn screen_state_with_scrollback(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        pane: u32,
        scrollback: Option<u32>,
        cells: bool,
    ) -> Result<ScreenState, SynthesisError> {
        // Read history before borrowing the render state: `grid_ref`
        // references die at the next terminal operation, so copy eagerly.
        let history = match scrollback {
            None => ScrollbackWindow::default(),
            Some(want) => Self::scrollback_window(terminal, want)?,
        };

        let title = pane_title(terminal);
        let view = self.project_viewport(terminal, cells)?;

        Ok(ScreenState {
            schema_version: SCHEMA_VERSION,
            pane,
            cols: view.cols,
            rows: view.rows,
            cursor: view.cursor,
            lines: view.lines,
            scrollback: history.lines,
            cells: view.cells,
            soft_wrap: Some(SoftWrap {
                lines: view.wrapped,
                scrollback: history.wrapped,
            }),
            truncated: history.truncated,
            truncated_reason: history.truncated.then(|| TRUNCATED_ROW_WINDOW.to_owned()),
            title,
            // Filled by `Self::render_screen` when the request asks for it.
            rendered: None,
            rendered_error: None,
        })
    }

    /// Walk the live viewport into the projection
    /// [`Self::screen_state_with_scrollback`] reports.
    ///
    /// Walks the tick's pool, which is a correct live copy: it rebuilds on a
    /// geometry change or after a foreign walk ([`Self::pool_walk_generation`]).
    /// It clears no dirty flag, row or global, so what the walk pulled from
    /// the terminal is still pending for the next [`Self::prepare_tick`]
    /// (phux-69pq.14: the agent detector's periodic scan no longer turns
    /// the next tick into a full render).
    fn project_viewport(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        cells: bool,
    ) -> Result<ViewportProjection, SynthesisError> {
        let generation = self.pool_walk_generation();
        let RenderWalk {
            snapshot,
            rows: rows_pool,
            cells: cells_pool,
        } = self.pool.begin(terminal, generation)?;
        let (cols, rows_n) = grid_dims(&snapshot)?;

        let cursor = snapshot.cursor_viewport()?.map(|c| CursorState {
            x: c.x,
            y: c.y,
            visible: snapshot.cursor_visible().unwrap_or(true),
        });

        // Only allocate the cells vec when the caller asked; the common
        // `--cells`-absent snapshot pays nothing.
        let mut cell_infos: Option<Vec<CellInfo>> = cells.then(Vec::new);

        let mut lines: Vec<String> = Vec::with_capacity(usize::from(rows_n));
        // Soft-wrap bits (ADR-0077 §2), always reported so a consumer can
        // tell "nothing wraps" from "older server".
        let mut wrapped_lines: Vec<u32> = Vec::new();
        walk_viewport_rows(rows_pool, &snapshot, rows_n, |row_index, row| {
            if viewport_row_is_wrapped(row)? {
                wrapped_lines.push(u32::from(row_index));
            }
            lines.push(project_row_text(
                cells_pool,
                row,
                cols,
                row_index,
                &mut cell_infos,
            )?);
            Ok(())
        })?;

        Ok(ViewportProjection {
            cols,
            rows: rows_n,
            cursor,
            lines,
            wrapped: wrapped_lines,
            cells: cell_infos,
        })
    }

    /// Read history rows above the viewport as right-trimmed strings, oldest
    /// first, with soft-wrap bits. `want` follows [`SCROLLBACK_ALL`]
    /// semantics. History `y = 0` is the oldest retained row.
    ///
    /// `truncated` is true exactly when older retained rows fell outside the
    /// window; it says nothing about rows libghostty already evicted.
    fn scrollback_window(
        terminal: &GhosttyTerminal<'alloc, '_>,
        want: u32,
    ) -> Result<ScrollbackWindow, SynthesisError> {
        let total = terminal.scrollback_rows()?;
        if total == 0 {
            return Ok(ScrollbackWindow::default());
        }
        let cols = terminal.cols()?;
        let start = history_window_start(total, want);

        let mut out: Vec<String> = Vec::with_capacity(total - start);
        let mut wrapped: Vec<u32> = Vec::new();
        for y in start..total {
            // `total` came from a C count; clamp rather than truncate.
            let y = u32::try_from(y).unwrap_or(u32::MAX);
            // Wrap indices are into the returned window, not into history.
            if cols > 0 && history_row_is_wrapped(terminal, y)? {
                wrapped.push(u32::try_from(out.len()).unwrap_or(u32::MAX));
            }
            out.push(history_row_text(terminal, cols, y)?);
        }
        Ok(ScrollbackWindow {
            lines: out,
            wrapped,
            truncated: start > 0,
        })
    }
}

/// The pane's OSC 0/2 title, or `None` when unset (never `Some("")`).
fn pane_title(terminal: &GhosttyTerminal<'_, '_>) -> Option<String> {
    terminal
        .title()
        .ok()
        .filter(|t| !t.is_empty())
        .map(ToOwned::to_owned)
}

/// Project one viewport row to its right-trimmed plain text, recording each
/// cell's [`CellInfo`] into `cell_infos` when the caller asked for cells.
fn project_row_text<'alloc>(
    cells_pool: &mut CellIterator<'alloc>,
    row: &RowIteration<'alloc, '_>,
    cols: u16,
    row_index: u16,
    cell_infos: &mut Option<Vec<CellInfo>>,
) -> Result<String, SynthesisError> {
    let mut buf = String::with_capacity(usize::from(cols));
    let mut col_index: u16 = 0;
    let mut cell_iter = cells_pool.update(row)?;
    while let Some(cell) = cell_iter.next() {
        let wide = cell.raw_cell()?.wide()?;
        if matches!(wide, CellWide::SpacerTail) {
            // Wide-cell tail: the base glyph already covers this column.
            continue;
        }
        record_cell_info(cell_infos, cell, row_index, col_index)?;
        append_cell_text(&mut buf, cell)?;
        // Advance by display width so columns match the grid (and cursor.x).
        col_index = col_index.saturating_add(if matches!(wide, CellWide::Wide) { 2 } else { 1 });
    }
    Ok(buf.trim_end().to_owned())
}

/// Record this cell's sparse [`CellInfo`] projection, if the caller asked for
/// cells and the cell carries a style or semantic mark worth reporting.
fn record_cell_info(
    cell_infos: &mut Option<Vec<CellInfo>>,
    cell: &CellIteration<'_, '_>,
    row_index: u16,
    col_index: u16,
) -> Result<(), SynthesisError> {
    let Some(infos) = cell_infos.as_mut() else {
        return Ok(());
    };
    if let Some(info) = collect_cell(cell, row_index, col_index)? {
        infos.push(info);
    }
    Ok(())
}

/// Append the cell's grapheme cluster to `buf`, or a space for a blank cell.
fn append_cell_text(buf: &mut String, cell: &CellIteration<'_, '_>) -> Result<(), SynthesisError> {
    let graphemes = cell.graphemes()?;
    if graphemes.is_empty() {
        buf.push(' ');
    } else {
        buf.extend(graphemes);
    }
    Ok(())
}

/// Start of the history window: the `want` rows nearest the viewport.
pub(super) fn history_window_start(total: usize, want: u32) -> usize {
    if want == SCROLLBACK_ALL {
        0
    } else {
        total.saturating_sub(usize::try_from(want).unwrap_or(usize::MAX))
    }
}

/// Soft-wrap bit of one history row: true when it continues onto the next.
fn history_row_is_wrapped(
    terminal: &GhosttyTerminal<'_, '_>,
    y: u32,
) -> Result<bool, SynthesisError> {
    let head = Point::History(PointCoordinate { x: 0, y });
    Ok(terminal.grid_ref(head)?.row()?.is_wrapped()?)
}

/// Read one history row into right-trimmed plain text, skipping wide-cell
/// tails (their base glyph already claimed both columns).
fn history_row_text(
    terminal: &GhosttyTerminal<'_, '_>,
    cols: u16,
    y: u32,
) -> Result<String, SynthesisError> {
    let mut buf = String::with_capacity(usize::from(cols));
    for x in 0..cols {
        let point = Point::History(PointCoordinate { x, y });
        let grid_ref = terminal.grid_ref(point)?;
        if matches!(grid_ref.cell()?.wide()?, CellWide::SpacerTail) {
            continue;
        }
        append_history_grapheme(&mut buf, &grid_ref)?;
    }
    Ok(buf.trim_end().to_owned())
}

/// Append a history cell's grapheme cluster (a space for a blank cell),
/// retrying on the heap for clusters deeper than the inline buffer.
fn append_history_grapheme(buf: &mut String, grid_ref: &GridRef<'_>) -> Result<(), SynthesisError> {
    let mut inline = [char::from(0u8); GRAPHEME_INLINE];
    match grid_ref.graphemes(&mut inline) {
        Ok(0) => buf.push(' '),
        Ok(n) => buf.extend(&inline[..n]),
        Err(libghostty_vt::Error::OutOfSpace { required }) => {
            let mut heap = vec![char::from(0u8); required];
            let n = grid_ref.graphemes(&mut heap)?;
            buf.extend(&heap[..n]);
        }
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

/// Project a viewport cell into a [`CellInfo`], or `None` for a plain cell
/// (no non-default style, no OSC-133 mark), keeping [`ScreenState::cells`]
/// sparse. Wide-cell tails are skipped by the caller.
///
/// Two sibling projections live in the TUI render path and `phux-record`;
/// `cell_projection_conformance.rs` keeps the three in agreement.
fn collect_cell(
    cell: &CellIteration<'_, '_>,
    row: u16,
    col: u16,
) -> Result<Option<CellInfo>, SynthesisError> {
    let style = cell.style()?;
    // libghostty defaults every cell to `Output`, so only `Input`/`Prompt`
    // carry information.
    let semantic = match cell.raw_cell()?.semantic_content()? {
        CellSemanticContent::Output => None,
        CellSemanticContent::Input => Some(SemanticContent::Input),
        CellSemanticContent::Prompt => Some(SemanticContent::Prompt),
    };

    // Resolved colors first; fall back to the raw color so a palette index
    // survives as a palette index.
    let fg = cell_color(cell.fg_color()?, style.fg_color);
    let bg = cell_color(cell.bg_color()?, style.bg_color);

    let cell_style = CellStyle {
        bold: style.bold,
        faint: style.faint,
        italic: style.italic,
        underline: !matches!(style.underline, libghostty_vt::style::Underline::None),
        blink: style.blink,
        inverse: style.inverse,
        invisible: style.invisible,
        strikethrough: style.strikethrough,
        overline: style.overline,
        fg,
        bg,
    };

    // Sparse: drop cells that carry nothing an agent could act on.
    if semantic.is_none() && cell_style == DEFAULT_CELL_STYLE {
        return Ok(None);
    }

    Ok(Some(CellInfo {
        col,
        row,
        semantic,
        style: cell_style,
    }))
}

/// The all-off, all-default [`CellStyle`] — the sentinel `collect_cell`
/// compares against to keep the cells projection sparse.
const DEFAULT_CELL_STYLE: CellStyle = CellStyle {
    bold: false,
    faint: false,
    italic: false,
    underline: false,
    blink: false,
    inverse: false,
    invisible: false,
    strikethrough: false,
    overline: false,
    fg: CellColor::Default,
    bg: CellColor::Default,
};

/// Prefer the cell's explicit color (palette indices keep their identity),
/// then a resolved RGB, else [`CellColor::Default`].
fn cell_color(resolved: Option<RgbColor>, raw: StyleColor) -> CellColor {
    match raw {
        StyleColor::Palette(index) => CellColor::Palette { index: index.0 },
        StyleColor::Rgb(rgb) => CellColor::Rgb {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        },
        StyleColor::None => resolved.map_or(CellColor::Default, |rgb| CellColor::Rgb {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        }),
    }
}
