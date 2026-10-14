use std::{cell::RefCell, rc::Rc, sync::Arc, time::Instant};

use super::{Settings, geometry::Geometry};
use gpui::prelude::*;
use gpui::{
    Bounds, ContentMask, FontFeatures, Hsla, Pixels, ShapedLine, SharedString, TextAlign, fill,
    point, px, size,
};
use phux_client_core::grid::{
    CELL_BLINK, CELL_BOLD, CELL_FAINT, CELL_INVERSE, CELL_INVISIBLE, CELL_ITALIC, CELL_OVERLINE,
    CELL_SELECTED, CELL_STRIKETHROUGH, COLOR_KIND_DEFAULT, COLOR_KIND_PALETTE,
};
use phux_client_runtime::publication::{
    Cell, CellMetadata, CursorStyle, CursorWidth, GridFrame, Rgb,
};

#[derive(Clone, Default)]
pub struct Observation {
    pub view_id: Option<phux_client_runtime::ViewId>,
    pub frame: Option<Arc<GridFrame>>,
    /// The connection epoch `frame` was proven current under this draw.
    pub epoch: Option<u64>,
    pub geometry: Geometry,
    /// Where each glyph landed; recorded only for `terminal-fixtures`.
    pub glyphs: Vec<GlyphObservation>,
    pub error: Option<String>,
    pub prepare_micros: u128,
    pub paint_micros: u128,
}

#[derive(Clone, Copy)]
pub struct GlyphObservation {
    pub row: u16,
    pub col: u16,
    pub bounds: Bounds<Pixels>,
    pub origin: gpui::Point<Pixels>,
    pub baseline: Pixels,
    pub foreground: Hsla,
}

/// One cell shaped alone: anything that is not opaque printable ASCII.
#[derive(Clone)]
struct Glyph {
    line: ShapedLine,
    bounds: Bounds<Pixels>,
    opacity: f32,
}

/// Consecutive cells of one row holding printable ASCII in one font and
/// colour. Shaped once, with each column forced to the cell width, and
/// painted as that single line. GPUI's per-glyph paint is the slow path.
#[derive(Clone)]
struct GlyphRun {
    bounds: Bounds<Pixels>,
    line: ShapedLine,
}

#[derive(Clone)]
enum Ink {
    Glyph(Box<Glyph>),
    // ShapedLine carries a 32-wide decoration buffer, so the run stays boxed.
    Run(Box<GlyphRun>),
}

/// One painted quad tagged with the row that produced it, so a later frame
/// can drop that row and keep the rest.
#[derive(Clone, Copy)]
struct RowQuad {
    row: u16,
    bounds: Bounds<Pixels>,
    color: Hsla,
}

/// One shaped ink tagged with its row.
#[derive(Clone)]
struct RowInk {
    row: u16,
    ink: Ink,
}

/// How one cell's text is drawn.
struct TextStyle {
    font: gpui::Font,
    color: Hsla,
    opacity: f32,
}

/// The run being collected: cells `start..start + text.len()` of `row`.
struct PendingRun {
    row: u16,
    start: u16,
    text: String,
    /// Index into [`Fonts`]: bold and italic bits.
    style: usize,
    color: Hsla,
}

struct OpacityLayer {
    element: gpui::AnyElement,
    result: Rc<RefCell<Option<Result<(), String>>>>,
}

/// Cells `start..end` of `row`, all painted `color`.
struct BackgroundRun {
    row: u16,
    start: u16,
    end: u16,
    color: Hsla,
}

#[derive(Clone, Copy)]
struct CellColors {
    foreground: Hsla,
    background: Hsla,
    underline: Hsla,
    opacity: f32,
}

/// The four faces a cell can ask for, indexed by [`style`]: built once per
/// scene so a cell costs an index, not a font clone and comparison.
struct Fonts {
    cell: [gpui::Font; 4],
    /// The same faces with ligatures off, for multi-cell runs.
    run: [gpui::Font; 4],
}

impl Fonts {
    fn new(base: &gpui::Font) -> Self {
        let faces = [
            base.clone(),
            base.clone().bold(),
            base.clone().italic(),
            base.clone().bold().italic(),
        ];
        // Cells never join: a ligature across them would misplace every
        // later glyph of the run. Per-cell shaping never formed one either.
        let features = FontFeatures(Arc::new(vec![("calt".into(), 0), ("liga".into(), 0)]));
        let run = faces.clone().map(|mut font| {
            font.features = features.clone();
            font
        });
        Self { cell: faces, run }
    }
}

/// Everything painting one frame needs that depends only on its inputs: the
/// frame, the element's bounds and scale, and the paint settings. A window
/// refresh, resize or sibling change repaints terminals whose scene did not
/// change; [`prepare`] then reuses it instead of walking and shaping every
/// cell again. A new frame keeps the shaped rows whose cells still paint the
/// same and reshapes only the rows that changed, plus a solid cursor's old
/// and new rows. A scrolled frame moves the kept rows instead of shaping them
/// again.
pub(super) struct Scene {
    frame: Arc<GridFrame>,
    bounds: Bounds<Pixels>,
    scale: f32,
    settings: Settings,
    geometry: Geometry,
    background: Hsla,
    backgrounds: Vec<RowQuad>,
    inks: Vec<RowInk>,
    /// Underlines, strike, and overline, in row order.
    decorations: Vec<RowQuad>,
    /// Solid block cursor, painted over cell backgrounds and under glyphs.
    cursor_blocks: Vec<(Bounds<Pixels>, Hsla)>,
    /// Bar, underline, and hollow cursors, painted over glyphs.
    cursor_overlays: Vec<(Bounds<Pixels>, Hsla)>,
    glyphs: Vec<GlyphObservation>,
}

impl Scene {
    /// Whether this scene paints `frame` in `bounds` exactly as a fresh one
    /// would. The frame is compared by identity: a publication is immutable,
    /// and the scene's own reference keeps its allocation from being reused.
    fn reusable(
        &self,
        frame: &Arc<GridFrame>,
        bounds: Bounds<Pixels>,
        scale: f32,
        settings: &Settings,
    ) -> bool {
        Arc::ptr_eq(&self.frame, frame)
            && self.bounds == bounds
            && self.scale == scale
            && self.settings.paints_like(settings)
    }
}

/// The scene a terminal element painted last, kept between draws.
pub(super) type SceneCache = Rc<RefCell<Option<Rc<Scene>>>>;

pub(super) struct Prepared {
    report: Observation,
    /// Painted when there is no scene (no frame, or a stale one), unless the
    /// host paints the background.
    fallback: Option<Hsla>,
    scene: Option<Rc<Scene>>,
    /// One per translucent glyph ink, in ink order. They wrap per-draw GPUI
    /// elements, so they are rebuilt even for a reused scene.
    layers: Vec<OpacityLayer>,
}

pub(super) fn prepare(
    frame: Result<Arc<GridFrame>, String>,
    settings: Settings,
    bounds: Bounds<Pixels>,
    cache: &SceneCache,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> Prepared {
    let started = Instant::now();
    let mut prepared = Prepared {
        report: Observation {
            view_id: None,
            ..Default::default()
        },
        fallback: (!settings.host_fills_background)
            .then(|| settings.background.unwrap_or_else(gpui::black)),
        scene: None,
        layers: Vec::new(),
    };
    match frame {
        Ok(frame) => {
            let scene = scene(frame, settings, bounds, cache, window);
            prepared.layers = opacity_layers(&scene, window, cx);
            prepared.scene = Some(scene);
        }
        Err(error) => {
            // Nothing left to repaint: release the last frame and its scene.
            cache.borrow_mut().take();
            prepared.report.error = Some(error);
        }
    }
    prepared.report.prepare_micros = started.elapsed().as_micros();
    crate::perf::PREPARE.record_elapsed(started);
    prepared
}

/// The cached scene when it still paints these inputs, else a new one.
fn scene(
    frame: Arc<GridFrame>,
    settings: Settings,
    bounds: Bounds<Pixels>,
    cache: &SceneCache,
    window: &gpui::Window,
) -> Rc<Scene> {
    let scale = window.scale_factor();
    if let Some(scene) = cache.borrow().as_ref()
        && scene.reusable(&frame, bounds, scale, &settings)
    {
        crate::perf::PREPARE_REUSED.incr();
        return Rc::clone(scene);
    }
    if let Some(previous) = take_scene(cache)
        && let Some(patched) = patch(
            previous,
            frame.clone(),
            settings.clone(),
            bounds,
            scale,
            window,
        )
    {
        crate::perf::PREPARE_PATCHED.incr();
        let patched = Rc::new(patched);
        *cache.borrow_mut() = Some(Rc::clone(&patched));
        return patched;
    }
    let scene = Rc::new(Builder::new(frame, settings, bounds, window).build(window));
    *cache.borrow_mut() = Some(Rc::clone(&scene));
    scene
}

fn take_scene(cache: &SceneCache) -> Option<Scene> {
    let existing = cache.borrow_mut().take()?;
    match Rc::try_unwrap(existing) {
        Ok(scene) => Some(scene),
        Err(shared) => {
            *cache.borrow_mut() = Some(shared);
            None
        }
    }
}

/// Reshape `frame` from `previous` when the geometry and theme still match.
/// `None` means the caller should build the scene from scratch. `previous`
/// is consumed either way.
fn patch(
    mut previous: Scene,
    frame: Arc<GridFrame>,
    settings: Settings,
    bounds: Bounds<Pixels>,
    scale: f32,
    window: &gpui::Window,
) -> Option<Scene> {
    if !can_patch(&previous, &frame, bounds, scale, &settings) {
        return None;
    }
    let rebuilt = rows_to_rebuild(&previous.frame, &frame, &settings);
    // Hashing both grids costs a full read. Do it only when the in-place
    // diff would reshape most of the screen, which is what a line of output
    // does. A few dirty rows are cheaper to repaint than to hash.
    if worth_scroll_search(rebuilt.len(), frame.rows)
        && let Some(scroll) = scroll_of(&previous.frame, &frame)
    {
        let scrolled = rows_after_scroll(&previous.frame, &frame, &settings, scroll);
        if scrolled.len() < rebuilt.len() {
            shift_scene(&mut previous, scroll);
            return finish_patch(previous, frame, settings, &scrolled, window);
        }
    }
    if rebuilt.len() == usize::from(frame.rows) {
        return None;
    }
    finish_patch(previous, frame, settings, &rebuilt, window)
}

/// True when so many rows changed that a scroll shift can beat reshaping them.
fn worth_scroll_search(rebuilt: usize, rows: u16) -> bool {
    let height = usize::from(rows);
    let slack = height / 8;
    height >= 2 && rebuilt >= height.saturating_sub(slack).max(2)
}

fn finish_patch(
    mut previous: Scene,
    frame: Arc<GridFrame>,
    settings: Settings,
    rebuilt: &[u16],
    window: &gpui::Window,
) -> Option<Scene> {
    if rebuilt.is_empty() {
        let cursor_changed =
            cursor_key(&previous.frame, &settings) != cursor_key(&frame, &settings);
        previous.frame = frame;
        previous.settings = settings;
        if cursor_changed {
            prepare_cursor(&mut previous);
        }
        return Some(previous);
    }
    let geometry = previous.geometry;
    let kept_bounds = previous.bounds;
    let kept_scale = previous.scale;
    let mut builder = Builder::from_kept(frame, settings, geometry, kept_bounds, kept_scale);
    builder.paint_rows(rebuilt, window);
    let fresh = builder.finish();
    Some(assemble(previous, fresh, rebuilt))
}

/// Whole rows the new frame shares with the previous one, moved up or down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scroll {
    /// Content moved toward row 0. The new rows are at the bottom.
    Up(u16),
    /// Content moved toward the last row. The new rows are at the top.
    Down(u16),
}

impl Scroll {
    fn rows(self) -> u16 {
        match self {
            Self::Up(rows) | Self::Down(rows) => rows,
        }
    }
}

fn scroll_of(previous: &GridFrame, next: &GridFrame) -> Option<Scroll> {
    if previous.cols != next.cols || previous.rows != next.rows || next.rows < 2 {
        return None;
    }
    let prev = row_fingerprints(previous);
    let next_rows = row_fingerprints(next);
    if let Some(shift) = matching_shift(&prev, &next_rows) {
        let scroll = Scroll::Up(shift);
        if scroll_holds(previous, next, scroll) {
            return Some(scroll);
        }
    }
    if let Some(shift) = matching_shift(&next_rows, &prev) {
        let scroll = Scroll::Down(shift);
        if scroll_holds(previous, next, scroll) {
            return Some(scroll);
        }
    }
    None
}

fn matching_shift(source: &[u64], moved: &[u64]) -> Option<u16> {
    (1..source.len())
        .find(|shift| rows_aligned(source, moved, *shift))
        .and_then(|shift| u16::try_from(shift).ok())
}

fn rows_aligned(source: &[u64], moved: &[u64], shift: usize) -> bool {
    let Some(source) = source.get(shift..) else {
        return false;
    };
    let kept = moved.len().saturating_sub(shift);
    moved[..kept]
        .iter()
        .zip(source)
        .all(|(left, right)| left == right)
}

fn scroll_holds(previous: &GridFrame, next: &GridFrame, scroll: Scroll) -> bool {
    let shift = usize::from(scroll.rows());
    let kept = usize::from(next.rows).saturating_sub(shift);
    (0..kept).all(|row| {
        let (prev_row, next_row) = scrolled_pair(scroll, row);
        rows_paint_alike(previous, next, prev_row, next_row)
    })
}

fn scrolled_pair(scroll: Scroll, row: usize) -> (u16, u16) {
    let shift = usize::from(scroll.rows());
    let (prev, next) = match scroll {
        Scroll::Up(_) => (row + shift, row),
        Scroll::Down(_) => (row, row + shift),
    };
    (
        u16::try_from(prev).unwrap_or(u16::MAX),
        u16::try_from(next).unwrap_or(u16::MAX),
    )
}

fn rows_after_scroll(
    previous: &GridFrame,
    next: &GridFrame,
    settings: &Settings,
    scroll: Scroll,
) -> Vec<u16> {
    let mut rows = entered_rows(next.rows, scroll);
    if let Some(row) = landed_cursor(previous, settings, scroll) {
        rows.push(row);
    }
    if let Some((row, _)) = solid_head(next, settings) {
        rows.push(row);
    }
    rows.sort_unstable();
    rows.dedup();
    rows
}

fn entered_rows(height: u16, scroll: Scroll) -> Vec<u16> {
    let shift = scroll.rows().min(height);
    match scroll {
        Scroll::Up(_) => (height - shift..height).collect(),
        Scroll::Down(_) => (0..shift).collect(),
    }
}

fn landed_cursor(previous: &GridFrame, settings: &Settings, scroll: Scroll) -> Option<u16> {
    let (row, _) = solid_head(previous, settings)?;
    destination_row(scroll, previous.rows, row)
}

fn destination_row(scroll: Scroll, height: u16, row: u16) -> Option<u16> {
    match scroll {
        Scroll::Up(by) if row >= by => Some(row - by),
        Scroll::Down(by) if row.checked_add(by).is_some_and(|next| next < height) => Some(row + by),
        _ => None,
    }
}

fn shift_scene(scene: &mut Scene, scroll: Scroll) {
    let height = scene.frame.rows;
    let distance = scene.geometry.cell_height * f32::from(scroll.rows());
    let dy = match scroll {
        Scroll::Up(_) => px(0.) - distance,
        Scroll::Down(_) => distance,
    };
    shift_quads(&mut scene.backgrounds, scroll, height, dy);
    shift_quads(&mut scene.decorations, scroll, height, dy);
    shift_inks(&mut scene.inks, scroll, height, dy);
    shift_glyphs(&mut scene.glyphs, scroll, height, dy);
}

fn shift_quads(quads: &mut Vec<RowQuad>, scroll: Scroll, height: u16, dy: Pixels) {
    quads.retain_mut(|quad| shift_row(&mut quad.row, &mut quad.bounds, scroll, height, dy));
}

fn shift_inks(inks: &mut Vec<RowInk>, scroll: Scroll, height: u16, dy: Pixels) {
    inks.retain_mut(|ink| {
        let Some(next) = destination_row(scroll, height, ink.row) else {
            return false;
        };
        ink.row = next;
        ink_bounds(&mut ink.ink).origin.y += dy;
        true
    });
}

fn shift_glyphs(glyphs: &mut Vec<GlyphObservation>, scroll: Scroll, height: u16, dy: Pixels) {
    glyphs.retain_mut(|glyph| {
        let kept = shift_row(&mut glyph.row, &mut glyph.bounds, scroll, height, dy);
        if kept {
            glyph.origin.y += dy;
            glyph.baseline += dy;
        }
        kept
    });
}

fn shift_row(
    row: &mut u16,
    bounds: &mut Bounds<Pixels>,
    scroll: Scroll,
    height: u16,
    dy: Pixels,
) -> bool {
    let Some(next) = destination_row(scroll, height, *row) else {
        return false;
    };
    *row = next;
    bounds.origin.y += dy;
    true
}

fn ink_bounds(ink: &mut Ink) -> &mut Bounds<Pixels> {
    match ink {
        Ink::Glyph(glyph) => &mut glyph.bounds,
        Ink::Run(run) => &mut run.bounds,
    }
}

fn row_fingerprints(frame: &GridFrame) -> Vec<u64> {
    (0..frame.rows)
        .map(|row| row_fingerprint(frame, row))
        .collect()
}

fn row_fingerprint(frame: &GridFrame, row: u16) -> u64 {
    let cols = usize::from(frame.cols);
    let Some(start) = usize::from(row).checked_mul(cols) else {
        return 0;
    };
    let mut hash = 0xcbf29ce484222325_u64;
    for index in start..start + cols {
        hash = mix_cell(hash, frame, index);
    }
    hash
}

fn mix_cell(mut hash: u64, frame: &GridFrame, index: usize) -> u64 {
    let Some(cell) = frame.buffer.cells.get(index) else {
        return mix(hash, 0xffff);
    };
    hash = mix(hash, u64::from(cell.utf8_len));
    hash = mix(hash, u64::from(cell.wide));
    hash = mix(hash, u64::from(cell.flags));
    hash = mix(hash, u64::from(cell.underline));
    for byte in color_bytes(cell) {
        hash = mix(hash, u64::from(byte));
    }
    hash = mix_metadata(hash, frame.buffer.metadata.get(index).copied());
    match text_span(cell, &frame.buffer.utf8) {
        Some(text) => {
            for byte in text {
                hash = mix(hash, u64::from(*byte));
            }
            hash
        }
        None => mix(hash, 0xff),
    }
}

fn mix_metadata(mut hash: u64, metadata: Option<CellMetadata>) -> u64 {
    let Some(metadata) = metadata else {
        return mix(hash, 0);
    };
    hash = mix(hash, u64::from(metadata.foreground_kind));
    hash = mix(hash, u64::from(metadata.foreground_palette_index));
    hash = mix(hash, u64::from(metadata.underline_color_is_default));
    mix(hash, u64::from(metadata.background_color_is_default))
}

fn mix(hash: u64, value: u64) -> u64 {
    hash.wrapping_mul(0x517cc1b727220a95).wrapping_add(value)
}

fn can_patch(
    scene: &Scene,
    frame: &GridFrame,
    bounds: Bounds<Pixels>,
    scale: f32,
    settings: &Settings,
) -> bool {
    scene.bounds == bounds
        && scene.scale == scale
        && scene.frame.cols == frame.cols
        && scene.frame.rows == frame.rows
        && scene.frame.colors == frame.colors
        && scene.settings.paints_like(settings)
}

fn rows_to_rebuild(previous: &GridFrame, next: &GridFrame, settings: &Settings) -> Vec<u16> {
    let old = solid_head(previous, settings);
    let new = solid_head(next, settings);
    (0..next.rows)
        .filter(|row| solid_row_changed(old, new, *row) || !row_paints_alike(previous, next, *row))
        .collect()
}

fn solid_head(frame: &GridFrame, settings: &Settings) -> Option<(u16, u16)> {
    solid_cursor(frame, settings).then(|| cursor_head(frame))
}

fn solid_row_changed(old: Option<(u16, u16)>, new: Option<(u16, u16)>, row: u16) -> bool {
    if old == new {
        return false;
    }
    old.is_some_and(|(at, _)| at == row) || new.is_some_and(|(at, _)| at == row)
}

/// Cursor placement that changes quads without changing cell glyphs: a bar,
/// underline, or hollow cursor, or a width change on an unmoved solid head.
fn cursor_key(frame: &GridFrame, settings: &Settings) -> Option<(u16, u16, CursorStyle, bool)> {
    if !cursor_visible(frame, settings) {
        return None;
    }
    let (row, col) = cursor_head(frame);
    let style = if settings.focused {
        frame.cursor.style
    } else {
        CursorStyle::BlockHollow
    };
    Some((row, col, style, frame.cursor.width.is_wide()))
}

struct RowParts<'a> {
    prev_cells: &'a [Cell],
    next_cells: &'a [Cell],
    prev_meta: &'a [CellMetadata],
    next_meta: &'a [CellMetadata],
}

fn row_paints_alike(previous: &GridFrame, next: &GridFrame, row: u16) -> bool {
    rows_paint_alike(previous, next, row, row)
}

fn rows_paint_alike(previous: &GridFrame, next: &GridFrame, prev_row: u16, next_row: u16) -> bool {
    let Some(parts) = row_slices(previous, next, prev_row, next_row) else {
        return false;
    };
    parts
        .prev_cells
        .iter()
        .zip(parts.next_cells)
        .all(|(left, right)| {
            cell_paints_alike(left, &previous.buffer.utf8, right, &next.buffer.utf8)
        })
        && parts
            .prev_meta
            .iter()
            .zip(parts.next_meta)
            .all(|(left, right)| metadata_matches(left, right))
}

fn row_slices<'a>(
    previous: &'a GridFrame,
    next: &'a GridFrame,
    prev_row: u16,
    next_row: u16,
) -> Option<RowParts<'a>> {
    if previous.cols != next.cols {
        return None;
    }
    let cols = usize::from(previous.cols);
    let prev = row_range(prev_row, cols)?;
    let next_span = row_range(next_row, cols)?;
    Some(RowParts {
        prev_cells: previous.buffer.cells.get(prev.clone())?,
        next_cells: next.buffer.cells.get(next_span.clone())?,
        prev_meta: previous.buffer.metadata.get(prev)?,
        next_meta: next.buffer.metadata.get(next_span)?,
    })
}

fn row_range(row: u16, cols: usize) -> Option<std::ops::Range<usize>> {
    let start = usize::from(row).checked_mul(cols)?;
    let end = start.checked_add(cols)?;
    Some(start..end)
}

fn cell_paints_alike(left: &Cell, left_utf8: &[u8], right: &Cell, right_utf8: &[u8]) -> bool {
    paint_fields_match(left, right) && same_text(left, left_utf8, right, right_utf8)
}

fn paint_fields_match(left: &Cell, right: &Cell) -> bool {
    left.utf8_len == right.utf8_len
        && left.wide == right.wide
        && left.flags == right.flags
        && left.underline == right.underline
        && colors_match(left, right)
}

fn colors_match(left: &Cell, right: &Cell) -> bool {
    color_bytes(left) == color_bytes(right)
}

fn color_bytes(cell: &Cell) -> [u8; 9] {
    [
        cell.foreground_r,
        cell.foreground_g,
        cell.foreground_b,
        cell.background_r,
        cell.background_g,
        cell.background_b,
        cell.underline_r,
        cell.underline_g,
        cell.underline_b,
    ]
}

fn metadata_matches(left: &CellMetadata, right: &CellMetadata) -> bool {
    left.foreground_kind == right.foreground_kind
        && left.foreground_palette_index == right.foreground_palette_index
        && left.underline_color_is_default == right.underline_color_is_default
        && left.background_color_is_default == right.background_color_is_default
}

fn same_text(left: &Cell, left_utf8: &[u8], right: &Cell, right_utf8: &[u8]) -> bool {
    text_span(left, left_utf8) == text_span(right, right_utf8)
}

fn text_span<'a>(cell: &Cell, utf8: &'a [u8]) -> Option<&'a [u8]> {
    let start = usize::try_from(cell.utf8_offset).ok()?;
    let end = start.checked_add(usize::from(cell.utf8_len))?;
    utf8.get(start..end)
}

fn assemble(mut previous: Scene, fresh: Scene, rebuilt: &[u16]) -> Scene {
    previous.frame = fresh.frame;
    previous.settings = fresh.settings;
    previous.background = fresh.background;
    previous.backgrounds = merge_rows(previous.backgrounds, fresh.backgrounds, rebuilt, |quad| {
        quad.row
    });
    previous.inks = merge_rows(previous.inks, fresh.inks, rebuilt, |ink| ink.row);
    previous.decorations = merge_rows(previous.decorations, fresh.decorations, rebuilt, |quad| {
        quad.row
    });
    previous.cursor_blocks = fresh.cursor_blocks;
    previous.cursor_overlays = fresh.cursor_overlays;
    previous.glyphs = merge_rows(previous.glyphs, fresh.glyphs, rebuilt, |glyph| glyph.row);
    previous
}

fn merge_rows<T>(
    previous: Vec<T>,
    fresh: Vec<T>,
    rebuilt: &[u16],
    row_of: impl Fn(&T) -> u16,
) -> Vec<T> {
    let mut kept: Vec<T> = previous
        .into_iter()
        .filter(|item| rebuilt.binary_search(&row_of(item)).is_err())
        .collect();
    kept.extend(fresh);
    kept.sort_by_key(|item| row_of(item));
    kept
}

fn opacity_layers(
    scene: &Scene,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> Vec<OpacityLayer> {
    scene
        .inks
        .iter()
        .filter_map(|ink| match &ink.ink {
            Ink::Glyph(glyph) if glyph.opacity < 1. => {
                Some(glyph.opacity_layer(scene.geometry, window, cx))
            }
            _ => None,
        })
        .collect()
}

/// Walks one frame's cells into a [`Scene`].
struct Builder {
    scene: Scene,
    fonts: Fonts,
    /// Default foreground and background after theme and reverse video.
    defaults: (Hsla, Hsla),
    /// The cell under a solid block cursor, whose glyph takes the background.
    solid_cursor: Option<(u16, u16)>,
    /// The open run of same-coloured cells on one row, merged into one quad.
    run: Option<BackgroundRun>,
    pending: Option<PendingRun>,
}

impl Builder {
    fn new(
        frame: Arc<GridFrame>,
        settings: Settings,
        bounds: Bounds<Pixels>,
        window: &gpui::Window,
    ) -> Self {
        let geometry = Geometry::measure(&settings, bounds, frame.cols, frame.rows, window);
        Self::from_kept(frame, settings, geometry, bounds, window.scale_factor())
    }

    fn from_kept(
        frame: Arc<GridFrame>,
        settings: Settings,
        geometry: Geometry,
        bounds: Bounds<Pixels>,
        scale: f32,
    ) -> Self {
        let defaults = defaults(&frame, &settings);
        let solid_cursor = solid_cursor(&frame, &settings).then(|| cursor_head(&frame));
        Self {
            fonts: Fonts::new(&settings.font),
            defaults,
            solid_cursor,
            run: None,
            pending: None,
            scene: Scene {
                frame,
                bounds,
                scale,
                settings,
                geometry,
                background: defaults.1,
                backgrounds: Vec::new(),
                inks: Vec::new(),
                decorations: Vec::new(),
                cursor_blocks: Vec::new(),
                cursor_overlays: Vec::new(),
                glyphs: Vec::new(),
            },
        }
    }

    fn build(mut self, window: &gpui::Window) -> Scene {
        let frame = Arc::clone(&self.scene.frame);
        for row in 0..frame.rows {
            self.paint_row(&frame, row, window);
        }
        self.finish()
    }

    fn paint_rows(&mut self, rows: &[u16], window: &gpui::Window) {
        let frame = Arc::clone(&self.scene.frame);
        for row in rows {
            self.paint_row(&frame, *row, window);
        }
    }

    fn paint_row(&mut self, frame: &GridFrame, row: u16, window: &gpui::Window) {
        for col in 0..frame.cols {
            self.prepare_cell(frame, row, col, window);
        }
        self.flush_background();
        self.flush_glyph_run(window);
    }

    fn finish(mut self) -> Scene {
        prepare_cursor(&mut self.scene);
        self.scene
    }

    fn prepare_cell(&mut self, frame: &GridFrame, row: u16, col: u16, window: &gpui::Window) {
        let Some(cell) = frame.cell(row, col) else {
            return;
        };
        let geometry = self.scene.geometry;
        let bounds = geometry.cell_bounds(row, col, 1);
        if !bounds.intersects(&geometry.bounds) {
            return;
        }
        let index = usize::from(row) * usize::from(frame.cols) + usize::from(col);
        let metadata = frame
            .buffer
            .metadata
            .get(index)
            .copied()
            .unwrap_or_default();
        let colors = cell_colors(cell, metadata, frame, &self.scene.settings, self.defaults);
        self.background_cell(row, col, colors.background);
        if hidden(cell, &self.scene.settings) {
            return;
        }
        self.prepare_decorations(row, cell, bounds, colors);
        // Both spacer variants contain no glyph. The head's whole grapheme is
        // shaped in one call; force_width would incorrectly advance combining marks.
        if matches!(cell.wide, 2 | 3) {
            return;
        }
        let text = frame.cell_text(row, col);
        if text.is_empty() {
            return;
        }
        let foreground = self.glyph_foreground(row, col, colors);
        let style = style(cell);
        if let [byte] = text
            && cell.wide == 0
            && colors.opacity >= 1.
            && (byte.is_ascii_graphic() || *byte == b' ')
        {
            self.ascii_cell(row, col, *byte, style, foreground, window);
            return;
        }
        self.flush_glyph_run(window);
        let text = String::from_utf8_lossy(text).into_owned();
        let width = if cell.wide == 1 { 2 } else { 1 };
        let style = TextStyle {
            font: self.fonts.cell[style].clone(),
            color: foreground,
            opacity: colors.opacity,
        };
        self.shape_cell((row, col, width), text, style, window);
    }

    /// A glyph under a solid block cursor takes the cell's background.
    fn glyph_foreground(&self, row: u16, col: u16, colors: CellColors) -> Hsla {
        if self.solid_cursor == Some((row, col)) {
            return colors.background;
        }
        colors.foreground
    }

    /// Extend the pending run with one ASCII cell, or start a run. A space
    /// has no ink: it ends the run and is only observed.
    fn ascii_cell(
        &mut self,
        row: u16,
        col: u16,
        byte: u8,
        style: usize,
        color: Hsla,
        window: &gpui::Window,
    ) {
        if let Some(run) = &mut self.pending
            && byte != b' '
            && run.row == row
            && usize::from(run.start) + run.text.len() == usize::from(col)
            && run.color == color
            && run.style == style
        {
            run.text.push(char::from(byte));
            return;
        }
        self.flush_glyph_run(window);
        if byte == b' ' {
            let bounds = self.scene.geometry.cell_bounds(row, col, 1);
            self.observe(row, col, bounds, None, color);
            return;
        }
        self.pending = Some(PendingRun {
            row,
            start: col,
            text: char::from(byte).to_string(),
            style,
            color,
        });
    }

    /// Shape the pending run as one line. A font that does not give exactly
    /// one glyph per byte (a ligature or substitution survived, or a glyph
    /// fell back to emoji) shapes those cells one at a time instead.
    fn flush_glyph_run(&mut self, window: &gpui::Window) {
        let Some(run) = self.pending.take() else {
            return;
        };
        let geometry = self.scene.geometry;
        let len = run.text.len();
        let text = SharedString::from(run.text);
        crate::perf::SHAPED.incr();
        let line = window.text_system().shape_line(
            text.clone(),
            px(self.scene.settings.font_size),
            &[gpui::TextRun {
                len,
                font: self.fonts.run[run.style].clone(),
                color: run.color,
                ..Default::default()
            }],
            Some(geometry.cell_width),
        );
        // A ligature or emoji substitution is not one column per byte.
        // Shape those cells alone so the grid stays one glyph per column.
        if !glyphs_match_bytes(&line, len) {
            for (offset, byte) in text.bytes().enumerate() {
                let style = TextStyle {
                    font: self.fonts.cell[run.style].clone(),
                    color: run.color,
                    opacity: 1.,
                };
                let cell = (
                    run.row,
                    run.start + u16::try_from(offset).unwrap_or(u16::MAX),
                    1,
                );
                self.shape_cell(cell, char::from(byte).to_string(), style, window);
            }
            return;
        }
        for offset in 0..len {
            let col = run.start + u16::try_from(offset).unwrap_or(u16::MAX);
            let bounds = geometry.cell_bounds(run.row, col, 1);
            self.observe(run.row, col, bounds, None, run.color);
        }
        let width = u16::try_from(len).unwrap_or(u16::MAX);
        self.scene.inks.push(RowInk {
            row: run.row,
            ink: Ink::Run(Box::new(GlyphRun {
                bounds: geometry.cell_bounds(run.row, run.start, width),
                line,
            })),
        });
    }

    /// Shape one cell's text alone. `cell` is its row, column and width.
    fn shape_cell(
        &mut self,
        (row, col, width): (u16, u16, u16),
        text: String,
        style: TextStyle,
        window: &gpui::Window,
    ) {
        let TextStyle {
            font,
            color: foreground,
            opacity,
        } = style;
        let bounds = self.scene.geometry.cell_bounds(row, col, width);
        let len = text.len();
        crate::perf::SHAPED.incr();
        let line = window.text_system().shape_line(
            text.into(),
            px(self.scene.settings.font_size),
            &[gpui::TextRun {
                len,
                font,
                color: foreground,
                ..Default::default()
            }],
            None,
        );
        self.observe(row, col, bounds, Some(&line), foreground.opacity(opacity));
        self.scene.inks.push(RowInk {
            row,
            ink: Ink::Glyph(Box::new(Glyph {
                line,
                bounds,
                opacity,
            })),
        });
    }

    /// Record where a cell's glyph lands, in row-major order, for fixtures.
    /// Production builds record nothing: this would otherwise allocate and
    /// retain a record per cell per frame.
    fn observe(
        &mut self,
        row: u16,
        col: u16,
        bounds: Bounds<Pixels>,
        line: Option<&ShapedLine>,
        foreground: Hsla,
    ) {
        if !cfg!(feature = "terminal-fixtures") {
            return;
        }
        let geometry = self.scene.geometry;
        let baseline = geometry.baseline + bounds.origin.y;
        let offset = line.map_or(geometry.baseline, |line| line_baseline(line, geometry));
        self.scene.glyphs.push(GlyphObservation {
            row,
            col,
            bounds,
            origin: point(bounds.origin.x, baseline - offset),
            baseline,
            foreground,
        });
    }

    /// Extend the row's open run, or start one. The whole surface is already
    /// filled with the default background, so those cells add no quad; a
    /// screen of text is then a handful of quads instead of one per cell.
    fn background_cell(&mut self, row: u16, col: u16, color: Hsla) {
        if let Some(run) = &mut self.run
            && run.row == row
            && run.end == col
            && run.color == color
        {
            run.end += 1;
            return;
        }
        self.flush_background();
        if color != self.scene.background {
            self.run = Some(BackgroundRun {
                row,
                start: col,
                end: col + 1,
                color,
            });
        }
    }

    fn flush_background(&mut self) {
        if let Some(run) = self.run.take() {
            let bounds = self
                .scene
                .geometry
                .cell_bounds(run.row, run.start, run.end - run.start);
            self.scene.backgrounds.push(RowQuad {
                row: run.row,
                bounds,
                color: run.color,
            });
        }
    }

    fn prepare_decorations(
        &mut self,
        row: u16,
        cell: &Cell,
        bounds: Bounds<Pixels>,
        mut colors: CellColors,
    ) {
        colors.foreground = colors.foreground.opacity(colors.opacity);
        colors.underline = colors.underline.opacity(colors.opacity);
        let thickness = px(1. / self.scene.geometry.scale);
        let bottom = bounds.bottom() - thickness * 2.;
        match cell.underline {
            1 => self.rule(row, bounds, bottom, thickness, colors.underline),
            2 => {
                self.rule(row, bounds, bottom, thickness, colors.underline);
                self.rule(
                    row,
                    bounds,
                    bottom - thickness * 2.,
                    thickness,
                    colors.underline,
                );
            }
            3..=5 => self.patterned_underline(
                row,
                cell.underline,
                bounds,
                bottom,
                thickness,
                colors.underline,
            ),
            _ => (),
        }
        if cell.flags & CELL_STRIKETHROUGH != 0 {
            self.rule(
                row,
                bounds,
                bounds.top() + bounds.size.height * 0.55,
                thickness,
                colors.foreground,
            );
        }
        if cell.flags & CELL_OVERLINE != 0 {
            self.rule(row, bounds, bounds.top(), thickness, colors.foreground);
        }
    }

    fn rule(
        &mut self,
        row: u16,
        bounds: Bounds<Pixels>,
        y: Pixels,
        thickness: Pixels,
        color: Hsla,
    ) {
        self.scene.decorations.push(RowQuad {
            row,
            bounds: Bounds::new(point(bounds.left(), y), size(bounds.size.width, thickness)),
            color,
        });
    }

    fn patterned_underline(
        &mut self,
        row: u16,
        kind: u8,
        bounds: Bounds<Pixels>,
        y: Pixels,
        thickness: Pixels,
        color: Hsla,
    ) {
        let step = match kind {
            3 => 1.,
            4 => 2.,
            _ => 4.,
        };
        let mut x = px(0.);
        let mut index = 0;
        while x < bounds.size.width {
            let offset = if kind == 3 {
                [0., 1., 2., 1.][index % 4]
            } else {
                0.
            };
            let width = if kind == 5 { thickness * 2. } else { thickness };
            self.scene.decorations.push(RowQuad {
                row,
                bounds: Bounds::new(
                    point(bounds.left() + x, y - thickness * offset),
                    size(width.min(bounds.size.width - x), thickness),
                ),
                color,
            });
            x += thickness * step;
            index += 1;
        }
    }
}

fn prepare_cursor(scene: &mut Scene) {
    scene.cursor_blocks.clear();
    scene.cursor_overlays.clear();
    let frame = Arc::clone(&scene.frame);
    let settings = &scene.settings;
    if !cursor_visible(&frame, settings) {
        return;
    }
    let geometry = scene.geometry;
    let (row, col) = cursor_head(&frame);
    let bounds = geometry.cell_bounds(row, col, if frame.cursor.width.is_wide() { 2 } else { 1 });
    let color = settings
        .cursor
        .or_else(|| frame.colors.cursor.map(rgb))
        .unwrap_or_else(|| defaults(&frame, settings).0);
    let thickness = px(1. / geometry.scale);
    let style = if settings.focused {
        frame.cursor.style
    } else {
        CursorStyle::BlockHollow
    };
    match style {
        CursorStyle::Block => scene.cursor_blocks.push((bounds, color)),
        CursorStyle::Bar => scene.cursor_overlays.push((
            Bounds::new(bounds.origin, size(thickness * 2., bounds.size.height)),
            color,
        )),
        CursorStyle::Underline => scene.cursor_overlays.push((
            Bounds::new(
                point(bounds.left(), bounds.bottom() - thickness * 2.),
                size(bounds.size.width, thickness * 2.),
            ),
            color,
        )),
        CursorStyle::BlockHollow => scene
            .cursor_overlays
            .extend(outline(bounds, thickness).map(|bounds| (bounds, color))),
    }
}

impl Prepared {
    pub fn paint(
        mut self,
        bounds: Bounds<Pixels>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Observation {
        let started = Instant::now();
        let Some(scene) = self.scene.take() else {
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                if let Some(fallback) = self.fallback {
                    window.paint_quad(fill(bounds, fallback));
                }
            });
            return self.finish(started);
        };
        let mut layers = std::mem::take(&mut self.layers).into_iter();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let hosted = scene.settings.host_fills_background
                && scene.settings.background == Some(scene.background);
            if !hosted {
                window.paint_quad(fill(bounds, scene.background));
            }
            for quad in &scene.backgrounds {
                window.paint_quad(fill(quad.bounds, quad.color));
            }
            for (bounds, color) in &scene.cursor_blocks {
                window.paint_quad(fill(*bounds, *color));
            }
            let geometry = scene.geometry;
            for ink in &scene.inks {
                let result = match &ink.ink {
                    Ink::Glyph(glyph) if glyph.opacity < 1. => layers
                        .next()
                        .ok_or_else(|| "opacity layer missing".to_owned())
                        .and_then(|layer| layer.paint(window, cx)),
                    Ink::Glyph(glyph) => {
                        paint_line(&glyph.line, glyph.bounds, geometry, window, cx)
                    }
                    Ink::Run(run) => run.paint(geometry, window, cx),
                };
                if let Err(error) = result {
                    self.report.error = Some(error);
                }
            }
            for quad in &scene.decorations {
                window.paint_quad(fill(quad.bounds, quad.color));
            }
            for (bounds, color) in &scene.cursor_overlays {
                window.paint_quad(fill(*bounds, *color));
            }
        });
        self.report.geometry = scene.geometry;
        self.report.frame = Some(Arc::clone(&scene.frame));
        if cfg!(feature = "terminal-fixtures") {
            self.report.glyphs.clone_from(&scene.glyphs);
        }
        self.finish(started)
    }

    fn finish(mut self, started: Instant) -> Observation {
        self.report.paint_micros = started.elapsed().as_micros();
        crate::perf::PAINT.record_elapsed(started);
        self.report
    }
}

impl GlyphRun {
    /// Paint the shaped line on the cell grid. `force_width` already put each
    /// base glyph on its column, so this is one line paint rather than one
    /// call per cell.
    fn paint(
        &self,
        geometry: Geometry,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Result<(), String> {
        let bounds = self.bounds;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            self.line
                .paint(
                    bounds.origin,
                    geometry.cell_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .map_err(|error| error.to_string())
        })
    }
}

/// True when `len` bytes of ASCII became one non-emoji glyph each, in order.
fn glyphs_match_bytes(line: &ShapedLine, len: usize) -> bool {
    let mut count = 0;
    for run in &line.runs {
        for glyph in &run.glyphs {
            if glyph.is_emoji || glyph.index != count {
                return false;
            }
            count += 1;
        }
    }
    count == len
}

/// Where a line shaped alone puts its baseline within one cell.
fn line_baseline(line: &ShapedLine, geometry: Geometry) -> Pixels {
    (geometry.cell_height - line.ascent - line.descent) / 2. + line.ascent
}

impl Glyph {
    fn opacity_layer(
        &self,
        geometry: Geometry,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> OpacityLayer {
        let line = self.line.clone();
        let bounds = self.bounds;
        let result = Rc::new(RefCell::new(None));
        let output = result.clone();
        // ShapedLine's color emoji path ignores TextRun.color. Div::opacity is
        // GPUI's public entry to the window opacity stack, used by both glyph
        // paths. TextRun stays opaque so monochrome glyphs are not dimmed twice.
        let mut element = gpui::div()
            .opacity(self.opacity)
            .w(bounds.size.width)
            .h(bounds.size.height)
            .child(
                gpui::canvas(
                    |_, _, _| (),
                    move |_, (), window, cx| {
                        *output.borrow_mut() =
                            Some(paint_line(&line, bounds, geometry, window, cx));
                    },
                )
                .size_full(),
            )
            .into_any_element();
        element.prepaint_as_root(
            bounds.origin,
            bounds.size.map(gpui::AvailableSpace::Definite),
            window,
            cx,
        );
        OpacityLayer { element, result }
    }
}

impl OpacityLayer {
    fn paint(mut self, window: &mut gpui::Window, cx: &mut gpui::App) -> Result<(), String> {
        self.element.paint(window, cx);
        self.result
            .borrow_mut()
            .take()
            .ok_or("opacity layer did not paint")?
    }
}

fn paint_line(
    line: &ShapedLine,
    bounds: Bounds<Pixels>,
    geometry: Geometry,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> Result<(), String> {
    let baseline = (geometry.cell_height - line.ascent - line.descent) / 2. + line.ascent;
    let origin = bounds.origin + point(px(0.), geometry.baseline - baseline);
    window
        .with_content_mask(Some(ContentMask { bounds }), |window| {
            line.paint(
                origin,
                geometry.cell_height,
                TextAlign::Left,
                None,
                window,
                cx,
            )
        })
        .map_err(|error| error.to_string())
}

fn rgb(color: Rgb) -> Hsla {
    gpui::rgb(u32::from(color.r) << 16 | u32::from(color.g) << 8 | u32::from(color.b)).into()
}

/// The theme's colour for one of the 16 ANSI slots, when a theme names them.
fn ansi(settings: &Settings, index: usize) -> Option<Hsla> {
    settings.palette.as_ref()?.get(index).copied()
}

/// Background and underline colours arrive palette-resolved without their
/// index. A colour equal to one of the frame's 16 ANSI entries was almost
/// certainly that slot, so it takes the theme's colour; anything else (true
/// colour, the 240 extended slots) paints as the application sent it.
fn themed_rgb(frame: &GridFrame, settings: &Settings, color: Rgb) -> Hsla {
    if settings.palette.is_some()
        && let Some(index) = frame.colors.palette[..16]
            .iter()
            .position(|entry| *entry == color)
        && let Some(themed) = ansi(settings, index)
    {
        return themed;
    }
    rgb(color)
}

fn defaults(frame: &GridFrame, settings: &Settings) -> (Hsla, Hsla) {
    let (foreground, background) = if frame.colors.reversed {
        (settings.background, settings.foreground)
    } else {
        (settings.foreground, settings.background)
    };
    (
        if frame.colors.has_foreground {
            rgb(frame.colors.foreground)
        } else {
            foreground.unwrap_or_else(|| rgb(frame.colors.foreground))
        },
        if frame.colors.has_background {
            rgb(frame.colors.background)
        } else {
            background.unwrap_or_else(|| rgb(frame.colors.background))
        },
    )
}

/// One cell's colours; `defaults` is [`defaults`] for this frame and settings.
fn cell_colors(
    cell: &Cell,
    metadata: CellMetadata,
    frame: &GridFrame,
    settings: &Settings,
    defaults: (Hsla, Hsla),
) -> CellColors {
    let mut foreground = if metadata.foreground_kind == COLOR_KIND_DEFAULT {
        defaults.0
    } else if metadata.foreground_kind == COLOR_KIND_PALETTE
        && let Some(themed) = ansi(settings, usize::from(metadata.foreground_palette_index))
    {
        themed
    } else {
        themed_rgb(
            frame,
            settings,
            Rgb {
                r: cell.foreground_r,
                g: cell.foreground_g,
                b: cell.foreground_b,
            },
        )
    };
    let mut background = if metadata.background_color_is_default {
        defaults.1
    } else {
        themed_rgb(
            frame,
            settings,
            Rgb {
                r: cell.background_r,
                g: cell.background_g,
                b: cell.background_b,
            },
        )
    };
    if cell.flags & CELL_INVERSE != 0 {
        std::mem::swap(&mut foreground, &mut background);
    }
    if cell.flags & CELL_SELECTED != 0 {
        foreground = settings.selection_foreground;
        background = settings.selection_background;
    }
    let opacity = if cell.flags & CELL_FAINT != 0 {
        0.5
    } else {
        1.
    };
    let underline = if metadata.underline_color_is_default {
        foreground
    } else {
        themed_rgb(
            frame,
            settings,
            Rgb {
                r: cell.underline_r,
                g: cell.underline_g,
                b: cell.underline_b,
            },
        )
    };
    CellColors {
        foreground,
        background,
        underline,
        opacity,
    }
}

/// The cell's face as an index into [`Fonts`]: bit 0 bold, bit 1 italic.
fn style(cell: &Cell) -> usize {
    usize::from(cell.flags & CELL_BOLD != 0) | usize::from(cell.flags & CELL_ITALIC != 0) << 1
}

fn hidden(cell: &Cell, settings: &Settings) -> bool {
    cell.flags & CELL_INVISIBLE != 0 || (cell.flags & CELL_BLINK != 0 && !settings.blink_visible)
}

fn cursor_visible(frame: &GridFrame, settings: &Settings) -> bool {
    frame.cursor.visible
        && settings.cursor_visible
        && (!frame.cursor.blinking || settings.blink_visible)
}

fn solid_cursor(frame: &GridFrame, settings: &Settings) -> bool {
    cursor_visible(frame, settings) && settings.focused && frame.cursor.style == CursorStyle::Block
}

fn cursor_head(frame: &GridFrame) -> (u16, u16) {
    (
        frame.cursor.row,
        frame
            .cursor
            .col
            .saturating_sub(u16::from(frame.cursor.width == CursorWidth::WideTail)),
    )
}

fn outline(bounds: Bounds<Pixels>, thickness: Pixels) -> [Bounds<Pixels>; 4] {
    [
        Bounds::new(bounds.origin, size(bounds.size.width, thickness)),
        Bounds::new(
            point(bounds.left(), bounds.bottom() - thickness),
            size(bounds.size.width, thickness),
        ),
        Bounds::new(bounds.origin, size(thickness, bounds.size.height)),
        Bounds::new(
            point(bounds.right() - thickness, bounds.top()),
            size(thickness, bounds.size.height),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client_runtime::publication::{
        Cursor, FrameColors, GridBuffer, GridDamage, Scrollbar,
    };

    fn frame() -> GridFrame {
        GridFrame {
            terminal_id: phux_protocol::ResourceId::local(1),
            generation: 1,
            stream_id: 1,
            bootstrap_id: 1,
            last_seq: 0,
            cols: 10,
            rows: 2,
            cursor: Cursor::default(),
            scrollbar: Scrollbar::default(),
            colors: FrameColors::default(),
            damage: GridDamage::Full,
            buffer: GridBuffer::default(),
        }
    }

    /// A builder over `frame` without a window: the geometry is given.
    fn builder(frame: GridFrame, settings: Settings, geometry: Geometry) -> Builder {
        let frame = Arc::new(frame);
        let defaults = defaults(&frame, &settings);
        Builder {
            fonts: Fonts::new(&settings.font),
            defaults,
            solid_cursor: None,
            run: None,
            pending: None,
            scene: Scene {
                frame,
                bounds: geometry.bounds,
                scale: 1.,
                settings,
                geometry,
                background: defaults.1,
                backgrounds: Vec::new(),
                inks: Vec::new(),
                decorations: Vec::new(),
                cursor_blocks: Vec::new(),
                cursor_overlays: Vec::new(),
                glyphs: Vec::new(),
            },
        }
    }

    #[test]
    fn backgrounds_merge_into_row_runs_and_skip_the_default() {
        let geometry = Geometry {
            cell_width: px(10.),
            cell_height: px(20.),
            ..Default::default()
        };
        let mut prepared = builder(frame(), Settings::default(), geometry);
        let red: Hsla = gpui::rgb(0xff0000).into();
        let blue: Hsla = gpui::rgb(0x0000ff).into();
        let default = prepared.scene.background;
        // Row 0: default, red, red, blue, default. Row 1: red continues no run.
        for (col, color) in [default, red, red, blue, default].into_iter().enumerate() {
            prepared.background_cell(0, col as u16, color);
        }
        prepared.background_cell(1, 5, red);
        prepared.background_cell(1, 7, red);
        prepared.flush_background();
        let quads: Vec<_> = prepared
            .scene
            .backgrounds
            .iter()
            .map(|quad| {
                (
                    quad.bounds.origin.x,
                    quad.bounds.origin.y,
                    quad.bounds.size.width,
                    quad.color,
                )
            })
            .collect();
        assert_eq!(
            quads,
            vec![
                (px(10.), px(0.), px(20.), red),
                (px(30.), px(0.), px(10.), blue),
                (px(50.), px(20.), px(10.), red),
                (px(70.), px(20.), px(10.), red),
            ]
        );
    }

    #[test]
    fn a_scene_is_reused_only_while_its_inputs_are_unchanged() {
        let settings = Settings::default();
        let geometry = Geometry {
            bounds: Bounds::new(point(px(4.), px(8.)), size(px(100.), px(40.))),
            ..Default::default()
        };
        let scene = builder(frame(), settings.clone(), geometry).scene;
        let same = Arc::clone(&scene.frame);
        let bounds = scene.bounds;
        assert!(scene.reusable(&same, bounds, 1., &settings));
        // Input policy and sizing do not change a prepared scene.
        let mut unpainted = settings.clone();
        unpainted.size_owner = false;
        unpainted.option_as_alt = true;
        unpainted.app_chords = vec!["ctrl+tab".into()];
        assert!(scene.reusable(&same, bounds, 1., &unpainted));
        assert!(
            !scene.reusable(&Arc::new(frame()), bounds, 1., &settings),
            "an equal but newly published frame is a new frame"
        );
        assert!(!scene.reusable(&same, bounds, 2., &settings));
        let moved = Bounds::new(point(px(5.), px(8.)), bounds.size);
        assert!(!scene.reusable(&same, moved, 1., &settings));
        let changes: [fn(&mut Settings); 6] = [
            |settings| settings.focused = false,
            |settings| settings.cursor_visible = false,
            |settings| settings.blink_visible = false,
            |settings| settings.foreground = Some(gpui::rgb(0x123456).into()),
            |settings| settings.font = gpui::font("Monaco"),
            |settings| settings.cell_width_scale = 1.1,
        ];
        for change in changes {
            let mut changed = settings.clone();
            change(&mut changed);
            assert!(!scene.reusable(&same, bounds, 1., &changed));
        }
    }

    #[test]
    fn a_row_matches_across_arena_offsets_until_flags_or_the_solid_cursor_change() {
        let settings = Settings::default();
        let mut left_utf8 = Vec::new();
        let mut right_utf8 = b"pad".to_vec();
        let left_cell = text_cell(&mut left_utf8, b"A");
        let right_cell = text_cell(&mut right_utf8, b"A");
        let blank = Cell::default();
        let left = painted_frame(vec![left_cell, blank, blank, blank], left_utf8);
        let mut right = painted_frame(vec![right_cell, blank, blank, blank], right_utf8);
        assert!(row_paints_alike(&left, &right, 0));
        assert!(rows_to_rebuild(&left, &right, &settings).is_empty());

        right.buffer.cells[0].flags |= CELL_SELECTED;
        assert_eq!(rows_to_rebuild(&left, &right, &settings), vec![0]);
        right.buffer.cells[0].flags = 0;

        let bar_left = cursor_at(left, CursorStyle::Bar, 0, 0);
        let bar_right = cursor_at(right, CursorStyle::Bar, 1, 3);
        assert!(rows_to_rebuild(&bar_left, &bar_right, &settings).is_empty());
        assert_ne!(
            cursor_key(&bar_left, &settings),
            cursor_key(&bar_right, &settings)
        );

        let block_left = cursor_at(bar_left, CursorStyle::Block, 0, 0);
        let block_right = cursor_at(bar_right, CursorStyle::Block, 1, 0);
        assert_eq!(
            rows_to_rebuild(&block_left, &block_right, &settings),
            vec![0, 1]
        );
    }

    #[test]
    fn a_scrolled_screen_keeps_the_rows_that_moved() {
        let settings = Settings::default();
        let previous = row_grid(&["aaa", "bbb", "ccc"]);
        let next = row_grid(&["bbb", "ccc", "ddd"]);
        assert_eq!(scroll_of(&previous, &next), Some(Scroll::Up(1)));
        assert_eq!(
            rows_after_scroll(&previous, &next, &settings, Scroll::Up(1)),
            vec![2]
        );

        let previous = cursor_at(previous, CursorStyle::Block, 2, 0);
        let next = cursor_at(next, CursorStyle::Block, 2, 0);
        assert_eq!(
            rows_after_scroll(&previous, &next, &settings, Scroll::Up(1)),
            vec![1, 2]
        );

        let back = row_grid(&["aaa", "bbb", "ccc"]);
        let forward = row_grid(&["zzz", "aaa", "bbb"]);
        assert_eq!(scroll_of(&back, &forward), Some(Scroll::Down(1)));
        assert_eq!(
            rows_after_scroll(&back, &forward, &settings, Scroll::Down(1)),
            vec![0]
        );
        assert!(scroll_of(&back, &row_grid(&["axa", "bbb", "ccc"])).is_none());
    }

    #[test]
    fn scroll_search_runs_only_when_most_rows_changed() {
        assert!(!worth_scroll_search(1, 40));
        assert!(!worth_scroll_search(34, 40));
        assert!(worth_scroll_search(35, 40));
        assert!(worth_scroll_search(40, 40));
        assert!(worth_scroll_search(3, 3));
        assert!(!worth_scroll_search(2, 3));
        assert!(!worth_scroll_search(0, 1));
    }

    #[test]
    fn shifting_a_scene_moves_the_kept_row() {
        let geometry = Geometry {
            cell_width: px(8.),
            cell_height: px(10.),
            cols: 3,
            rows: 3,
            ..Default::default()
        };
        let mut scene = builder(
            row_grid(&["aaa", "bbb", "ccc"]),
            Settings::default(),
            geometry,
        )
        .scene;
        let origin = geometry.cell_bounds(2, 0, 1);
        scene.backgrounds.push(RowQuad {
            row: 2,
            bounds: origin,
            color: gpui::red(),
        });
        shift_scene(&mut scene, Scroll::Up(1));
        assert_eq!(scene.backgrounds.len(), 1);
        assert_eq!(scene.backgrounds[0].row, 1);
        assert_eq!(
            scene.backgrounds[0].bounds.origin.y,
            geometry.cell_bounds(1, 0, 1).origin.y
        );
    }

    fn row_grid(lines: &[&str]) -> GridFrame {
        let mut utf8 = Vec::new();
        let mut cells = Vec::new();
        for line in lines {
            for byte in line.as_bytes() {
                cells.push(text_cell(&mut utf8, &[*byte]));
            }
        }
        let mut frame = frame();
        frame.cols = u16::try_from(lines.first().map_or(0, |line| line.len())).unwrap_or(0);
        frame.rows = u16::try_from(lines.len()).unwrap_or(0);
        frame.buffer.metadata = vec![CellMetadata::default(); cells.len()];
        frame.buffer.cells = cells;
        frame.buffer.utf8 = utf8;
        frame
    }

    fn cursor_at(mut frame: GridFrame, style: CursorStyle, row: u16, col: u16) -> GridFrame {
        frame.cursor.visible = true;
        frame.cursor.blinking = false;
        frame.cursor.style = style;
        frame.cursor.row = row;
        frame.cursor.col = col;
        frame
    }

    #[test]
    fn merging_rows_drops_the_rebuilt_row_and_keeps_order() {
        let quad = |row, color: Hsla| RowQuad {
            row,
            bounds: Bounds::default(),
            color,
        };
        let fresh = quad(1, gpui::red());
        let merged = merge_rows(
            vec![
                quad(0, gpui::black()),
                quad(1, gpui::black()),
                quad(2, gpui::black()),
            ],
            vec![fresh],
            &[1],
            |item| item.row,
        );
        let rows: Vec<_> = merged.iter().map(|item| item.row).collect();
        assert_eq!(rows, vec![0, 1, 2]);
        assert_eq!(merged[1].color, fresh.color);
    }

    fn painted_frame(cells: Vec<Cell>, utf8: Vec<u8>) -> GridFrame {
        let mut frame = frame();
        frame.cols = 2;
        frame.rows = 2;
        frame.buffer.metadata = vec![CellMetadata::default(); cells.len()];
        frame.buffer.cells = cells;
        frame.buffer.utf8 = utf8;
        frame
    }

    fn text_cell(utf8: &mut Vec<u8>, text: &[u8]) -> Cell {
        let offset = u32::try_from(utf8.len()).unwrap_or(0);
        utf8.extend_from_slice(text);
        Cell {
            utf8_offset: offset,
            utf8_len: u16::try_from(text.len()).unwrap_or(0),
            ..Cell::default()
        }
    }

    #[test]
    fn styles_index_the_four_faces() {
        let fonts = Fonts::new(&gpui::font("Menlo"));
        let face = |flags| {
            &fonts.cell[style(&Cell {
                flags,
                ..Default::default()
            })]
        };
        assert_eq!(face(0).weight, gpui::FontWeight::default());
        assert_eq!(face(CELL_BOLD).weight, gpui::FontWeight::BOLD);
        assert_eq!(face(CELL_ITALIC).style, gpui::FontStyle::Italic);
        let both = face(CELL_BOLD | CELL_ITALIC);
        assert_eq!(
            (both.weight, both.style),
            (gpui::FontWeight::BOLD, gpui::FontStyle::Italic)
        );
        assert!(
            fonts
                .run
                .iter()
                .all(|font| font.features != fonts.cell[0].features)
        );
    }

    #[test]
    fn inverse_swaps_true_color_before_selection_and_faint() {
        let frame = frame();
        let settings = Settings::default();
        let mut cell = Cell {
            foreground_r: 255,
            background_b: 255,
            flags: CELL_INVERSE,
            ..Default::default()
        };
        let metadata = CellMetadata {
            foreground_kind: 2,
            ..Default::default()
        };
        let colors = cell_colors(
            &cell,
            metadata,
            &frame,
            &settings,
            defaults(&frame, &settings),
        );
        assert_eq!(colors.foreground, gpui::rgb(0x0000ff).into());
        assert_eq!(colors.background, gpui::rgb(0xff0000).into());
        cell.flags |= CELL_SELECTED | CELL_FAINT;
        let colors = cell_colors(
            &cell,
            metadata,
            &frame,
            &settings,
            defaults(&frame, &settings),
        );
        assert_eq!(colors.background, settings.selection_background);
        assert_eq!(
            colors.foreground.a, 1.,
            "glyph color must not double-dim window opacity"
        );
        assert_eq!(colors.opacity, 0.5);
    }

    #[test]
    fn theme_uses_provenance_and_respects_terminal_default_override() {
        let mut frame = frame();
        let settings = Settings {
            foreground: Some(gpui::rgb(0xabcdef).into()),
            background: Some(gpui::rgb(0x123456).into()),
            ..Default::default()
        };
        let cell = Cell::default();
        let metadata = CellMetadata {
            background_color_is_default: true,
            underline_color_is_default: true,
            ..Default::default()
        };
        let colors = cell_colors(
            &cell,
            metadata,
            &frame,
            &settings,
            defaults(&frame, &settings),
        );
        assert_eq!(colors.foreground, settings.foreground.expect("foreground"));
        assert_eq!(colors.underline, colors.foreground);
        frame.colors.reversed = true;
        let colors = cell_colors(
            &cell,
            metadata,
            &frame,
            &settings,
            defaults(&frame, &settings),
        );
        assert_eq!(colors.foreground, settings.background.expect("background"));
        frame.colors.has_foreground = true;
        frame.colors.foreground = Rgb { r: 255, g: 0, b: 0 };
        assert_eq!(
            cell_colors(
                &cell,
                metadata,
                &frame,
                &settings,
                defaults(&frame, &settings)
            )
            .foreground,
            gpui::rgb(0xff0000).into()
        );
    }

    #[test]
    fn hidden_and_blinking_cells_do_not_produce_glyphs() {
        let mut settings = Settings::default();
        assert!(hidden(
            &Cell {
                flags: CELL_INVISIBLE,
                ..Default::default()
            },
            &settings
        ));
        let blinking = Cell {
            flags: CELL_BLINK,
            ..Default::default()
        };
        assert!(!hidden(&blinking, &settings));
        settings.blink_visible = false;
        assert!(hidden(&blinking, &settings));
    }

    #[test]
    fn wide_tail_cursor_uses_head_and_unfocused_cursor_is_not_solid() {
        let mut frame = frame();
        frame.cursor = Cursor {
            visible: true,
            col: 4,
            row: 1,
            width: CursorWidth::WideTail,
            style: CursorStyle::Block,
            blinking: false,
        };
        assert_eq!(cursor_head(&frame), (1, 3));
        assert!(solid_cursor(&frame, &Settings::default()));
        assert!(!solid_cursor(
            &frame,
            &Settings {
                focused: false,
                ..Default::default()
            }
        ));
    }
}
