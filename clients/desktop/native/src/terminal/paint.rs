use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, LazyLock},
    time::Instant,
};

use super::{Settings, geometry::Geometry};
use gpui::prelude::*;
use gpui::{
    Bounds, ContentMask, FontFeatures, Hsla, Pixels, ShapedLine, SharedString, fill, point, px,
    size,
};
use phux_client_core::grid::{
    CELL_BLINK, CELL_BOLD, CELL_FAINT, CELL_INVERSE, CELL_INVISIBLE, CELL_ITALIC, CELL_OVERLINE,
    CELL_SELECTED, CELL_STRIKETHROUGH, COLOR_KIND_DEFAULT, COLOR_KIND_PALETTE,
};
use phux_client_runtime::publication::{
    Cell, CellMetadata, CursorStyle, CursorWidth, GridDamage, GridFrame, Rgb,
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
    /// The sub-row scroll shift the grid was painted with.
    pub shift: Pixels,
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

/// One cell shaped alone and clipped to its cells: a wide cell, a grapheme of
/// several characters, or a glyph the cell font does not draw.
#[derive(Clone)]
struct Glyph {
    line: ShapedLine,
    bounds: Bounds<Pixels>,
    /// Monochrome glyph colour, faint alpha included.
    color: Hsla,
    /// Below 1 only for a faint line holding a colour emoji, which GPUI paints
    /// without its `TextRun` colour. That glyph paints inside an opacity layer
    /// with the opaque colour instead.
    opacity: f32,
}

/// Consecutive single-width cells of one row whose glyphs all come from the
/// cell fonts, one glyph per cell. Spaces and blank cells between them stay
/// in the run, and each colour or face change is another `TextRun` of the
/// same line. Shaped once with each column forced to the cell width.
#[derive(Clone)]
struct GlyphRun {
    bounds: Bounds<Pixels>,
    line: ShapedLine,
    /// The colour of each span of `line`, as (end byte, colour), in order.
    colors: Vec<(usize, Hsla)>,
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
    /// Whether the ink's glyphs are in its row's [`RowSprites`]. An ink that
    /// is not (no retained raster, a resolve error, or a translucent emoji)
    /// paints from its shaped line.
    resolved: bool,
}

/// One row's glyphs, resolved against the window's sprite atlas when the row
/// was shaped. Painting them is one GPUI call per row, with no shaping,
/// rasterizing or atlas lookup. A scroll moves the row by `dy` instead of
/// resolving it again: rows sit on whole device pixels, so a moved glyph
/// keeps its rasterization.
#[derive(Clone)]
struct RowSprites {
    row: u16,
    dy: Pixels,
    sprites: Arc<[gpui::GlyphSprite]>,
}

/// How one cell's text is drawn.
#[derive(Clone, Copy, PartialEq)]
struct TextStyle {
    /// Index into [`Fonts`]: bold and italic bits.
    face: usize,
    /// Opaque glyph colour.
    color: Hsla,
    /// Faint text paints below 1.
    opacity: f32,
}

/// `len` bytes of a pending run drawn in one style.
struct Span {
    len: usize,
    style: TextStyle,
}

/// The run being collected: one character per cell, from `start` on `row`.
#[derive(Default)]
struct PendingRun {
    row: u16,
    start: u16,
    /// Cells already in `text`.
    cells: u16,
    text: String,
    /// Blank cells after `text`, added as spaces only if a glyph follows.
    blanks: u16,
    spans: Vec<Span>,
    /// Whether every character is ASCII.
    ascii: bool,
}

impl PendingRun {
    /// The column after the last cell, blanks included.
    fn end(&self) -> usize {
        usize::from(self.start) + usize::from(self.cells) + usize::from(self.blanks)
    }

    fn push(&mut self, ch: char, style: TextStyle) {
        if self.blanks > 0 {
            let spaces = usize::from(self.blanks);
            self.text.extend(std::iter::repeat_n(' ', spaces));
            if let Some(last) = self.spans.last_mut() {
                last.len += spaces;
            }
            self.cells += self.blanks;
            self.blanks = 0;
        }
        self.text.push(ch);
        self.cells += 1;
        self.ascii &= ch.is_ascii();
        let len = ch.len_utf8();
        match self.spans.last_mut() {
            Some(last) if last.style == style => last.len += len,
            _ => self.spans.push(Span { len, style }),
        }
    }
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
    /// The resolved `run` faces. A run glyph from any other font is a
    /// fallback that may overhang its cell, so it is shaped and clipped alone.
    run_ids: std::cell::OnceCell<[gpui::FontId; 4]>,
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
        static FEATURES: LazyLock<FontFeatures> =
            LazyLock::new(|| FontFeatures(Arc::new(vec![("calt".into(), 0), ("liga".into(), 0)])));
        let run = faces.clone().map(|mut font| {
            font.features = FEATURES.clone();
            font
        });
        Self {
            cell: faces,
            run,
            run_ids: std::cell::OnceCell::new(),
        }
    }

    fn run_ids(&self, window: &gpui::Window) -> [gpui::FontId; 4] {
        *self.run_ids.get_or_init(|| {
            let text = window.text_system();
            self.run.each_ref().map(|font| text.resolve_font(font))
        })
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
    /// The faces `settings.font` asks for, kept while the scene is patched.
    fonts: Rc<Fonts>,
    /// What `sprites` were resolved under. `None` when the window cannot
    /// retain glyphs, and then every ink paints from its shaped line.
    raster: Option<gpui::GlyphRaster>,
    background: Hsla,
    backgrounds: Vec<RowQuad>,
    inks: Vec<RowInk>,
    /// Resolved glyphs, one entry per row that has any, in row order.
    sprites: Vec<RowSprites>,
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
        (scale, raster): (f32, Option<gpui::GlyphRaster>),
        settings: &Settings,
    ) -> bool {
        Arc::ptr_eq(&self.frame, frame)
            && self.bounds == bounds
            && self.scale == scale
            && self.raster == raster
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
    /// How far the grid paints above its whole-row position, in device
    /// pixels: zero, or minus a sub-row scroll fraction of a cell.
    shift: Pixels,
}

/// `fraction` is the part of a row above the viewport that a smooth scroll
/// has moved past, in `[0, 1)`. The scene is kept at whole-row positions,
/// so changing only the fraction reuses it and shifts it at paint time; the
/// frame's fringe row fills the space the shift opens at the bottom.
pub(super) fn prepare(
    frame: Result<Arc<GridFrame>, String>,
    settings: Settings,
    fraction: f32,
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
        shift: px(0.),
    };
    match frame {
        Ok(frame) => {
            let scene = scene(frame, settings, bounds, cache, window);
            if has_fringe(&scene.frame) {
                prepared.shift = row_shift(scene.geometry, fraction);
            }
            prepared.layers = opacity_layers(&scene, prepared.shift, window, cx);
            prepared.scene = Some(scene);
        }
        Err(error) => {
            // A rejected reacquire is not a blank. The last scene stays while
            // its bounds still match, so a reconnect does not flash the pane.
            prepared.report.error = Some(error);
            if let Some(scene) = scene_at(cache, bounds) {
                prepared.layers = opacity_layers(&scene, prepared.shift, window, cx);
                prepared.scene = Some(scene);
            }
        }
    }
    prepared.report.prepare_micros = started.elapsed().as_micros();
    crate::perf::PREPARE.record_elapsed(started);
    prepared
}

/// The cached scene when a reacquire failed but the pane has not moved.
fn scene_at(cache: &SceneCache, bounds: Bounds<Pixels>) -> Option<Rc<Scene>> {
    let scene = Rc::clone(cache.borrow().as_ref()?);
    (scene.bounds == bounds).then_some(scene)
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
    let raster = window.glyph_raster();
    if let Some(scene) = cache.borrow().as_ref()
        && scene.reusable(&frame, bounds, (scale, raster), &settings)
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
            (scale, raster),
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
    scale: (f32, Option<gpui::GlyphRaster>),
    window: &gpui::Window,
) -> Option<Scene> {
    if !can_patch(&previous, &frame, bounds, scale, &settings) {
        return None;
    }
    let rebuilt = rows_to_rebuild(&previous.frame, &frame, &settings);
    // Hashing both grids costs a full read. Do it only when the in-place
    // diff would reshape most of the screen, which is what a line of output
    // does. A few dirty rows are cheaper to repaint than to hash.
    if (previous.frame.scrollbar.offset != frame.scrollbar.offset
        || worth_scroll_search(rebuilt.len(), frame.rows))
        && let Some(scroll) = scroll_of(&previous.frame, &frame)
    {
        // Two frames that both carry a fringe scroll it with the viewport: a
        // row leaving the bottom becomes the fringe, and the old fringe can
        // enter, so only the rows new to the scene are shaped.
        let height = shifted_height(&previous.frame, &frame);
        let mut scrolled = rows_after_scroll(
            &previous.frame,
            &frame,
            &settings,
            (scroll, height),
            &previous.geometry,
        );
        if height == frame.rows {
            scrolled = with_fringe_row(scrolled, &frame);
        }
        if scrolled.len() < rebuilt.len() + usize::from(has_fringe(&frame)) {
            if height == frame.rows {
                drop_fringe(&mut previous);
            }
            shift_scene(&mut previous, scroll, height);
            return finish_patch(previous, frame, settings, &scrolled, window);
        }
    }
    if rebuilt.len() == usize::from(frame.rows) {
        return None;
    }
    drop_fringe(&mut previous);
    let rebuilt = with_fringe_row(rebuilt, &frame);
    finish_patch(previous, frame, settings, &rebuilt, window)
}

/// Rows a scroll shift moves: the viewport, plus the fringe row when both
/// frames carry one.
fn shifted_height(previous: &GridFrame, next: &GridFrame) -> u16 {
    if has_fringe(previous) && has_fringe(next) {
        next.rows + 1
    } else {
        next.rows
    }
}

/// Whether `frame` carries the row below its viewport, which the painter
/// builds as row `frame.rows`.
fn has_fringe(frame: &GridFrame) -> bool {
    frame.buffer.fringe.len() == usize::from(frame.cols)
}

/// A viewport cell, or the fringe cell in row `frame.rows`.
fn painted_cell(frame: &GridFrame, row: u16, col: u16) -> Option<(&Cell, CellMetadata)> {
    if row == frame.rows && col < frame.cols {
        let col = usize::from(col);
        let cell = frame.buffer.fringe.get(col)?;
        let metadata = frame.buffer.fringe_metadata.get(col).copied();
        return Some((cell, metadata.unwrap_or_default()));
    }
    let cell = frame.cell(row, col)?;
    let index = usize::from(row) * usize::from(frame.cols) + usize::from(col);
    let metadata = frame.buffer.metadata.get(index).copied();
    Some((cell, metadata.unwrap_or_default()))
}

fn painted_text(frame: &GridFrame, row: u16, col: u16) -> &[u8] {
    if row == frame.rows {
        frame.buffer.fringe_text(usize::from(col))
    } else {
        frame.cell_text(row, col)
    }
}

/// `rows` plus the fringe row when `frame` has one. Rows stay sorted.
fn with_fringe_row(mut rows: Vec<u16>, frame: &GridFrame) -> Vec<u16> {
    if has_fringe(frame) {
        rows.push(frame.rows);
    }
    rows
}

/// Remove the previous fringe row; a patch rebuilds it from the new frame.
fn drop_fringe(scene: &mut Scene) {
    let rows = scene.frame.rows;
    scene.backgrounds.retain(|quad| quad.row < rows);
    scene.inks.retain(|ink| ink.row < rows);
    scene.sprites.retain(|sprites| sprites.row < rows);
    scene.decorations.retain(|quad| quad.row < rows);
    scene.glyphs.retain(|glyph| glyph.row < rows);
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
    let fonts = Rc::clone(&previous.fonts);
    let mut builder = Builder::from_kept(
        frame,
        settings,
        geometry,
        (kept_bounds, kept_scale, previous.raster),
        Some(fonts),
    );
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
    // History navigation gives a cheap candidate; each retained row is
    // still checked before reuse. Live output at the history limit falls
    // back to the content search below.
    let delta = i128::from(next.scrollbar.offset) - i128::from(previous.scrollbar.offset);
    if delta != 0 && delta.unsigned_abs() < u128::from(next.rows) {
        let rows = delta.unsigned_abs() as u16;
        return Some(if delta > 0 {
            Scroll::Up(rows)
        } else {
            Scroll::Down(rows)
        });
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

/// Rows of the new scene, among the `height` a shift moves, that cannot be
/// moved from the previous scene and must be shaped.
fn rows_after_scroll(
    previous: &GridFrame,
    next: &GridFrame,
    settings: &Settings,
    (scroll, height): (Scroll, u16),
    geometry: &Geometry,
) -> Vec<u16> {
    let reverse = match scroll {
        Scroll::Up(rows) => Scroll::Down(rows),
        Scroll::Down(rows) => Scroll::Up(rows),
    };
    let old_cursor = solid_head(previous, settings);
    let new_cursor = solid_head(next, settings);
    (0..height)
        .filter(|row| {
            let Some(source) = destination_row(reverse, height, *row) else {
                return true;
            };
            !fully_built(previous, source, geometry)
                || old_cursor
                    .filter(|(at, _)| *at == source)
                    .map(|(_, col)| col)
                    != new_cursor.filter(|(at, _)| *at == *row).map(|(_, col)| col)
                || !rows_paint_alike(previous, next, source, *row)
        })
        .collect()
}

/// Whether the previous scene holds every primitive of `row`. The builder
/// skips viewport cells outside the pane, so a row cut by the pane's edge
/// is shaped again; the fringe row is always built whole.
fn fully_built(previous: &GridFrame, row: u16, geometry: &Geometry) -> bool {
    if row == previous.rows {
        return true;
    }
    let bounds = geometry.cell_bounds(row, 0, 1);
    bounds.top() >= geometry.bounds.top() && bounds.bottom() <= geometry.bounds.bottom()
}

fn destination_row(scroll: Scroll, height: u16, row: u16) -> Option<u16> {
    match scroll {
        Scroll::Up(by) if row >= by => Some(row - by),
        Scroll::Down(by) if row.checked_add(by).is_some_and(|next| next < height) => Some(row + by),
        _ => None,
    }
}

fn shift_scene(scene: &mut Scene, scroll: Scroll, height: u16) {
    let geometry = &scene.geometry;
    shift_quads(&mut scene.backgrounds, scroll, height, geometry);
    shift_quads(&mut scene.decorations, scroll, height, geometry);
    shift_inks(&mut scene.inks, scroll, height, geometry);
    shift_sprites(&mut scene.sprites, scroll, height, geometry);
    shift_glyphs(&mut scene.glyphs, scroll, height, geometry);
}

fn shift_quads(quads: &mut Vec<RowQuad>, scroll: Scroll, height: u16, geometry: &Geometry) {
    quads.retain_mut(|quad| {
        shift_row(&mut quad.row, &mut quad.bounds, scroll, height, geometry).is_some()
    });
}

fn shift_inks(inks: &mut Vec<RowInk>, scroll: Scroll, height: u16, geometry: &Geometry) {
    inks.retain_mut(|ink| {
        shift_row(
            &mut ink.row,
            ink_bounds(&mut ink.ink),
            scroll,
            height,
            geometry,
        )
        .is_some()
    });
}

fn shift_sprites(rows: &mut Vec<RowSprites>, scroll: Scroll, height: u16, geometry: &Geometry) {
    rows.retain_mut(|sprites| {
        let Some(next) = destination_row(scroll, height, sprites.row) else {
            return false;
        };
        sprites.dy +=
            geometry.cell_bounds(next, 0, 1).top() - geometry.cell_bounds(sprites.row, 0, 1).top();
        sprites.row = next;
        true
    });
}

fn shift_glyphs(
    glyphs: &mut Vec<GlyphObservation>,
    scroll: Scroll,
    height: u16,
    geometry: &Geometry,
) {
    glyphs.retain_mut(|glyph| {
        let Some(dy) = shift_row(&mut glyph.row, &mut glyph.bounds, scroll, height, geometry)
        else {
            return false;
        };
        glyph.origin.y += dy;
        glyph.baseline += dy;
        true
    });
}

fn shift_row(
    row: &mut u16,
    bounds: &mut Bounds<Pixels>,
    scroll: Scroll,
    height: u16,
    geometry: &Geometry,
) -> Option<Pixels> {
    let next = destination_row(scroll, height, *row)?;
    let dy = geometry.cell_bounds(next, 0, 1).top() - geometry.cell_bounds(*row, 0, 1).top();
    *row = next;
    bounds.origin.y += dy;
    Some(dy)
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
    (scale, raster): (f32, Option<gpui::GlyphRaster>),
    settings: &Settings,
) -> bool {
    scene.bounds == bounds
        && scene.scale == scale
        && scene.raster == raster
        && scene.frame.cols == frame.cols
        && scene.frame.rows == frame.rows
        && scene.frame.colors == frame.colors
        && scene.frame.terminal_id == frame.terminal_id
        && scene.frame.stream_id == frame.stream_id
        && scene.frame.bootstrap_id == frame.bootstrap_id
        && scene.frame.generation <= frame.generation
        && scene.settings.paints_like(settings)
}

fn rows_to_rebuild(previous: &GridFrame, next: &GridFrame, settings: &Settings) -> Vec<u16> {
    let old = solid_head(previous, settings);
    let new = solid_head(next, settings);
    // Row damage describes one publication only. A painter can skip generations,
    // so compare every row unless this is the adjacent frame of the same replica.
    let exact_rows = next.damage == GridDamage::Rows
        && previous.generation.checked_add(1) == Some(next.generation)
        && previous.stream_id == next.stream_id
        && previous.bootstrap_id == next.bootstrap_id
        && next.buffer.row_dirty.len() == usize::from(next.rows);
    (0..next.rows)
        .filter(|row| {
            solid_row_changed(old, new, *row)
                || (!exact_rows || next.is_row_dirty(*row))
                    && !row_paints_alike(previous, next, *row)
        })
        .collect()
}

fn solid_row_changed(old: Option<(u16, u16)>, new: Option<(u16, u16)>, row: u16) -> bool {
    if old == new {
        return false;
    }
    old.is_some_and(|(at, _)| at == row) || new.is_some_and(|(at, _)| at == row)
}

fn solid_head(frame: &GridFrame, settings: &Settings) -> Option<(u16, u16)> {
    solid_cursor(frame, settings).then(|| cursor_head(frame))
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
    let (prev_cells, prev_meta) = painted_row(previous, prev_row)?;
    let (next_cells, next_meta) = painted_row(next, next_row)?;
    Some(RowParts {
        prev_cells,
        next_cells,
        prev_meta,
        next_meta,
    })
}

/// A viewport row's cells and metadata, or the fringe's for row `frame.rows`.
fn painted_row(frame: &GridFrame, row: u16) -> Option<(&[Cell], &[CellMetadata])> {
    if row == frame.rows {
        return has_fringe(frame).then_some((
            frame.buffer.fringe.as_slice(),
            frame.buffer.fringe_metadata.as_slice(),
        ));
    }
    let span = row_range(row, usize::from(frame.cols))?;
    Some((
        frame.buffer.cells.get(span.clone())?,
        frame.buffer.metadata.get(span)?,
    ))
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
    previous.sprites = merge_rows(previous.sprites, fresh.sprites, rebuilt, |sprites| {
        sprites.row
    });
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
    // Both inputs are already in row order, including after a constant scroll
    // translation. Merge once instead of sorting every retained primitive.
    let mut merged = Vec::with_capacity(previous.len() + fresh.len());
    let mut fresh = fresh.into_iter().peekable();
    for item in previous {
        let row = row_of(&item);
        if rebuilt.binary_search(&row).is_ok() {
            continue;
        }
        while fresh.peek().is_some_and(|item| row_of(item) < row) {
            merged.push(fresh.next().expect("peeked row"));
        }
        merged.push(item);
    }
    merged.extend(fresh);
    merged
}

/// Minus `fraction` of a row, rounded to device pixels so glyph masks and
/// cell edges stay on the pixel grid while the grid moves.
fn row_shift(geometry: Geometry, fraction: f32) -> Pixels {
    let scale = geometry.scale.max(1.);
    -px((f32::from(geometry.cell_height) * fraction.clamp(0., 1.) * scale).round() / scale)
}

fn opacity_layers(
    scene: &Scene,
    shift: Pixels,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> Vec<OpacityLayer> {
    scene
        .inks
        .iter()
        .filter_map(|ink| match &ink.ink {
            Ink::Glyph(glyph) if glyph.opacity < 1. => {
                Some(glyph.opacity_layer(scene.geometry, shift, window, cx))
            }
            _ => None,
        })
        .collect()
}

/// Walks one frame's cells into a [`Scene`].
struct Builder {
    scene: Scene,
    fonts: Rc<Fonts>,
    /// Default foreground and background after theme and reverse video.
    defaults: (Hsla, Hsla),
    /// The cell under a solid block cursor, whose glyph takes the background.
    solid_cursor: Option<(u16, u16)>,
    /// The open run of same-coloured cells on one row, merged into one quad.
    run: Option<BackgroundRun>,
    pending: Option<PendingRun>,
    /// Glyphs resolved for the row being built.
    row_sprites: Vec<gpui::GlyphSprite>,
}

impl Builder {
    fn new(
        frame: Arc<GridFrame>,
        settings: Settings,
        bounds: Bounds<Pixels>,
        window: &gpui::Window,
    ) -> Self {
        let geometry = Geometry::measure(&settings, bounds, frame.cols, frame.rows, window);
        Self::from_kept(
            frame,
            settings,
            geometry,
            (bounds, window.scale_factor(), window.glyph_raster()),
            None,
        )
    }

    /// `fonts`, when given, were built from a font that paints like `settings.font`.
    fn from_kept(
        frame: Arc<GridFrame>,
        settings: Settings,
        geometry: Geometry,
        (bounds, scale, raster): (Bounds<Pixels>, f32, Option<gpui::GlyphRaster>),
        fonts: Option<Rc<Fonts>>,
    ) -> Self {
        let defaults = defaults(&frame, &settings);
        let solid_cursor = solid_cursor(&frame, &settings).then(|| cursor_head(&frame));
        let fonts = fonts.unwrap_or_else(|| Rc::new(Fonts::new(&settings.font)));
        Self {
            fonts: Rc::clone(&fonts),
            defaults,
            solid_cursor,
            run: None,
            pending: None,
            row_sprites: Vec::new(),
            scene: Scene {
                frame,
                bounds,
                scale,
                settings,
                geometry,
                fonts,
                raster,
                background: defaults.1,
                backgrounds: Vec::new(),
                inks: Vec::new(),
                sprites: Vec::new(),
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
        if has_fringe(&frame) {
            self.paint_row(&frame, frame.rows, window);
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
        if !self.row_sprites.is_empty() {
            self.scene.sprites.push(RowSprites {
                row,
                dy: px(0.),
                sprites: self.row_sprites.drain(..).collect(),
            });
        }
    }

    /// Resolve an ink's glyphs into the row's sprites, painted as `paint_glyphs`
    /// would paint them. False leaves the row's sprites as they were, and the
    /// ink then paints from its shaped line.
    fn resolve(
        &mut self,
        line: &ShapedLine,
        bounds: Bounds<Pixels>,
        colors: &[(usize, Hsla)],
        clip_cells: bool,
        window: &gpui::Window,
    ) -> bool {
        if self.scene.raster.is_none() {
            return false;
        }
        let kept = self.row_sprites.len();
        let geometry = self.scene.geometry;
        let resolved = each_glyph(line, bounds, colors, geometry, clip_cells, |glyph| {
            let clip = glyph.clip.map_or(bounds, |cell| cell.intersect(&bounds));
            if let Some(sprite) = window
                .resolve_glyph(
                    glyph.origin,
                    glyph.font_id,
                    glyph.id,
                    line.font_size,
                    glyph.color,
                    glyph.is_emoji,
                    clip,
                )
                .map_err(|error| error.to_string())?
            {
                self.row_sprites.push(sprite);
            }
            Ok(())
        });
        if resolved.is_err() {
            self.row_sprites.truncate(kept);
        }
        resolved.is_ok()
    }

    fn finish(mut self) -> Scene {
        prepare_cursor(&mut self.scene);
        self.scene
    }

    fn prepare_cell(&mut self, frame: &GridFrame, row: u16, col: u16, window: &gpui::Window) {
        let Some((cell, metadata)) = painted_cell(frame, row, col) else {
            return;
        };
        let geometry = self.scene.geometry;
        let bounds = geometry.cell_bounds(row, col, 1);
        // The fringe row starts at the grid's bottom edge; a sub-row scroll
        // brings it into view.
        if row < frame.rows && !bounds.intersects(&geometry.bounds) {
            return;
        }
        let colors = cell_colors(cell, metadata, frame, &self.scene.settings, self.defaults);
        self.background_cell(row, col, colors.background);
        if hidden(cell, &self.scene.settings) {
            return;
        }
        self.prepare_ink(frame, (row, col), cell, bounds, colors, window);
        // Decorations stay above procedural blocks as well as shaped text.
        self.prepare_decorations(row, cell, bounds, colors);
    }

    fn prepare_ink(
        &mut self,
        frame: &GridFrame,
        (row, col): (u16, u16),
        cell: &Cell,
        bounds: Bounds<Pixels>,
        colors: CellColors,
        window: &gpui::Window,
    ) {
        // Both spacer variants contain no glyph. The head's whole grapheme is
        // shaped in one call; force_width would incorrectly advance combining marks.
        if matches!(cell.wide, 2 | 3) {
            return;
        }
        let text = painted_text(frame, row, col);
        let foreground = self.glyph_foreground(row, col, colors);
        if text.is_empty() || text == b" " {
            if !text.is_empty() {
                self.observe(row, col, bounds, None, foreground);
            }
            self.blank_cell(row, col);
            return;
        }
        if cell.wide == 0 && self.prepare_block((row, col), text, bounds, colors, window) {
            return;
        }
        let style = TextStyle {
            face: style(cell),
            color: foreground,
            opacity: colors.opacity,
        };
        if cell.wide == 0
            && let Some(ch) = lone_char(text)
        {
            self.observe(row, col, bounds, None, foreground.opacity(colors.opacity));
            self.run_cell(row, col, ch, style, window);
            return;
        }
        self.flush_glyph_run(window);
        let text = String::from_utf8_lossy(text).into_owned();
        let width = if cell.wide == 1 { 2 } else { 1 };
        self.shape_cell((row, col, width), text, style, true, window);
    }

    fn prepare_block(
        &mut self,
        (row, col): (u16, u16),
        text: &[u8],
        bounds: Bounds<Pixels>,
        colors: CellColors,
        window: &gpui::Window,
    ) -> bool {
        let Some(regions) = block_regions(text) else {
            return false;
        };
        self.flush_glyph_run(window);
        let foreground = self
            .glyph_foreground(row, col, colors)
            .opacity(colors.opacity);
        self.observe(row, col, bounds, None, foreground);
        for &(x, y, width, height) in regions {
            self.scene.decorations.push(RowQuad {
                row,
                bounds: block_bounds(bounds, (x, y, width, height), self.scene.geometry.scale),
                color: foreground,
            });
        }
        true
    }

    /// A glyph under a solid block cursor takes the cell's background.
    fn glyph_foreground(&self, row: u16, col: u16, colors: CellColors) -> Hsla {
        if self.solid_cursor == Some((row, col)) {
            return colors.background;
        }
        colors.foreground
    }

    /// A space or empty cell. It has no ink, but keeps the pending run open:
    /// it becomes a space in that run if another glyph follows on the row.
    fn blank_cell(&mut self, row: u16, col: u16) {
        if let Some(run) = &mut self.pending
            && run.row == row
            && run.end() == usize::from(col)
        {
            run.blanks += 1;
        }
    }

    /// Extend the pending run with one single-width character, or start one.
    fn run_cell(&mut self, row: u16, col: u16, ch: char, style: TextStyle, window: &gpui::Window) {
        if let Some(run) = &mut self.pending
            && run.row == row
            && run.end() == usize::from(col)
        {
            run.push(ch, style);
            return;
        }
        self.flush_glyph_run(window);
        let mut run = PendingRun {
            row,
            start: col,
            ascii: true,
            ..Default::default()
        };
        run.push(ch, style);
        self.pending = Some(run);
    }

    fn flush_glyph_run(&mut self, window: &gpui::Window) {
        if let Some(run) = self.pending.take() {
            self.shape_run(run, window);
        }
    }

    /// Shape `run` as one line, one cell-font glyph per cell. A run that does
    /// not shape that way (a ligature survived, or a glyph fell back to
    /// another font or to emoji) is split: its ASCII stretches are shaped as
    /// runs again and every other cell alone.
    fn shape_run(&mut self, run: PendingRun, window: &gpui::Window) {
        if run.text.is_empty() {
            return;
        }
        let geometry = self.scene.geometry;
        let text = SharedString::from(run.text);
        let runs: Vec<gpui::TextRun> = run
            .spans
            .iter()
            .map(|span| gpui::TextRun {
                len: span.len,
                font: self.fonts.run[span.style.face].clone(),
                color: span.style.color.opacity(span.style.opacity),
                ..Default::default()
            })
            .collect();
        crate::perf::SHAPED.incr();
        let line = window.text_system().shape_line(
            text.clone(),
            px(self.scene.settings.font_size),
            &runs,
            Some(geometry.cell_width),
        );
        if glyphs_match_cells(&line, &text, &self.fonts.run_ids(window)) {
            let mut end = 0;
            let colors: Vec<_> = runs
                .iter()
                .map(|run| {
                    end += run.len;
                    (end, run.color)
                })
                .collect();
            let bounds = geometry.cell_bounds(run.row, run.start, run.cells);
            let resolved = self.resolve(&line, bounds, &colors, true, window);
            self.scene.inks.push(RowInk {
                row: run.row,
                ink: Ink::Run(Box::new(GlyphRun {
                    bounds,
                    line,
                    colors,
                })),
                resolved,
            });
            return;
        }
        let mut spans = run.spans.iter();
        let mut span_end = 0;
        let mut style = None;
        let mut part: Option<PendingRun> = None;
        for (cell, (offset, ch)) in text.char_indices().enumerate() {
            while offset >= span_end
                && let Some(span) = spans.next()
            {
                span_end += span.len;
                style = Some(span.style);
            }
            let Some(style) = style else {
                return;
            };
            let col = run
                .start
                .saturating_add(u16::try_from(cell).unwrap_or(u16::MAX));
            if ch == ' ' {
                if let Some(part) = &mut part {
                    part.blanks += 1;
                }
                continue;
            }
            if ch.is_ascii() && !run.ascii {
                part.get_or_insert_with(|| PendingRun {
                    row: run.row,
                    start: col,
                    ascii: true,
                    ..Default::default()
                })
                .push(ch, style);
                continue;
            }
            if let Some(part) = part.take() {
                self.shape_run(part, window);
            }
            self.shape_cell((run.row, col, 1), ch.to_string(), style, false, window);
        }
        if let Some(part) = part {
            self.shape_run(part, window);
        }
    }

    /// Shape one cell's text alone. `cell` is its row, column and width.
    fn shape_cell(
        &mut self,
        (row, col, width): (u16, u16, u16),
        text: String,
        style: TextStyle,
        observe: bool,
        window: &gpui::Window,
    ) {
        let TextStyle {
            face,
            color,
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
                font: self.fonts.cell[face].clone(),
                color: color.opacity(opacity),
                ..Default::default()
            }],
            None,
        );
        if observe {
            self.observe(row, col, bounds, Some(&line), color.opacity(opacity));
        }
        let emoji = opacity < 1.
            && line
                .runs
                .iter()
                .any(|run| run.glyphs.iter().any(|glyph| glyph.is_emoji));
        let (color, opacity) = if emoji {
            (color, opacity)
        } else {
            (color.opacity(opacity), 1.)
        };
        let resolved =
            opacity == 1. && self.resolve(&line, bounds, &[(line.len(), color)], false, window);
        self.scene.inks.push(RowInk {
            row,
            ink: Ink::Glyph(Box::new(Glyph {
                line,
                bounds,
                color,
                opacity,
            })),
            resolved,
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
    let color = cursor_color(&frame, settings);
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
    /// Cell backgrounds, a solid cursor and glyphs share one GPUI layer, so
    /// they take one draw order instead of one bounds-tree insertion each;
    /// GPUI draws a layer's quads before its sprites. Decorations, procedural
    /// blocks and the other cursors share a second layer above it.
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
        let shift = self.shift;
        let moved = |bounds: Bounds<Pixels>| shifted(bounds, shift);
        // Rows below the viewport are the fringe, visible only while shifted.
        // The shifted grid is clipped to the rows, not the padding under them.
        let rows = scene.frame.rows;
        let shown = |row: u16| row < rows || shift != px(0.);
        let grid = if shift == px(0.) {
            bounds
        } else {
            let bottom = scene
                .geometry
                .cell_bounds(rows, 0, 1)
                .top()
                .min(bounds.bottom());
            Bounds::from_corners(bounds.origin, point(bounds.right(), bottom))
        };
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_layer(bounds, |window| {
                let hosted = scene.settings.host_fills_background
                    && scene.settings.background == Some(scene.background);
                if !hosted {
                    window.paint_quad(fill(bounds, scene.background));
                }
                window.with_content_mask(Some(ContentMask { bounds: grid }), |window| {
                    for quad in scene.backgrounds.iter().filter(|quad| shown(quad.row)) {
                        window.paint_quad(fill(moved(quad.bounds), quad.color));
                    }
                    for (bounds, color) in &scene.cursor_blocks {
                        window.paint_quad(fill(moved(*bounds), *color));
                    }
                    for row in scene.sprites.iter().filter(|row| shown(row.row)) {
                        window.paint_glyph_sprites(&row.sprites, point(px(0.), row.dy + shift));
                    }
                    let geometry = scene.geometry;
                    for ink in &scene.inks {
                        let result = match &ink.ink {
                            Ink::Glyph(glyph) if glyph.opacity < 1. => {
                                let layer = layers
                                    .next()
                                    .ok_or_else(|| "opacity layer missing".to_owned());
                                if !shown(ink.row) {
                                    continue;
                                }
                                layer.and_then(|layer| layer.paint(window, cx))
                            }
                            _ if ink.resolved || !shown(ink.row) => continue,
                            Ink::Glyph(glyph) => glyph.paint(glyph.color, geometry, shift, window),
                            Ink::Run(run) => run.paint(geometry, shift, window),
                        };
                        if let Err(error) = result {
                            self.report.error = Some(error);
                        }
                    }
                });
            });
            window.paint_layer(bounds, |window| {
                window.with_content_mask(Some(ContentMask { bounds: grid }), |window| {
                    for quad in scene.decorations.iter().filter(|quad| shown(quad.row)) {
                        window.paint_quad(fill(moved(quad.bounds), quad.color));
                    }
                    for (bounds, color) in &scene.cursor_overlays {
                        window.paint_quad(fill(moved(*bounds), *color));
                    }
                });
            });
        });
        self.report.shift = shift;
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
    /// `force_width` already put each glyph on its column, so the run is one
    /// pass over its glyphs, clipped to its cells.
    fn paint(
        &self,
        geometry: Geometry,
        shift: Pixels,
        window: &mut gpui::Window,
    ) -> Result<(), String> {
        let bounds = shifted(self.bounds, shift);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            paint_glyphs(&self.line, bounds, &self.colors, geometry, true, window)
        })
    }
}

impl Glyph {
    /// Paint the glyph clipped to its cells, monochrome glyphs in `color`.
    fn paint(
        &self,
        color: Hsla,
        geometry: Geometry,
        shift: Pixels,
        window: &mut gpui::Window,
    ) -> Result<(), String> {
        let bounds = shifted(self.bounds, shift);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            paint_glyphs(
                &self.line,
                bounds,
                &[(self.line.len(), color)],
                geometry,
                false,
                window,
            )
        })
    }

    fn opacity_layer(
        &self,
        geometry: Geometry,
        shift: Pixels,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> OpacityLayer {
        let glyph = self.clone();
        let bounds = shifted(self.bounds, shift);
        let result = Rc::new(RefCell::new(None));
        let output = result.clone();
        // ShapedLine's color emoji path ignores TextRun.color. Div::opacity is
        // GPUI's public entry to the window opacity stack, used by both glyph
        // paths. The colour stays opaque so monochrome glyphs are not dimmed twice.
        let mut element = gpui::div()
            .opacity(self.opacity)
            .w(bounds.size.width)
            .h(bounds.size.height)
            .child(
                gpui::canvas(
                    |_, _, _| (),
                    move |_, (), window, _| {
                        *output.borrow_mut() =
                            Some(glyph.paint(glyph.color, geometry, shift, window));
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

/// `bounds` moved down by `shift` (up when negative).
pub(super) fn shifted(bounds: Bounds<Pixels>, shift: Pixels) -> Bounds<Pixels> {
    Bounds::new(point(bounds.left(), bounds.top() + shift), bounds.size)
}

/// True when each character of `text` became one non-emoji glyph from one
/// of `fonts`, in order.
fn glyphs_match_cells(line: &ShapedLine, text: &str, fonts: &[gpui::FontId]) -> bool {
    let mut cells = text.char_indices();
    for run in &line.runs {
        if !fonts.contains(&run.font_id) {
            return false;
        }
        for glyph in &run.glyphs {
            match cells.next() {
                Some((offset, _)) if !glyph.is_emoji && glyph.index == offset => {}
                _ => return false,
            }
        }
    }
    cells.next().is_none()
}

/// The single character of a cell, if it is one printable character.
fn lone_char(text: &[u8]) -> Option<char> {
    let mut chars = std::str::from_utf8(text).ok()?.chars();
    let ch = chars.next()?;
    (chars.next().is_none() && !ch.is_control()).then_some(ch)
}

/// Where a line shaped alone puts its baseline within one cell.
fn line_baseline(line: &ShapedLine, geometry: Geometry) -> Pixels {
    (geometry.cell_height - line.ascent - line.descent) / 2. + line.ascent
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

/// One glyph of a shaped line, placed on its cell.
struct PlacedGlyph {
    origin: gpui::Point<Pixels>,
    font_id: gpui::FontId,
    id: gpui::GlyphId,
    color: Hsla,
    is_emoji: bool,
    /// The cell a non-ASCII run glyph is clipped to, inside the line's bounds.
    clip: Option<Bounds<Pixels>>,
}

/// Visit `line`'s inked glyphs from the left of `bounds`, every glyph on the
/// row baseline. `colors` gives each span's (end byte, colour); colour emoji
/// ignore it. Spaces have no ink and are skipped. With `clip_cells`, each
/// glyph is one cell of a run and a non-ASCII glyph (box drawing, symbols)
/// is clipped to its own cell, as if it were shaped alone.
fn each_glyph(
    line: &ShapedLine,
    bounds: Bounds<Pixels>,
    colors: &[(usize, Hsla)],
    geometry: Geometry,
    clip_cells: bool,
    mut visit: impl FnMut(PlacedGlyph) -> Result<(), String>,
) -> Result<(), String> {
    let baseline = bounds.top() + geometry.baseline;
    let text = line.text.as_bytes();
    let mut spans = colors.iter();
    let mut span = spans.next();
    for run in &line.runs {
        for glyph in &run.glyphs {
            if text.get(glyph.index) == Some(&b' ') {
                continue;
            }
            while let Some(&(end, _)) = span
                && glyph.index >= end
            {
                span = spans.next();
            }
            let clip = (clip_cells && text.get(glyph.index).is_some_and(|byte| *byte >= 0x80))
                .then(|| {
                    let column =
                        (f32::from(glyph.position.x) / f32::from(geometry.cell_width)).round();
                    Bounds::new(
                        point(bounds.left() + geometry.cell_width * column, bounds.top()),
                        size(geometry.cell_width, bounds.size.height),
                    )
                });
            visit(PlacedGlyph {
                origin: point(
                    bounds.left() + glyph.position.x,
                    baseline + glyph.position.y,
                ),
                font_id: run.font_id,
                id: glyph.id,
                color: span.map_or_else(gpui::black, |&(_, color)| color),
                is_emoji: glyph.is_emoji,
                clip,
            })?;
        }
    }
    Ok(())
}

/// Paint `line` glyph by glyph, as [`each_glyph`] places it, inside the
/// current content mask.
fn paint_glyphs(
    line: &ShapedLine,
    bounds: Bounds<Pixels>,
    colors: &[(usize, Hsla)],
    geometry: Geometry,
    clip_cells: bool,
    window: &mut gpui::Window,
) -> Result<(), String> {
    each_glyph(line, bounds, colors, geometry, clip_cells, |glyph| {
        let paint = |window: &mut gpui::Window| {
            if glyph.is_emoji {
                window.paint_emoji(glyph.origin, glyph.font_id, glyph.id, line.font_size)
            } else {
                window.paint_glyph(
                    glyph.origin,
                    glyph.font_id,
                    glyph.id,
                    line.font_size,
                    glyph.color,
                )
            }
        };
        match glyph.clip {
            Some(cell) => window.with_content_mask(Some(ContentMask { bounds: cell }), paint),
            None => paint(window),
        }
        .map_err(|error| error.to_string())
    })
}

fn rgb(color: Rgb) -> Hsla {
    gpui::rgb(u32::from(color.r) << 16 | u32::from(color.g) << 8 | u32::from(color.b)).into()
}

/// Solid Unicode blocks describe cell coverage, not font ink. Drawing them
/// from the cell edges avoids font side bearings and line-height gaps in
/// terminal UIs that use half blocks for smooth panels.
fn block_regions(text: &[u8]) -> Option<&'static [(u8, u8, u8, u8)]> {
    Some(match std::str::from_utf8(text).ok()? {
        "▀" => &[(0, 0, 8, 4)],
        "▁" => &[(0, 7, 8, 1)],
        "▂" => &[(0, 6, 8, 2)],
        "▃" => &[(0, 5, 8, 3)],
        "▄" => &[(0, 4, 8, 4)],
        "▅" => &[(0, 3, 8, 5)],
        "▆" => &[(0, 2, 8, 6)],
        "▇" => &[(0, 1, 8, 7)],
        "█" => &[(0, 0, 8, 8)],
        "▉" => &[(0, 0, 7, 8)],
        "▊" => &[(0, 0, 6, 8)],
        "▋" => &[(0, 0, 5, 8)],
        "▌" => &[(0, 0, 4, 8)],
        "▍" => &[(0, 0, 3, 8)],
        "▎" => &[(0, 0, 2, 8)],
        "▏" => &[(0, 0, 1, 8)],
        "▐" => &[(4, 0, 4, 8)],
        "▔" => &[(0, 0, 8, 1)],
        "▕" => &[(7, 0, 1, 8)],
        "▖" => &[(0, 4, 4, 4)],
        "▗" => &[(4, 4, 4, 4)],
        "▘" => &[(0, 0, 4, 4)],
        "▙" => &[(0, 0, 4, 8), (4, 4, 4, 4)],
        "▚" => &[(0, 0, 4, 4), (4, 4, 4, 4)],
        "▛" => &[(0, 0, 8, 4), (0, 4, 4, 4)],
        "▜" => &[(0, 0, 8, 4), (4, 4, 4, 4)],
        "▝" => &[(4, 0, 4, 4)],
        "▞" => &[(4, 0, 4, 4), (0, 4, 4, 4)],
        "▟" => &[(4, 0, 4, 8), (0, 4, 4, 4)],
        _ => return None,
    })
}

fn block_bounds(
    bounds: Bounds<Pixels>,
    (x, y, width, height): (u8, u8, u8, u8),
    scale: f32,
) -> Bounds<Pixels> {
    let edge = |origin: Pixels, length: Pixels, fraction: u8| {
        px(
            ((f32::from(origin) + f32::from(length) * f32::from(fraction) / 8.) * scale).round()
                / scale,
        )
    };
    let left = edge(bounds.left(), bounds.size.width, x);
    let top = edge(bounds.top(), bounds.size.height, y);
    let right = edge(bounds.left(), bounds.size.width, x + width);
    let bottom = edge(bounds.top(), bounds.size.height, y + height);
    Bounds::new(point(left, top), size(right - left, bottom - top))
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

fn cursor_color(frame: &GridFrame, settings: &Settings) -> Hsla {
    settings
        .cursor
        .or_else(|| frame.colors.cursor.map(rgb))
        .unwrap_or_else(|| defaults(frame, settings).0)
}

/// Where a laser beam lands, and its colour: the focused, visible cursor.
pub(super) fn laser_target(
    frame: &GridFrame,
    settings: &Settings,
) -> Option<(super::laser::Spot, Hsla)> {
    (settings.cursor_laser && settings.focused && cursor_visible(frame, settings))
        .then(|| (cursor_span(frame), cursor_color(frame, settings)))
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

/// Painted cursor as `(row, col, width)`. A wide tail reports the head cell.
pub(super) fn cursor_span(frame: &GridFrame) -> (u16, u16, u16) {
    let (row, col) = cursor_head(frame);
    let width = if frame.cursor.width.is_wide() { 2 } else { 1 };
    (row, col, width)
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
        let fonts = Rc::new(Fonts::new(&settings.font));
        Builder {
            fonts: Rc::clone(&fonts),
            defaults,
            solid_cursor: None,
            run: None,
            pending: None,
            row_sprites: Vec::new(),
            scene: Scene {
                frame,
                bounds: geometry.bounds,
                scale: 1.,
                settings,
                geometry,
                fonts,
                raster: None,
                background: defaults.1,
                backgrounds: Vec::new(),
                inks: Vec::new(),
                sprites: Vec::new(),
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
            scale: 1.,
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
        assert!(scene.reusable(&same, bounds, (1., None), &settings));
        // Input policy and sizing do not change a prepared scene.
        let mut unpainted = settings.clone();
        unpainted.size_owner = false;
        unpainted.option_as_alt = true;
        unpainted.app_chords = vec!["ctrl+tab".into()];
        assert!(scene.reusable(&same, bounds, (1., None), &unpainted));
        assert!(
            !scene.reusable(&Arc::new(frame()), bounds, (1., None), &settings),
            "an equal but newly published frame is a new frame"
        );
        assert!(!scene.reusable(&same, bounds, (2., None), &settings));
        let moved = Bounds::new(point(px(5.), px(8.)), bounds.size);
        assert!(!scene.reusable(&same, moved, (1., None), &settings));
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
            assert!(!scene.reusable(&same, bounds, (1., None), &changed));
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
        assert_eq!(rows_to_rebuild(&left, &right, &settings), Vec::<u16>::new());

        right.buffer.cells[0].flags |= CELL_SELECTED;
        assert_eq!(rows_to_rebuild(&left, &right, &settings), vec![0]);
        right.buffer.cells[0].flags = 0;

        let bar_left = cursor_at(left, CursorStyle::Bar, 0, 0);
        let bar_right = cursor_at(right, CursorStyle::Bar, 1, 1);
        assert_eq!(
            rows_to_rebuild(&bar_left, &bar_right, &settings),
            Vec::<u16>::new()
        );
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
    fn sparse_damage_keeps_changed_rows_but_skipped_publications_compare_the_whole_grid() {
        let settings = Settings::default();
        let previous = row_grid(&["aaaa", "bbbb", "cccc"]);
        let mut adjacent = row_grid(&["aaaa", "bBbb", "cccc"]);
        adjacent.generation = 2;
        adjacent.damage = GridDamage::Rows;
        adjacent.buffer.row_dirty = vec![false, true, false];
        assert_eq!(rows_to_rebuild(&previous, &adjacent, &settings), vec![1]);

        let mut skipped = row_grid(&["zaza", "bBbb", "cccc"]);
        skipped.generation = 3;
        skipped.damage = GridDamage::Rows;
        skipped.buffer.row_dirty = vec![false, true, false];
        assert_eq!(rows_to_rebuild(&previous, &skipped, &settings), vec![0, 1]);

        adjacent.damage = GridDamage::Full;
        adjacent.buffer.cells[0].flags |= CELL_SELECTED;
        assert_eq!(rows_to_rebuild(&previous, &adjacent, &settings), vec![0, 1]);
    }

    #[test]
    fn a_scrolled_screen_keeps_the_rows_that_moved() {
        let settings = Settings::default();
        let previous = row_grid(&["aaa", "bbb", "ccc"]);
        let next = row_grid(&["bbb", "ccc", "ddd"]);
        assert_eq!(scroll_of(&previous, &next), Some(Scroll::Up(1)));
        assert_eq!(
            rows_after_scroll(
                &previous,
                &next,
                &settings,
                (Scroll::Up(1), previous.rows),
                &scrolling_geometry()
            ),
            vec![2]
        );

        let previous = cursor_at(previous, CursorStyle::Block, 2, 0);
        let next = cursor_at(next, CursorStyle::Block, 2, 0);
        assert_eq!(
            rows_after_scroll(
                &previous,
                &next,
                &settings,
                (Scroll::Up(1), previous.rows),
                &scrolling_geometry()
            ),
            vec![1, 2]
        );

        let back = row_grid(&["aaa", "bbb", "ccc"]);
        let forward = row_grid(&["zzz", "aaa", "bbb"]);
        assert_eq!(scroll_of(&back, &forward), Some(Scroll::Down(1)));
        assert_eq!(
            rows_after_scroll(
                &back,
                &forward,
                &settings,
                (Scroll::Down(1), back.rows),
                &scrolling_geometry()
            ),
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
            scale: 1.,
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
        let height = scene.frame.rows;
        shift_scene(&mut scene, Scroll::Up(1), height);
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
        assert_eq!(cursor_span(&frame), (1, 3, 2));
        frame.cursor.width = CursorWidth::Wide;
        assert_eq!(cursor_span(&frame), (1, 4, 2));
        frame.cursor.width = CursorWidth::Narrow;
        assert_eq!(cursor_span(&frame), (1, 4, 1));
        assert!(solid_cursor(&frame, &Settings::default()));
        assert!(!solid_cursor(
            &frame,
            &Settings {
                focused: false,
                ..Default::default()
            }
        ));
    }

    fn scrolling_geometry() -> Geometry {
        Geometry {
            bounds: Bounds::new(point(px(0.), px(0.)), size(px(100.), px(100.))),
            cell_width: px(10.),
            cell_height: px(20.),
            scale: 1.,
            ..Default::default()
        }
    }

    fn scrolling_frame(rows: &[&str], offset: u64) -> GridFrame {
        let mut frame = frame();
        frame.cols = 2;
        frame.rows = rows.len() as u16;
        frame.cursor.visible = false;
        frame.scrollbar.offset = offset;
        for row in rows {
            let cell = text_cell(&mut frame.buffer.utf8, row.as_bytes());
            frame.buffer.cells.extend([cell, Cell::default()]);
            frame.buffer.metadata.extend([CellMetadata::default(); 2]);
        }
        frame
    }

    #[test]
    fn scrolling_reuses_only_exact_rows_in_both_directions_and_at_history_limit() {
        let settings = Settings::default();
        let geometry = scrolling_geometry();
        let old = scrolling_frame(&["A", "B", "C", "D"], 10);
        let mut next = scrolling_frame(&["B", "C", "D", "E"], 11);
        assert_eq!(scroll_of(&old, &next), Some(Scroll::Up(1)));
        assert_eq!(
            rows_after_scroll(&old, &next, &settings, (Scroll::Up(1), old.rows), &geometry),
            vec![3]
        );
        assert_eq!(scroll_of(&next, &old), Some(Scroll::Down(1)));
        assert_eq!(
            rows_after_scroll(
                &next,
                &old,
                &settings,
                (Scroll::Down(1), next.rows),
                &geometry
            ),
            vec![0]
        );
        next.scrollbar.offset = old.scrollbar.offset;
        assert_eq!(
            scroll_of(&old, &next),
            Some(Scroll::Up(1)),
            "history at capacity"
        );
        next.scrollbar.offset += 1;
        next.buffer.cells[2].flags |= CELL_SELECTED;
        next.buffer.cells[4].wide = 1;
        assert_eq!(
            rows_after_scroll(&old, &next, &settings, (Scroll::Up(1), old.rows), &geometry),
            vec![1, 2, 3],
            "selection and wide-head changes must not keep stale ink"
        );
        let clipped = Geometry {
            bounds: Bounds::new(geometry.bounds.origin, size(px(100.), px(30.))),
            ..geometry
        };
        assert_eq!(
            rows_after_scroll(&old, &next, &settings, (Scroll::Up(1), old.rows), &clipped),
            vec![0, 1, 2, 3],
            "previously clipped rows must be freshly painted when exposed"
        );
    }

    fn with_fringe(mut frame: GridFrame, text: &str) -> GridFrame {
        let cell = text_cell(&mut frame.buffer.utf8, text.as_bytes());
        frame.buffer.fringe = vec![cell, Cell::default()];
        frame.buffer.fringe_metadata = vec![CellMetadata::default(); 2];
        frame
    }

    /// A row that leaves the viewport becomes the fringe and the old fringe
    /// enters it, so a one-row history scroll shapes only the row new to the
    /// scene.
    #[test]
    fn history_scrolling_moves_the_fringe_with_the_viewport() {
        let settings = Settings::default();
        let geometry = scrolling_geometry();
        // Older history above: scrolling back moves content down.
        let near = with_fringe(scrolling_frame(&["B", "C", "D", "E"], 11), "F");
        let back = with_fringe(scrolling_frame(&["A", "B", "C", "D"], 10), "E");
        assert_eq!(shifted_height(&near, &back), 5);
        assert_eq!(scroll_of(&near, &back), Some(Scroll::Down(1)));
        assert_eq!(
            rows_after_scroll(&near, &back, &settings, (Scroll::Down(1), 5), &geometry),
            vec![0],
            "the row leaving the bottom is the new fringe"
        );
        assert_eq!(
            rows_after_scroll(&back, &near, &settings, (Scroll::Up(1), 5), &geometry),
            vec![4],
            "the old fringe enters the viewport"
        );
        let other = with_fringe(scrolling_frame(&["A", "B", "C", "D"], 10), "X");
        assert_eq!(
            rows_after_scroll(&near, &other, &settings, (Scroll::Down(1), 5), &geometry),
            vec![0, 4],
            "a fringe that does not match its source row is shaped again"
        );
        assert_eq!(
            shifted_height(&scrolling_frame(&["A", "B", "C", "D"], 10), &back),
            4,
            "a frame without a fringe shifts the viewport alone"
        );
    }

    #[test]
    fn scrolling_invalidates_old_and_new_solid_cursor_ink() {
        let old = cursor_at(
            scrolling_frame(&["A", "B", "C", "D"], 0),
            CursorStyle::Block,
            1,
            0,
        );
        let next = cursor_at(
            scrolling_frame(&["B", "C", "D", "E"], 1),
            CursorStyle::Block,
            2,
            0,
        );
        assert_eq!(
            rows_after_scroll(
                &old,
                &next,
                &Settings::default(),
                (Scroll::Up(1), old.rows),
                &scrolling_geometry()
            ),
            vec![0, 2, 3]
        );
    }

    #[test]
    fn retained_primitives_move_together_on_the_shared_pixel_grid() {
        let geometry = scrolling_geometry();
        let mut scene = builder(
            scrolling_frame(&["A", "B", "C"], 0),
            Settings::default(),
            geometry,
        )
        .scene;
        let bounds = geometry.cell_bounds(1, 0, 2);
        scene.backgrounds.push(RowQuad {
            row: 1,
            bounds,
            color: gpui::red(),
        });
        scene.decorations.push(RowQuad {
            row: 1,
            bounds,
            color: gpui::blue(),
        });
        scene.glyphs.push(GlyphObservation {
            row: 1,
            col: 0,
            bounds,
            origin: bounds.origin,
            baseline: px(35.),
            foreground: gpui::red(),
        });
        let height = scene.frame.rows;
        shift_scene(&mut scene, Scroll::Up(1), height);
        assert_eq!(scene.backgrounds[0].row, 0);
        assert_eq!(scene.backgrounds[0].bounds, geometry.cell_bounds(0, 0, 2));
        assert_eq!(scene.decorations[0].bounds, scene.backgrounds[0].bounds);
        assert_eq!(scene.glyphs[0].row, 0);
        assert_eq!(scene.glyphs[0].origin.y, px(0.));
        assert_eq!(scene.glyphs[0].baseline, px(15.));
    }

    #[test]
    fn retained_rows_never_cross_replica_or_geometry_fences() {
        let geometry = Geometry {
            scale: 1.,
            ..Default::default()
        };
        let scene = builder(frame(), Settings::default(), geometry).scene;
        let mut next = frame();
        assert!(can_patch(
            &scene,
            &next,
            scene.bounds,
            (1., None),
            &scene.settings
        ));
        next.bootstrap_id += 1;
        assert!(!can_patch(
            &scene,
            &next,
            scene.bounds,
            (1., None),
            &scene.settings
        ));
        next.bootstrap_id -= 1;
        next.stream_id += 1;
        assert!(!can_patch(
            &scene,
            &next,
            scene.bounds,
            (1., None),
            &scene.settings
        ));
        next.stream_id -= 1;
        next.generation = 0;
        assert!(!can_patch(
            &scene,
            &next,
            scene.bounds,
            (1., None),
            &scene.settings
        ));
        next.generation = 2;
        assert!(!can_patch(
            &scene,
            &next,
            scene.bounds,
            (2., None),
            &scene.settings
        ));
    }

    #[test]
    fn half_blocks_share_the_exact_device_pixel_boundary() {
        for scale in [1., 1.25, 1.5, 2.] {
            let bounds = Bounds::new(
                point(px(2. / scale), px(3. / scale)),
                size(px(13. / scale), px(27. / scale)),
            );
            let upper = block_bounds(bounds, block_regions("▀".as_bytes()).unwrap()[0], scale);
            let lower = block_bounds(bounds, block_regions("▄".as_bytes()).unwrap()[0], scale);
            assert_eq!(upper.bottom(), lower.top());
            assert_eq!(upper.top(), bounds.top());
            assert_eq!(lower.bottom(), bounds.bottom());
            assert_eq!(upper.size.width, bounds.size.width);
            assert!(
                block_regions("▀\u{fe0f}".as_bytes()).is_none(),
                "a grapheme with selectors must keep normal text shaping"
            );
        }
    }
}
