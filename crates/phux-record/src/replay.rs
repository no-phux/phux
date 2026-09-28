//! Offline VT replay: captured bytes back into `RenderedFrame`s.
//!
//! Two rules hold this module together:
//!
//! 1. `RenderState::update` consumes the terminal's dirty bits, so the
//!    `Terminal` is private and this replayer's `RenderPool` is its only
//!    observer. A second observer would steal the bits and drop frames.
//! 2. [`Replayer::sample`] returns `Some` on its first call regardless of the
//!    dirty bit, or a recording that opens on a settled screen renders
//!    nothing.
//!
//! The cell projection here is a knowing third copy of the one in
//! `phux-tui`'s `attach/render.rs` and `phux-server`'s `grid/synthesizer.rs`;
//! sharing it would force `phux-core` onto `libghostty-vt`.
//! `crates/phux/tests/conformance/cell_projection_conformance.rs` holds the
//! three to cell-for-cell agreement. Change one, change all three.

use libghostty_vt::Terminal as GhosttyTerminal;
use libghostty_vt::render::{
    CellIteration, CellIterator, Dirty, RowIteration, RowIterator, Snapshot,
};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{RgbColor, Style, StyleColor, Underline};
use phux_core::screen::{CellColor, CellStyle, CursorState, RenderedFrame};
use phux_protocol::render_pool::{RenderPool, RenderWalk};

use crate::error::RecordError;
use crate::raster::Theme;

/// The replayer owns one terminal for life, so the pool token is constant.
const POOL_GENERATION: u128 = 0;

/// One sampled frame plus the row band that changed since the last sample.
#[derive(Debug, Clone)]
pub struct Sampled {
    /// The grid as dense cells.
    pub frame: RenderedFrame,
    /// Inclusive `(min_row, max_row)` of the rows that changed, or `None`
    /// when everything changed (including the first frame). An encoder hint
    /// only: the frame is always complete.
    pub dirty_rows: Option<(u16, u16)>,
}

/// Replays a captured byte stream through a private terminal emulator.
///
/// [`feed`](Self::feed) it the cast's `"o"` payloads in order and
/// [`sample`](Self::sample) on the export's fixed clock.
#[derive(Debug)]
pub struct Replayer {
    /// Never handed out; see rule 1 in the module docs.
    term: GhosttyTerminal<'static, 'static>,
    pool: RenderPool<'static>,
    sampled_once: bool,
    /// The cursor as of the last emitted frame: a `DECTCEM` toggle changes
    /// the frame without dirtying a cell.
    last_cursor: Option<CursorState>,
}

impl Replayer {
    /// Build a replayer over a fresh `cols` x `rows` terminal with no
    /// scrollback. Zero dimensions are rejected, not clamped.
    pub fn new(cols: u16, rows: u16) -> Result<Self, RecordError> {
        if cols == 0 || rows == 0 {
            return Err(RecordError::Replay(format!(
                "terminal dimensions must be non-zero, got {cols}x{rows}"
            )));
        }
        let mut term = GhosttyTerminal::new(cols, rows)
            .map_err(|err| replay_err("terminal construction", &err))?;
        term.set_scrollback_max_lines(Some(0))
            .map_err(|err| replay_err("terminal construction", &err))?;
        Ok(Self {
            term,
            pool: RenderPool::new().map_err(|err| replay_err("render pool", &err))?,
            sampled_once: false,
            last_cursor: None,
        })
    }

    /// Feed captured bytes into the emulator. Infallible: malformed input is
    /// replayed as the terminal would have shown it.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.term.vt_write(bytes);
    }

    /// Resize the replayed terminal, as an asciicast `"r"` event asks. Cell
    /// pixel dimensions are zero; nothing reads them.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), RecordError> {
        self.term
            .resize(cols, rows, 0, 0)
            .map_err(|err| replay_err("resize", &err))
    }

    /// Sample the grid, or `None` when nothing changed since the last call.
    /// The first call never returns `None`, and a cursor change counts as a
    /// change even when libghostty reports the grid clean.
    pub fn sample(&mut self) -> Result<Option<Sampled>, RecordError> {
        let first = !self.sampled_once;
        let RenderWalk {
            snapshot,
            rows,
            cells,
        } = self
            .pool
            .begin(&self.term, POOL_GENERATION)
            .map_err(|err| replay_err("snapshot", &err))?;
        let view = read_view(&snapshot)?;
        let cursor_changed = view.cursor != self.last_cursor;
        if !first && !cursor_changed && matches!(view.dirty, Dirty::Clean) {
            return Ok(None);
        }
        self.sampled_once = true;

        let mut frame = RenderedFrame::blank(view.cols, view.rows);

        let whole_canvas = first || matches!(view.dirty, Dirty::Full);

        let mut band = project_rows(rows, cells, &snapshot, view.cols, view.rows, &mut frame)?;

        if cursor_changed {
            band = band_with_cursor_rows(
                band,
                self.last_cursor.as_ref(),
                view.cursor.as_ref(),
                view.rows,
            );
        }
        self.last_cursor.clone_from(&view.cursor);
        frame.cursor = view.cursor;

        // Clear the sticky snapshot-level bit too, or every later sample
        // reports `Dirty::Full`. The client renderer and server synthesizer
        // pair the row and snapshot clears the same way.
        snapshot
            .set_dirty(Dirty::Clean)
            .map_err(|err| replay_err("set_dirty", &err))?;

        Ok(Some(Sampled {
            frame,
            // A partial dirty that touched no row falls back to whole-canvas.
            dirty_rows: if whole_canvas { None } else { band },
        }))
    }

    /// The terminal's own colour table. Reads through the same render state
    /// as `sample`; pending dirty state survives until a sample clears it.
    pub(crate) fn theme(&mut self) -> Result<Theme, RecordError> {
        let RenderWalk { snapshot, .. } = self
            .pool
            .begin(&self.term, POOL_GENERATION)
            .map_err(|err| replay_err("snapshot", &err))?;
        let colors = snapshot
            .colors()
            .map_err(|err| replay_err("colors", &err))?;
        Ok(Theme {
            fg: rgb(colors.foreground),
            bg: rgb(colors.background),
            palette: colors.palette.map(rgb),
        })
    }
}

/// What one snapshot reports before any row is walked.
struct SnapshotView {
    dirty: Dirty,
    cols: u16,
    rows: u16,
    cursor: Option<CursorState>,
}

/// Read the per-sample facts off one snapshot.
fn read_view(snapshot: &Snapshot<'_, '_>) -> Result<SnapshotView, RecordError> {
    let dirty = snapshot.dirty().map_err(|err| replay_err("dirty", &err))?;
    let cols = snapshot.cols().map_err(|err| replay_err("cols", &err))?;
    let rows = snapshot.rows().map_err(|err| replay_err("rows", &err))?;
    let cursor = read_cursor(snapshot, cols, rows)?;
    Ok(SnapshotView {
        dirty,
        cols,
        rows,
        cursor,
    })
}

/// Project every row of the snapshot into `frame`, returning the inclusive
/// band of rows libghostty reported dirty.
fn project_rows(
    rows_iter: &mut RowIterator<'static>,
    cells_iter: &mut CellIterator<'static>,
    snapshot: &Snapshot<'static, '_>,
    cols: u16,
    rows: u16,
    frame: &mut RenderedFrame,
) -> Result<Option<(u16, u16)>, RecordError> {
    let mut band: Option<(u16, u16)> = None;
    let mut row_iter = rows_iter
        .update(snapshot)
        .map_err(|err| replay_err("rows", &err))?;
    let mut row_index: u16 = 0;
    while let Some(row) = row_iter.next() {
        if row_index >= rows {
            break;
        }
        if row.dirty().map_err(|err| replay_err("row dirty", &err))? {
            band = Some(widen(band, row_index));
        }
        project_row(cells_iter, row, row_index, cols, frame)?;
        row.set_dirty(false)
            .map_err(|err| replay_err("row set_dirty", &err))?;
        row_index = row_index.saturating_add(1);
    }
    Ok(band)
}

/// Project one row's cells into `frame`; every row is projected, dirty or not.
fn project_row(
    cells_iter: &mut CellIterator<'static>,
    row: &RowIteration<'static, '_>,
    row_index: u16,
    cols: u16,
    frame: &mut RenderedFrame,
) -> Result<(), RecordError> {
    let mut col: u16 = 0;
    let mut cell_iter = cells_iter
        .update(row)
        .map_err(|err| replay_err("cells", &err))?;
    while let Some(cell) = cell_iter.next() {
        if col >= cols {
            break;
        }
        let (grapheme, style) = project_cell(cell)?;
        if let Some(dst) = frame.cell_mut(row_index, col) {
            dst.grapheme = grapheme;
            dst.style = style;
        }
        col = col.saturating_add(1);
    }
    Ok(())
}

/// Widen `band` to cover the rows the cursor left and now occupies (a
/// visibility-only toggle dirties no row).
fn band_with_cursor_rows(
    band: Option<(u16, u16)>,
    last: Option<&CursorState>,
    current: Option<&CursorState>,
    rows: u16,
) -> Option<(u16, u16)> {
    let mut band = band;
    for row in [last, current]
        .into_iter()
        .flatten()
        .map(|state| state.y)
        .filter(|row| *row < rows)
    {
        band = Some(widen(band, row));
    }
    band
}

/// Wrap a libghostty failure with the operation that produced it.
fn replay_err(what: &str, err: &libghostty_vt::Error) -> RecordError {
    RecordError::Replay(format!("{what}: {err}"))
}

/// libghostty's RGB triple as the plain array the rasterizer indexes.
const fn rgb(color: RgbColor) -> [u8; 3] {
    [color.r, color.g, color.b]
}

/// Grow an inclusive `(min, max)` row band to cover `row`.
const fn widen(band: Option<(u16, u16)>, row: u16) -> (u16, u16) {
    match band {
        Some((lo, hi)) => (
            if row < lo { row } else { lo },
            if row > hi { row } else { hi },
        ),
        None => (row, row),
    }
}

/// The viewport cursor, or `None` when there is none on screen.
fn read_cursor(
    snapshot: &Snapshot<'_, '_>,
    cols: u16,
    rows: u16,
) -> Result<Option<CursorState>, RecordError> {
    let Some(view) = snapshot
        .cursor_viewport()
        .map_err(|err| replay_err("cursor", &err))?
    else {
        return Ok(None);
    };
    if view.y >= rows || view.x >= cols {
        return Ok(None);
    }
    Ok(Some(CursorState {
        x: view.x,
        y: view.y,
        visible: snapshot
            .cursor_visible()
            .map_err(|err| replay_err("cursor visible", &err))?,
    }))
}

/// Project one libghostty cell into its `(grapheme, style)` pair.
fn project_cell(cell: &CellIteration<'_, '_>) -> Result<(String, CellStyle), RecordError> {
    let wide = cell
        .raw_cell()
        .map_err(|err| replay_err("raw cell", &err))?
        .wide()
        .map_err(|err| replay_err("cell wide", &err))?;
    let graphemes = cell
        .graphemes()
        .map_err(|err| replay_err("graphemes", &err))?;
    let grapheme = if matches!(wide, CellWide::SpacerTail) {
        // The right half of a wide glyph is the empty string (phux-core's
        // dense convention).
        String::new()
    } else if graphemes.is_empty() {
        " ".to_owned()
    } else {
        graphemes.iter().collect()
    };
    let style = to_cell_style(
        &cell.style().map_err(|err| replay_err("style", &err))?,
        cell.fg_color().map_err(|err| replay_err("fg", &err))?,
        cell.bg_color().map_err(|err| replay_err("bg", &err))?,
    );
    Ok((grapheme, style))
}

/// Project a libghostty cell's `(Style, resolved fg, resolved bg)` into a
/// plain-data [`CellStyle`].
fn to_cell_style(style: &Style, fg: Option<RgbColor>, bg: Option<RgbColor>) -> CellStyle {
    CellStyle {
        bold: style.bold,
        faint: style.faint,
        italic: style.italic,
        underline: !matches!(style.underline, Underline::None),
        blink: style.blink,
        inverse: style.inverse,
        invisible: style.invisible,
        strikethrough: style.strikethrough,
        overline: style.overline,
        fg: cell_color(fg, style.fg_color),
        bg: cell_color(bg, style.bg_color),
    }
}

/// Project a cell colour, keeping a palette index as an index (the
/// rasterizer resolves it through the recording's theme) and falling back to
/// the resolved RGB.
fn cell_color(resolved: Option<RgbColor>, raw: StyleColor) -> CellColor {
    match raw {
        StyleColor::Palette(index) => CellColor::Palette { index: index.0 },
        StyleColor::Rgb(color) => CellColor::Rgb {
            r: color.r,
            g: color.g,
            b: color.b,
        },
        StyleColor::None => resolved.map_or(CellColor::Default, |color| CellColor::Rgb {
            r: color.r,
            g: color.g,
            b: color.b,
        }),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::{Replayer, Sampled};
    use phux_core::screen::CellColor;

    fn sample(replayer: &mut Replayer) -> Option<Sampled> {
        replayer.sample().expect("sample must not fail")
    }

    fn feed_then_sample(replayer: &mut Replayer, bytes: &[u8]) -> Sampled {
        replayer.feed(bytes);
        sample(replayer).expect("input was fed, so the grid is dirty")
    }

    fn glyph_at(sampled: &Sampled, row: u16, col: u16) -> &str {
        &sampled
            .frame
            .cell(row, col)
            .expect("cell in range")
            .grapheme
    }

    fn row_text(sampled: &Sampled, row: u16) -> String {
        (0..sampled.frame.cols)
            .map(|col| glyph_at(sampled, row, col))
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// Rule 2 of the module docs, and the idle guarantee the sampling design
    /// rests on: after the first frame, an idle terminal costs no frames.
    #[test]
    fn first_sample_always_emits_then_idle_emits_nothing() {
        let mut replayer = Replayer::new(80, 24).expect("replayer");
        // `theme()` runs the same render-state update; the latch, not the
        // dirty bit, is what makes the first sample emit.
        let _theme = replayer.theme().expect("theme");
        let first = sample(&mut replayer).expect("the first sample never reports clean");
        assert_eq!((first.frame.cols, first.frame.rows), (80, 24));
        assert_eq!(first.dirty_rows, None, "the first frame is whole-canvas");
        for tick in 0..600 {
            assert!(
                sample(&mut replayer).is_none(),
                "idle sample {tick} emitted"
            );
        }
    }

    /// phux-u8zm / phux-5pyx: after a resize the pooled trio must walk the new
    /// grid, including cells past the old width, and keep earlier content.
    #[test]
    fn resize_rebuilds_the_pool_and_preserves_content() {
        let mut replayer = Replayer::new(4, 2).expect("replayer");
        assert_eq!(row_text(&feed_then_sample(&mut replayer, b"ab"), 0), "ab");
        replayer.resize(8, 3).expect("resize");
        let after = feed_then_sample(&mut replayer, b"\x1b[1;5HX");
        assert_eq!((after.frame.cols, after.frame.rows), (8, 3));
        assert_eq!(after.frame.cells.len(), 8 * 3);
        assert_eq!(row_text(&after, 0), "ab  X");
        assert_eq!(after.dirty_rows, None, "a resize repaints the whole canvas");
        replayer.resize(3, 2).expect("resize");
        let shrunk = sample(&mut replayer).expect("resize is dirty");
        assert_eq!(row_text(&shrunk, 0), "ab", "content survives a shrink");
    }

    /// A palette index must survive as an index, not be flattened to the
    /// capture-time RGB.
    #[test]
    fn sgr_palette_index_stays_cellcolor_palette() {
        let mut replayer = Replayer::new(10, 3).expect("replayer");
        let sampled = feed_then_sample(&mut replayer, b"\x1b[38;5;42mX");
        let cell = sampled.frame.cell(0, 0).expect("cell");
        assert_eq!(cell.style.fg, CellColor::Palette { index: 42 });
    }

    /// A hidden cursor reported visible would leave an inverted block on
    /// every frame of a full-screen TUI.
    #[test]
    fn cursor_is_reported_in_viewport_coords_with_its_visibility() {
        let mut replayer = Replayer::new(20, 6).expect("replayer");
        let shown = feed_then_sample(&mut replayer, b"\x1b[4;7H");
        let cursor = shown.frame.cursor.expect("a visible cursor");
        assert_eq!((cursor.x, cursor.y, cursor.visible), (6, 3, true));

        // DECTCEM off dirties no cell; the frame exists because `sample`
        // compares the cursor too.
        let hidden = feed_then_sample(&mut replayer, b"\x1b[?25l");
        assert_eq!(hidden.dirty_rows, Some((3, 3)));
        assert!(!hidden.frame.cursor.expect("position still known").visible);
        assert!(
            sample(&mut replayer).is_none(),
            "a settled cursor is not a change"
        );
    }

    #[test]
    fn theme_does_not_swallow_a_pending_frame() {
        let mut replayer = Replayer::new(10, 3).expect("replayer");
        let _first = sample(&mut replayer).expect("first emits");
        replayer.feed(b"pending");
        let theme = replayer.theme().expect("theme");
        assert_ne!(theme.fg, theme.bg);
        let sampled = sample(&mut replayer).expect("bytes fed before theme() still emit");
        assert_eq!(row_text(&sampled, 0), "pending");
    }

    #[test]
    fn zero_dimensions_are_rejected_by_name() {
        let err = Replayer::new(0, 24).expect_err("zero cols must be refused");
        assert!(err.to_string().contains("0x24"), "{err}");
        assert!(Replayer::new(80, 0).is_err(), "zero rows must be refused");
    }

    /// Sub-rectangle encoding depends on narrow bands after the first frame.
    #[test]
    fn dirty_rows_narrows_to_the_touched_rows() {
        let mut replayer = Replayer::new(20, 10).expect("replayer");
        let _first = sample(&mut replayer).expect("first emits");
        // A cursor move dirties the row it left and the row it entered.
        let moved = feed_then_sample(&mut replayer, b"\x1b[6;1H");
        assert_eq!(moved.dirty_rows, Some((0, 5)));
        let written = feed_then_sample(&mut replayer, b"X");
        assert_eq!(glyph_at(&written, 5, 0), "X");
        assert_eq!(written.dirty_rows, Some((5, 5)));
    }
}
