use std::{cell::RefCell, rc::Rc, sync::Arc, time::Instant};

use super::{GridFrame, Settings, geometry::Geometry, gpui};
use gpui::prelude::*;
use gpui::{
    Bounds, ContentMask, FontFeatures, FontId, GlyphId, Hsla, Pixels, ShapedLine, SharedString,
    TextAlign, fill, point, px, size,
};
use phux_client_core::grid::{
    CELL_BLINK, CELL_BOLD, CELL_FAINT, CELL_INVERSE, CELL_INVISIBLE, CELL_ITALIC, CELL_OVERLINE,
    CELL_SELECTED, CELL_STRIKETHROUGH, COLOR_KIND_DEFAULT, COLOR_KIND_PALETTE,
};
use phux_client_runtime::publication::{Cell, CellMetadata, CursorStyle, CursorWidth, Rgb};

#[derive(Default)]
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
struct Glyph {
    line: ShapedLine,
    bounds: Bounds<Pixels>,
    opacity: f32,
}

/// Consecutive cells of one row holding printable ASCII in one font and
/// colour, shaped as one line and painted one glyph per cell. A screen of
/// text is then a few shapes per row instead of one per cell.
struct GlyphRun {
    bounds: Bounds<Pixels>,
    color: Hsla,
    glyphs: Vec<(FontId, GlyphId)>,
}

enum Ink {
    Glyph(Box<Glyph>),
    Run(GlyphRun),
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
/// frame, the element's bounds and scale, and the paint settings. GPUIX
/// re-renders the whole window for any change, so most draws repaint a
/// terminal whose scene is unchanged; [`prepare`] then reuses it instead of
/// walking and shaping every cell again.
pub(super) struct Scene {
    frame: Arc<GridFrame>,
    bounds: Bounds<Pixels>,
    scale: f32,
    settings: Settings,
    geometry: Geometry,
    background: Hsla,
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    inks: Vec<Ink>,
    /// Decorations, then the cursor: painted over the glyphs, in order.
    overlays: Vec<(Bounds<Pixels>, Hsla)>,
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
    /// Painted when there is no scene (no frame, or a stale one).
    fallback: Hsla,
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
            view_id: settings.view_id,
            ..Default::default()
        },
        fallback: settings.background.unwrap_or_else(gpui::black),
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
    let scene = Rc::new(Builder::new(frame, settings, bounds, window).build(window));
    *cache.borrow_mut() = Some(Rc::clone(&scene));
    scene
}

fn opacity_layers(
    scene: &Scene,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> Vec<OpacityLayer> {
    scene
        .inks
        .iter()
        .filter_map(|ink| match ink {
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
                scale: window.scale_factor(),
                settings,
                geometry,
                background: defaults.1,
                backgrounds: Vec::new(),
                inks: Vec::new(),
                overlays: Vec::new(),
                glyphs: Vec::new(),
            },
        }
    }

    fn build(mut self, window: &gpui::Window) -> Scene {
        let frame = Arc::clone(&self.scene.frame);
        for row in 0..frame.rows {
            for col in 0..frame.cols {
                self.prepare_cell(&frame, row, col, window);
            }
        }
        self.flush_background();
        self.flush_glyph_run(window);
        self.prepare_cursor(&frame);
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
        self.prepare_decorations(cell, bounds, colors);
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
            None,
        );
        let Some(glyphs) = one_glyph_per_byte(&line, len) else {
            for (offset, byte) in text.bytes().enumerate() {
                let style = TextStyle {
                    font: self.fonts.cell[run.style].clone(),
                    color: run.color,
                    opacity: 1.,
                };
                let cell = (run.row, run.start + offset as u16, 1);
                self.shape_cell(cell, char::from(byte).to_string(), style, window);
            }
            return;
        };
        for offset in 0..len {
            let col = run.start + offset as u16;
            let bounds = geometry.cell_bounds(run.row, col, 1);
            self.observe(run.row, col, bounds, None, run.color);
        }
        self.scene.inks.push(Ink::Run(GlyphRun {
            bounds: geometry.cell_bounds(run.row, run.start, len as u16),
            color: run.color,
            glyphs,
        }));
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
        self.scene.inks.push(Ink::Glyph(Box::new(Glyph {
            line,
            bounds,
            opacity,
        })));
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
            self.scene.backgrounds.push((bounds, run.color));
        }
    }

    fn prepare_decorations(&mut self, cell: &Cell, bounds: Bounds<Pixels>, mut colors: CellColors) {
        colors.foreground = colors.foreground.opacity(colors.opacity);
        colors.underline = colors.underline.opacity(colors.opacity);
        let thickness = px(1. / self.scene.geometry.scale);
        let bottom = bounds.bottom() - thickness * 2.;
        match cell.underline {
            1 => self.rule(bounds, bottom, thickness, colors.underline),
            2 => {
                self.rule(bounds, bottom, thickness, colors.underline);
                self.rule(bounds, bottom - thickness * 2., thickness, colors.underline);
            }
            3..=5 => self.patterned_underline(
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
                bounds,
                bounds.top() + bounds.size.height * 0.55,
                thickness,
                colors.foreground,
            );
        }
        if cell.flags & CELL_OVERLINE != 0 {
            self.rule(bounds, bounds.top(), thickness, colors.foreground);
        }
    }

    fn rule(&mut self, bounds: Bounds<Pixels>, y: Pixels, thickness: Pixels, color: Hsla) {
        self.scene.overlays.push((
            Bounds::new(point(bounds.left(), y), size(bounds.size.width, thickness)),
            color,
        ));
    }

    fn patterned_underline(
        &mut self,
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
            self.scene.overlays.push((
                Bounds::new(
                    point(bounds.left() + x, y - thickness * offset),
                    size(width.min(bounds.size.width - x), thickness),
                ),
                color,
            ));
            x += thickness * step;
            index += 1;
        }
    }

    fn prepare_cursor(&mut self, frame: &GridFrame) {
        let settings = &self.scene.settings;
        if !cursor_visible(frame, settings) {
            return;
        }
        let geometry = self.scene.geometry;
        let (row, col) = cursor_head(frame);
        let bounds =
            geometry.cell_bounds(row, col, if frame.cursor.width.is_wide() { 2 } else { 1 });
        let color = settings
            .cursor
            .or_else(|| frame.colors.cursor.map(rgb))
            .unwrap_or(self.defaults.0);
        let thickness = px(1. / geometry.scale);
        let style = if settings.focused {
            frame.cursor.style
        } else {
            CursorStyle::BlockHollow
        };
        let scene = &mut self.scene;
        match style {
            CursorStyle::Block => scene.backgrounds.push((bounds, color)),
            CursorStyle::Bar => scene.overlays.push((
                Bounds::new(bounds.origin, size(thickness * 2., bounds.size.height)),
                color,
            )),
            CursorStyle::Underline => scene.overlays.push((
                Bounds::new(
                    point(bounds.left(), bounds.bottom() - thickness * 2.),
                    size(bounds.size.width, thickness * 2.),
                ),
                color,
            )),
            CursorStyle::BlockHollow => scene
                .overlays
                .extend(outline(bounds, thickness).map(|bounds| (bounds, color))),
        }
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
                window.paint_quad(fill(bounds, self.fallback));
            });
            return self.finish(started);
        };
        let mut layers = std::mem::take(&mut self.layers).into_iter();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(fill(bounds, scene.background));
            for (bounds, color) in &scene.backgrounds {
                window.paint_quad(fill(*bounds, *color));
            }
            let geometry = scene.geometry;
            let font_size = px(scene.settings.font_size);
            for ink in &scene.inks {
                let result = match ink {
                    Ink::Glyph(glyph) if glyph.opacity < 1. => layers
                        .next()
                        .ok_or_else(|| "opacity layer missing".to_owned())
                        .and_then(|layer| layer.paint(window, cx)),
                    Ink::Glyph(glyph) => paint_line(&glyph.line, glyph.bounds, geometry, window, cx),
                    Ink::Run(run) => run.paint(geometry, font_size, window),
                };
                if let Err(error) = result {
                    self.report.error = Some(error);
                }
            }
            for (bounds, color) in &scene.overlays {
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
    /// Each glyph at its own cell's origin on the shared baseline, exactly
    /// where a cell shaped alone would put it.
    fn paint(
        &self,
        geometry: Geometry,
        font_size: Pixels,
        window: &mut gpui::Window,
    ) -> Result<(), String> {
        let bounds = self.bounds;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let baseline = bounds.origin.y + geometry.baseline;
            let mut x = bounds.origin.x;
            for (font, glyph) in &self.glyphs {
                window
                    .paint_glyph(point(x, baseline), *font, *glyph, font_size, self.color)
                    .map_err(|error| error.to_string())?;
                x += geometry.cell_width;
            }
            Ok(())
        })
    }
}

/// The glyphs of a line shaped from `len` bytes of ASCII, when each byte
/// became exactly one non-emoji glyph in order.
fn one_glyph_per_byte(line: &ShapedLine, len: usize) -> Option<Vec<(FontId, GlyphId)>> {
    let mut glyphs = Vec::with_capacity(len);
    for run in &line.runs {
        for glyph in &run.glyphs {
            if glyph.is_emoji || glyph.index != glyphs.len() {
                return None;
            }
            glyphs.push((run.font_id, glyph.id));
        }
    }
    (glyphs.len() == len).then_some(glyphs)
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
            terminal_id: phux_client_ffi::projection::id::parse("local:1").expect("fixture id"),
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
                overlays: Vec::new(),
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
            .map(|(bounds, color)| (bounds.origin.x, bounds.origin.y, bounds.size.width, *color))
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
        // Invalidation tokens and props the painter never reads keep it.
        let mut unpainted = settings.clone();
        unpainted.set("paintRevision", &serde_json::json!(7));
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
            |settings| settings.set("theme", &serde_json::json!({"foreground": "#123456"})),
            |settings| settings.set("font", &serde_json::json!({"family": "Monaco"})),
            |settings| settings.set("font", &serde_json::json!({"cellWidth": 1.1})),
        ];
        for change in changes {
            let mut changed = settings.clone();
            change(&mut changed);
            assert!(!scene.reusable(&same, bounds, 1., &changed));
        }
    }

    #[test]
    fn styles_index_the_four_faces() {
        let fonts = Fonts::new(&gpui::font("Menlo"));
        let face = |flags| &fonts.cell[style(&Cell { flags, ..Default::default() })];
        assert_eq!(face(0).weight, gpui::FontWeight::default());
        assert_eq!(face(CELL_BOLD).weight, gpui::FontWeight::BOLD);
        assert_eq!(face(CELL_ITALIC).style, gpui::FontStyle::Italic);
        let both = face(CELL_BOLD | CELL_ITALIC);
        assert_eq!((both.weight, both.style), (gpui::FontWeight::BOLD, gpui::FontStyle::Italic));
        assert!(fonts.run.iter().all(|font| font.features != fonts.cell[0].features));
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
        let colors = cell_colors(&cell, metadata, &frame, &settings, defaults(&frame, &settings));
        assert_eq!(colors.foreground, gpui::rgb(0x0000ff).into());
        assert_eq!(colors.background, gpui::rgb(0xff0000).into());
        cell.flags |= CELL_SELECTED | CELL_FAINT;
        let colors = cell_colors(&cell, metadata, &frame, &settings, defaults(&frame, &settings));
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
        let colors = cell_colors(&cell, metadata, &frame, &settings, defaults(&frame, &settings));
        assert_eq!(colors.foreground, settings.foreground.expect("foreground"));
        assert_eq!(colors.underline, colors.foreground);
        frame.colors.reversed = true;
        let colors = cell_colors(&cell, metadata, &frame, &settings, defaults(&frame, &settings));
        assert_eq!(colors.foreground, settings.background.expect("background"));
        frame.colors.has_foreground = true;
        frame.colors.foreground = Rgb { r: 255, g: 0, b: 0 };
        assert_eq!(
            cell_colors(&cell, metadata, &frame, &settings, defaults(&frame, &settings)).foreground,
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
