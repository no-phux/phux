//! Offline VT replay: captured bytes back into `RenderedFrame`s.
//!
//! The [`Replayer`] owns a private libghostty `Terminal` and feeds the
//! captured byte stream into it, sampling the grid on the render driver's
//! fixed clock. Two structural rules hold this module together, and both are
//! already documented in-repo at `crates/phux-server/src/grid/synthesizer.rs`
//! where they cost two CI flakes to learn:
//!
//! 1. `RenderState::update` **consumes** the terminal's dirty bits — it
//!    drains them into that one render state, where they stay until that
//!    state clears them. The `Terminal` is therefore constructed internally,
//!    exactly one `RenderState` lives for the replayer's whole life, and no
//!    accessor ever hands out a `&Terminal`. A second observer of the same
//!    terminal takes the bits this one needed, dirty-based frame coalescing
//!    silently drops frames, and the export shows stale content.
//! 2. [`Replayer::sample`] returns `Some` on its **first** call regardless of
//!    the dirty bit. A recording that opens on a settled screen would
//!    otherwise render zero frames.
//!
//! # A third copy of one projection, knowingly
//!
//! [`Replayer::sample`] projects libghostty cells into
//! [`phux_core::screen::RenderedCell`]s. That projection already exists twice:
//! `crates/phux-client/src/attach/render.rs` (`render_at_cells` plus its
//! `to_cell_style` / `cell_color` helpers) walks it client-side to answer
//! `phux snapshot --rendered`, and `crates/phux-server/src/grid/synthesizer.rs`
//! walks it server-side for `--cells`. The three must agree cell-for-cell: a
//! recording, a rendered snapshot, and a cell snapshot of the same screen
//! describe the same glyph identically, or one of them is lying.
//!
//! They are not shared because the only crate all three could import from is
//! `phux-core`, and moving the projection there would force the domain crate
//! to take a `libghostty-vt` dependency — a much worse trade than three
//! ~30-line walks that a single test failure catches. Change one, change all
//! three.

use libghostty_vt::render::{CellIteration, CellIterator, Dirty, RowIterator, Snapshot};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{RgbColor, Style, StyleColor, Underline};
use libghostty_vt::{RenderState, Terminal as GhosttyTerminal, TerminalOptions};
use phux_core::screen::{CellColor, CellStyle, CursorState, RenderedFrame};

use crate::error::RecordError;
use crate::raster::Theme;

/// One sampled frame plus the row band that changed since the last sample.
#[derive(Debug, Clone)]
pub struct Sampled {
    /// The grid as dense cells.
    pub frame: RenderedFrame,
    /// Inclusive `(min_row, max_row)` of the rows that changed, or `None`
    /// when everything changed (which includes the first frame). The encoder
    /// turns this into a sub-rectangle so an idle screen with one blinking
    /// prompt costs a few rows of pixels instead of a whole canvas.
    pub dirty_rows: Option<(u16, u16)>,
}

/// Replays a captured byte stream through a private terminal emulator.
///
/// Construct one, [`feed`](Self::feed) it the cast's `"o"` payloads in order,
/// and [`sample`](Self::sample) on the export's fixed clock. The emulator,
/// its render state, and both iterators are owned here and never escape —
/// see rule 1 in the module docs for why that is structural rather than
/// stylistic.
#[derive(Debug)]
pub struct Replayer {
    /// The private emulator. Never handed out, not even behind a shared
    /// reference: a second `RenderState` observing it would consume the
    /// dirty bits this replayer's frame coalescing depends on.
    term: GhosttyTerminal<'static, 'static>,
    /// The one and only render state, alive for the replayer's whole life.
    state: RenderState<'static>,
    /// Pooled row iterator, reused across samples rather than reallocated.
    rows: RowIterator<'static>,
    /// Pooled cell iterator, likewise.
    cells: CellIterator<'static>,
    /// Whether any sample has been taken yet; see rule 2 in the module docs.
    sampled_once: bool,
    /// The cursor as of the last emitted frame. Compared on every sample
    /// because a `DECTCEM` visibility toggle changes what the frame looks
    /// like without dirtying a single cell.
    last_cursor: Option<CursorState>,
}

impl Replayer {
    /// Build a replayer over a fresh `cols` x `rows` terminal.
    ///
    /// Scrollback is zero: an export shows the viewport, and retaining
    /// history would cost memory proportional to the recording for pixels
    /// that are never drawn.
    ///
    /// Rejects a zero dimension rather than clamping it. A cast header
    /// claiming a zero-width terminal is malformed, and silently replaying it
    /// at 1x1 would produce a plausible-looking but wrong export instead of a
    /// message naming the broken input.
    pub fn new(cols: u16, rows: u16) -> Result<Self, RecordError> {
        if cols == 0 || rows == 0 {
            return Err(RecordError::Replay(format!(
                "terminal dimensions must be non-zero, got {cols}x{rows}"
            )));
        }
        Ok(Self {
            term: GhosttyTerminal::new(TerminalOptions {
                cols,
                rows,
                max_scrollback: 0,
            })
            .map_err(|err| replay_err("terminal construction", &err))?,
            state: RenderState::new().map_err(|err| replay_err("render state", &err))?,
            rows: RowIterator::new().map_err(|err| replay_err("row iterator", &err))?,
            cells: CellIterator::new().map_err(|err| replay_err("cell iterator", &err))?,
            sampled_once: false,
            last_cursor: None,
        })
    }

    /// Feed captured bytes into the emulator.
    ///
    /// Infallible by design: libghostty's `vt_write` logs malformed input
    /// rather than failing, which is exactly right when replaying a stream
    /// that was captured from an untrusted process. A recording of a program
    /// that emitted garbage still exports; it just exports the garbage.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.term.vt_write(bytes);
    }

    /// Resize the replayed terminal, as an asciicast `"r"` event asks.
    ///
    /// Cell pixel dimensions are zero: nothing in the export path reads them,
    /// and reporting a made-up cell size to the emulator would put a wrong
    /// answer into any in-band size report the replayed program asked for.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), RecordError> {
        self.term
            .resize(cols, rows, 0, 0)
            .map_err(|err| replay_err("resize", &err))
    }

    /// Sample the grid, or `None` when nothing changed since the last call.
    ///
    /// `None` costs the animation no frame at all — the driver folds the
    /// period into the previous frame's delay — so an idle terminal is free.
    /// The **first** call never returns `None`: see rule 2 in the module
    /// docs.
    ///
    /// The returned frame is always complete, whatever
    /// [`Sampled::dirty_rows`] says. The dirty band is an encoder hint for
    /// sub-rectangle frames, never a statement about which cells are
    /// populated.
    ///
    /// A cursor change counts as a change even when libghostty reports the
    /// grid clean. Hiding the cursor (`DECTCEM` off) touches no cell, so a
    /// pure dirty-bit test would leave an inverted block sitting on every
    /// remaining frame of a full-screen TUI that had asked for it to go away.
    pub fn sample(&mut self) -> Result<Option<Sampled>, RecordError> {
        // Rule 2 of the module docs, latched in our own state rather than
        // inferred from the emulator: whether a frame has ever been emitted is
        // not something the dirty bit can answer, and a caller who read the
        // theme first would otherwise get an empty export.
        let first = !self.sampled_once;
        let snapshot = self
            .state
            .update(&self.term)
            .map_err(|err| replay_err("snapshot", &err))?;
        let dirty = snapshot.dirty().map_err(|err| replay_err("dirty", &err))?;
        let cols = snapshot.cols().map_err(|err| replay_err("cols", &err))?;
        let rows = snapshot.rows().map_err(|err| replay_err("rows", &err))?;
        let cursor = read_cursor(&snapshot, cols, rows)?;
        let cursor_changed = cursor != self.last_cursor;
        if !first && !cursor_changed && matches!(dirty, Dirty::Clean) {
            return Ok(None);
        }
        self.sampled_once = true;

        let mut frame = RenderedFrame::blank(cols, rows);

        // A first frame and a `Dirty::Full` are both "everything moved"; the
        // encoder wants a whole-canvas frame for those, which `None` means.
        let whole_canvas = first || matches!(dirty, Dirty::Full);
        let mut band: Option<(u16, u16)> = None;

        let mut row_iter = self
            .rows
            .update(&snapshot)
            .map_err(|err| replay_err("rows", &err))?;
        let mut row_index: u16 = 0;
        while let Some(row) = row_iter.next() {
            if row_index >= rows {
                break;
            }
            if row.dirty().map_err(|err| replay_err("row dirty", &err))? {
                band = Some(widen(band, row_index));
            }
            // Every row is projected, dirty or not: the frame is dense and
            // whole, and the caller may be building a keyframe.
            let mut col: u16 = 0;
            let mut cell_iter = self
                .cells
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
            // Clear the per-row dirty bit after reading it, per the
            // libghostty contract the client renderer follows at
            // `attach/render.rs`'s `render_at_inner`. Leaving it set makes
            // every subsequent sample report the row dirty forever, which
            // defeats sub-rectangle encoding.
            row.set_dirty(false)
                .map_err(|err| replay_err("row set_dirty", &err))?;
            row_index = row_index.saturating_add(1);
        }

        // The cursor cell is repainted (inverted) by the rasterizer, so both
        // the row it left and the row it now occupies belong in the band.
        // libghostty already marks a positional move dirty; this covers the
        // visibility-only toggle, which it does not.
        if cursor_changed {
            for row in [self.last_cursor.as_ref(), cursor.as_ref()]
                .into_iter()
                .flatten()
                .map(|state| state.y)
                .filter(|row| *row < rows)
            {
                band = Some(widen(band, row));
            }
        }
        self.last_cursor.clone_from(&cursor);
        frame.cursor = cursor;

        // Clear the snapshot-level bit too, last, once everything has been
        // read. libghostty's dirty state is *sticky*: `RenderState::update`
        // drains the terminal's bits into the render state and they stay set
        // until this call, so a replayer that cleared only the per-row bits
        // would see `Dirty::Full` on every sample forever, emit a frame per
        // period for a screen nobody touched, and turn a ten-minute idle
        // recording into a ten-minute film. Both clears are required; the
        // client renderer pairs them the same way at `attach/render.rs`'s
        // `render_at_inner`, as does the server synthesizer's `mark_synced`.
        snapshot
            .set_dirty(Dirty::Clean)
            .map_err(|err| replay_err("set_dirty", &err))?;

        Ok(Some(Sampled {
            frame,
            // A `Dirty::Partial` that touched no row and moved no cursor
            // falls through to `None` — a whole-canvas frame is always a
            // correct answer, just an unoptimised one.
            dirty_rows: if whole_canvas { None } else { band },
        }))
    }

    /// The terminal's own color table.
    ///
    /// This reads through the same `RenderState::update` that [`Self::sample`]
    /// does, which **drains the terminal's dirty bits into the render
    /// state**. It does not lose a pending frame — the state accumulates
    /// until a sample clears it, which is exactly why [`Self::sample`] pairs
    /// its per-row clears with a snapshot-level one — but it is still not a
    /// free call, and the render driver reads it immediately after the first
    /// `sample()` where the answer is settled and cheap.
    ///
    /// The cursor color is optional on the wire — a terminal that never set
    /// one reports `None` — and falls back to the foreground, which is what
    /// an unstyled block cursor looks like.
    pub fn theme(&mut self) -> Result<Theme, RecordError> {
        let snapshot = self
            .state
            .update(&self.term)
            .map_err(|err| replay_err("snapshot", &err))?;
        let colors = snapshot
            .colors()
            .map_err(|err| replay_err("colors", &err))?;
        let fg = rgb(colors.foreground);
        Ok(Theme {
            fg,
            bg: rgb(colors.background),
            cursor: colors.cursor.map_or(fg, rgb),
            palette: colors.palette.map(rgb),
        })
    }
}

/// Wrap a libghostty failure with the operation that produced it.
///
/// Taken by reference so the call sites stay one line under
/// `clippy::needless_pass_by_value`; the error is only formatted.
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

/// The viewport cursor, or `None` when there is none to draw.
///
/// Off-viewport cursors are dropped rather than clamped: an exported frame
/// should not invent a cursor the screen did not show.
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
///
/// Mirrors `render_at_cells` in `crates/phux-client/src/attach/render.rs`
/// exactly — see the module docs on why this is deliberately the third copy.
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
        // Right half of a wide glyph. The base cell carries the whole
        // cluster, so this column emits nothing and still advances by one —
        // phux-core's dense row-major convention (`screen.rs`'s
        // `RenderedCell`). Emitting a space here instead would shift every
        // later column on any line containing CJK.
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
///
/// Copied from `to_cell_style` in `crates/phux-client/src/attach/render.rs`,
/// which is itself a mirror of the server synthesizer's `collect_cell`.
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

/// Project a cell color to [`CellColor`], preferring the explicit per-cell
/// `StyleColor` so a palette index keeps its identity, and falling back to
/// the iteration's resolved RGB.
///
/// The identity is load-bearing here, not cosmetic: the rasterizer resolves
/// `CellColor::Palette` through the recording's own theme, so a `SGR 38;5;n`
/// cell exported at one theme and re-exported at another paints the right
/// color both times. Flattening to RGB at this boundary would bake the
/// capture-time palette into every frame.
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
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::{Replayer, Sampled};
    use phux_core::screen::CellColor;

    /// Feed `bytes`, then sample, expecting a frame.
    fn feed_then_sample(replayer: &mut Replayer, bytes: &[u8]) -> Sampled {
        replayer.feed(bytes);
        replayer
            .sample()
            .expect("sample must not fail")
            .expect("input was fed, so the grid is dirty")
    }

    /// The grapheme at `(row, col)` of a sampled frame.
    fn glyph_at(sampled: &Sampled, row: u16, col: u16) -> String {
        sampled
            .frame
            .cell(row, col)
            .expect("cell must be in range")
            .grapheme
            .clone()
    }

    /// The right-trimmed text of one row, for whole-line assertions.
    fn row_text(sampled: &Sampled, row: u16) -> String {
        (0..sampled.frame.cols)
            .map(|col| glyph_at(sampled, row, col))
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn plain_text_lands_in_the_expected_cells() {
        let mut replayer = Replayer::new(20, 5).expect("replayer");
        let sampled = feed_then_sample(&mut replayer, b"hi");
        assert_eq!(glyph_at(&sampled, 0, 0), "h");
        assert_eq!(glyph_at(&sampled, 0, 1), "i");
        assert_eq!(glyph_at(&sampled, 0, 2), " ", "untouched cells stay blank");
    }

    /// Rule 2 of the module docs. A recording that opens on a settled screen
    /// (no output at all before the first sample) must still produce a frame,
    /// or the whole export is empty.
    #[test]
    fn first_sample_emits_even_on_a_clean_terminal() {
        let mut replayer = Replayer::new(10, 3).expect("replayer");
        // Touch the render state before sampling — `theme()` runs the same
        // `RenderState::update` — and feed nothing at all. The `sampled_once`
        // latch, not the dirty bit, is what makes this emit.
        let _theme = replayer.theme().expect("theme");
        let sampled = replayer
            .sample()
            .expect("sample must not fail")
            .expect("the first sample never reports clean");
        assert_eq!(sampled.frame.cols, 10);
        assert_eq!(sampled.frame.rows, 3);
        assert_eq!(
            sampled.dirty_rows, None,
            "the first frame is a whole-canvas frame",
        );
    }

    #[test]
    fn second_sample_with_no_input_returns_none() {
        let mut replayer = Replayer::new(10, 3).expect("replayer");
        let _first = replayer.sample().expect("sample").expect("first emits");
        assert!(
            replayer.sample().expect("sample").is_none(),
            "an idle terminal must cost the animation no frame",
        );
    }

    /// The cost guarantee the whole sampling design rests on: a recording of
    /// a session that sat idle for a minute must not cost 600 frames at
    /// 10 fps. One clean sample proving it would be a coincidence; a run of
    /// them is the property.
    #[test]
    fn a_long_idle_run_emits_no_frames_at_all() {
        let mut replayer = Replayer::new(80, 24).expect("replayer");
        let _first = replayer.sample().expect("sample").expect("first emits");
        for tick in 0..600 {
            assert!(
                replayer.sample().expect("sample").is_none(),
                "idle sample {tick} emitted a frame",
            );
        }
    }

    #[test]
    fn second_sample_after_input_returns_some() {
        let mut replayer = Replayer::new(10, 3).expect("replayer");
        let _first = replayer.sample().expect("sample").expect("first emits");
        let sampled = feed_then_sample(&mut replayer, b"x");
        assert_eq!(glyph_at(&sampled, 0, 0), "x");
    }

    #[test]
    fn resize_changes_reported_frame_dims() {
        let mut replayer = Replayer::new(10, 3).expect("replayer");
        let first = replayer.sample().expect("sample").expect("first emits");
        assert_eq!((first.frame.cols, first.frame.rows), (10, 3));
        replayer.resize(40, 12).expect("resize");
        let after = replayer
            .sample()
            .expect("sample")
            .expect("a resize is a change");
        assert_eq!((after.frame.cols, after.frame.rows), (40, 12));
        assert_eq!(
            after.frame.cells.len(),
            40 * 12,
            "the dense grid grows with the terminal",
        );
        assert_eq!(
            after.dirty_rows, None,
            "a resize repaints the whole canvas, so the encoder gets no band",
        );
    }

    /// A cast's `"r"` event lands mid-stream, so the content written before
    /// it has to survive. Every sample re-reads every row from the live grid
    /// rather than diffing against a cached body, which is what keeps the
    /// post-resize frame honest.
    #[test]
    fn resize_preserves_already_written_content() {
        let mut replayer = Replayer::new(10, 4).expect("replayer");
        let before = feed_then_sample(&mut replayer, b"hello");
        assert_eq!(row_text(&before, 0), "hello");
        replayer.resize(30, 8).expect("resize");
        let after = replayer.sample().expect("sample").expect("resize is dirty");
        assert_eq!(row_text(&after, 0), "hello", "content survives a grow");
        replayer.resize(8, 4).expect("resize");
        let shrunk = replayer.sample().expect("sample").expect("resize is dirty");
        assert_eq!(row_text(&shrunk, 0), "hello", "content survives a shrink");
    }

    #[test]
    fn sgr_truecolor_becomes_cellcolor_rgb() {
        let mut replayer = Replayer::new(10, 3).expect("replayer");
        let sampled = feed_then_sample(&mut replayer, b"\x1b[38;2;10;20;30mX");
        let cell = sampled.frame.cell(0, 0).expect("cell");
        assert_eq!(cell.grapheme, "X");
        assert_eq!(
            cell.style.fg,
            CellColor::Rgb {
                r: 10,
                g: 20,
                b: 30
            },
        );
    }

    /// The identity guard: a palette index must survive as an index, not be
    /// flattened into the RGB the capture-time theme happened to resolve it
    /// to. See `cell_color`'s doc comment.
    #[test]
    fn sgr_palette_index_stays_cellcolor_palette() {
        let mut replayer = Replayer::new(10, 3).expect("replayer");
        let sampled = feed_then_sample(&mut replayer, b"\x1b[38;5;42mX");
        let cell = sampled.frame.cell(0, 0).expect("cell");
        assert_eq!(
            cell.style.fg,
            CellColor::Palette { index: 42 },
            "a palette index must not be resolved to RGB at this boundary",
        );
    }

    #[test]
    fn bold_and_underline_survive_to_cellstyle() {
        let mut replayer = Replayer::new(10, 3).expect("replayer");
        let sampled = feed_then_sample(&mut replayer, b"\x1b[1;4mB");
        let style = &sampled.frame.cell(0, 0).expect("cell").style;
        assert!(style.bold, "SGR 1 must survive");
        assert!(style.underline, "SGR 4 must survive");
        assert!(!style.italic, "nothing else may be invented");
    }

    /// phux-core's dense convention: the base cell carries the whole cluster
    /// and the spacer tail carries the empty string, so a consumer can
    /// reconstruct exact widths. Getting this wrong shifts every later column
    /// on any line containing CJK.
    #[test]
    fn cjk_wide_cell_tail_grapheme_is_empty_string() {
        let mut replayer = Replayer::new(10, 3).expect("replayer");
        let sampled = feed_then_sample(&mut replayer, "世".as_bytes());
        assert_eq!(glyph_at(&sampled, 0, 0), "世");
        assert_eq!(
            glyph_at(&sampled, 0, 1),
            "",
            "the spacer tail emits nothing and still occupies its column",
        );
    }

    /// The composited `--rec` stream is exactly this shape: the client paints
    /// into the alt screen and addresses cells absolutely.
    #[test]
    fn alt_screen_enter_and_cup_replay_correctly() {
        let mut replayer = Replayer::new(10, 8).expect("replayer");
        let sampled = feed_then_sample(&mut replayer, b"\x1b[?1049h\x1b[5;3HX");
        // CUP is 1-based; row 5 col 3 is index (4, 2).
        assert_eq!(glyph_at(&sampled, 4, 2), "X");
    }

    /// The rasterizer inverts the cursor cell, so the cursor has to arrive in
    /// viewport coordinates and carry its DECTCEM visibility. A hidden cursor
    /// reported as visible would put an inverted block in the middle of every
    /// exported frame of a full-screen TUI.
    #[test]
    fn cursor_is_reported_in_viewport_coords_with_its_visibility() {
        let mut replayer = Replayer::new(20, 6).expect("replayer");
        // CUP row 4 col 7, 1-based, so the zero-based viewport cursor is (6, 3).
        let shown = feed_then_sample(&mut replayer, b"\x1b[4;7H");
        let cursor = shown.frame.cursor.expect("a visible cursor");
        assert_eq!((cursor.x, cursor.y), (6, 3));
        assert!(cursor.visible, "DECTCEM is on by default");

        // DECTCEM off dirties no cell, so this frame exists only because
        // `sample` compares the cursor as well as the dirty bit. Without that
        // comparison the cursor block would linger on every later frame.
        let hidden = feed_then_sample(&mut replayer, b"\x1b[?25l");
        assert_eq!(
            hidden.dirty_rows,
            Some((3, 3)),
            "the band must cover the row the cursor sits on",
        );
        let cursor = hidden.frame.cursor.expect("position still known");
        assert!(!cursor.visible, "DECTCEM off must survive to the frame");

        // And once it has been reported, it stops costing frames.
        assert!(
            replayer.sample().expect("sample").is_none(),
            "a settled hidden cursor is not a change",
        );
    }

    #[test]
    fn theme_reports_a_256_entry_palette() {
        let mut replayer = Replayer::new(10, 3).expect("replayer");
        let theme = replayer.theme().expect("theme");
        assert_eq!(theme.palette.len(), 256);
        assert_ne!(
            theme.fg, theme.bg,
            "a terminal whose fg equals its bg would export a blank film",
        );
    }

    /// The dirty state libghostty accumulates is sticky until a sample
    /// clears it, so reading the theme between two samples cannot lose the
    /// frame in between. The render driver reads it right after the first
    /// sample anyway; this pins the safety net so a future reordering of that
    /// call does not silently drop a frame.
    #[test]
    fn theme_does_not_swallow_a_pending_frame() {
        let mut replayer = Replayer::new(10, 3).expect("replayer");
        let _first = replayer.sample().expect("sample").expect("first emits");
        replayer.feed(b"pending");
        let _theme = replayer.theme().expect("theme");
        let sampled = replayer
            .sample()
            .expect("sample")
            .expect("the bytes fed before theme() must still produce a frame");
        assert_eq!(row_text(&sampled, 0), "pending");
    }

    /// A malformed cast claiming a zero-width terminal must be named, not
    /// silently replayed at some invented size.
    #[test]
    fn zero_dimensions_are_rejected_by_name() {
        let err = Replayer::new(0, 24).expect_err("zero cols must be refused");
        assert!(
            format!("{err}").contains("0x24"),
            "the message must name the offending dimensions, got: {err}",
        );
        assert!(Replayer::new(80, 0).is_err(), "zero rows must be refused");
    }

    /// Sub-rectangle encoding depends on this: after the opening whole-canvas
    /// frame, touching one row must report a one-row band, not the canvas.
    #[test]
    fn dirty_rows_narrows_to_the_touched_rows() {
        let mut replayer = Replayer::new(20, 10).expect("replayer");
        let first = replayer.sample().expect("sample").expect("first emits");
        assert_eq!(first.dirty_rows, None, "the opening frame is whole-canvas");
        // CUP to row 6 (index 5). Moving the cursor dirties both the row it
        // left and the row it entered — the cursor is painted, so both rows
        // genuinely change. Asserting it here keeps the band honest.
        let moved = feed_then_sample(&mut replayer, b"\x1b[6;1H");
        assert_eq!(
            moved.dirty_rows,
            Some((0, 5)),
            "a cursor move dirties the row it left and the row it entered",
        );
        // Now the cursor is settled on row 5, so writing there touches that
        // row and nothing else.
        let sampled = feed_then_sample(&mut replayer, b"X");
        assert_eq!(glyph_at(&sampled, 5, 0), "X");
        assert_eq!(
            sampled.dirty_rows,
            Some((5, 5)),
            "only the written row changed",
        );
    }
}
