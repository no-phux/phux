//! Shared scene primitives, observations, and builder state.

use super::*;

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
pub(super) struct Glyph {
    pub(super) line: ShapedLine,
    pub(super) bounds: Bounds<Pixels>,
    /// Monochrome glyph colour, faint alpha included.
    pub(super) color: Hsla,
    /// Below 1 only for a faint line holding a colour emoji, which GPUI paints
    /// without its `TextRun` colour. That glyph paints inside an opacity layer
    /// with the opaque colour instead.
    pub(super) opacity: f32,
}

/// Consecutive single-width cells of one row whose glyphs all come from the
/// cell fonts, one glyph per cell. Spaces and blank cells between them stay
/// in the run, and each colour or face change is another `TextRun` of the
/// same line. Shaped once with each column forced to the cell width.
#[derive(Clone)]
pub(super) struct GlyphRun {
    pub(super) bounds: Bounds<Pixels>,
    pub(super) line: ShapedLine,
    /// The colour of each span of `line`, as (end byte, colour), in order.
    pub(super) colors: Vec<(usize, Hsla)>,
}

#[derive(Clone)]
pub(super) enum Ink {
    Glyph(Box<Glyph>),
    // ShapedLine carries a 32-wide decoration buffer, so the run stays boxed.
    Run(Box<GlyphRun>),
}

/// One painted quad tagged with the row that produced it, so a later frame
/// can drop that row and keep the rest.
#[derive(Clone, Copy)]
pub(super) struct RowQuad {
    pub(super) row: u16,
    pub(super) bounds: Bounds<Pixels>,
    pub(super) color: Hsla,
}

/// One shaped ink tagged with its row.
#[derive(Clone)]
pub(super) struct RowInk {
    pub(super) row: u16,
    pub(super) ink: Ink,
    /// Whether the ink's glyphs are in its row's [`RowSprites`]. An ink that
    /// is not (no retained raster, a resolve error, or a translucent emoji)
    /// paints from its shaped line.
    pub(super) resolved: bool,
}

/// One row's glyphs, resolved against the window's sprite atlas when the row
/// was shaped. Painting them is one GPUI call per row, with no shaping,
/// rasterizing or atlas lookup. A scroll moves the row by `dy` instead of
/// resolving it again: rows sit on whole device pixels, so a moved glyph
/// keeps its rasterization.
#[derive(Clone)]
pub(super) struct RowSprites {
    pub(super) row: u16,
    pub(super) dy: Pixels,
    pub(super) sprites: Arc<[crate::platform::Sprite]>,
}

/// How one cell's text is drawn.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct TextStyle {
    /// Index into [`Fonts`]: bold and italic bits.
    pub(super) face: usize,
    /// Opaque glyph colour.
    pub(super) color: Hsla,
    /// Faint text paints below 1.
    pub(super) opacity: f32,
}

/// `len` bytes of a pending run drawn in one style.
pub(super) struct Span {
    pub(super) len: usize,
    pub(super) style: TextStyle,
}

/// The run being collected: one character per cell, from `start` on `row`.
#[derive(Default)]
pub(super) struct PendingRun {
    pub(super) row: u16,
    pub(super) start: u16,
    /// Cells already in `text`.
    pub(super) cells: u16,
    pub(super) text: String,
    /// Blank cells after `text`, added as spaces only if a glyph follows.
    pub(super) blanks: u16,
    pub(super) spans: Vec<Span>,
    /// Whether every character is ASCII.
    pub(super) ascii: bool,
}

impl PendingRun {
    /// The column after the last cell, blanks included.
    pub(super) fn end(&self) -> usize {
        usize::from(self.start) + usize::from(self.cells) + usize::from(self.blanks)
    }

    pub(super) fn push(&mut self, ch: char, style: TextStyle) {
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

pub(super) struct OpacityLayer {
    pub(super) element: gpui::AnyElement,
    pub(super) result: Rc<RefCell<Option<Result<(), String>>>>,
}

/// Cells `start..end` of `row`, all painted `color`.
pub(super) struct BackgroundRun {
    pub(super) row: u16,
    pub(super) start: u16,
    pub(super) end: u16,
    pub(super) color: Hsla,
}

#[derive(Clone, Copy)]
pub(super) struct CellColors {
    pub(super) foreground: Hsla,
    pub(super) background: Hsla,
    pub(super) underline: Hsla,
    pub(super) opacity: f32,
}

/// The four faces a cell can ask for, indexed by [`style`]: built once per
/// scene so a cell costs an index, not a font clone and comparison.
pub(super) struct Fonts {
    pub(super) cell: [gpui::Font; 4],
    /// The same faces with ligatures off, for multi-cell runs.
    pub(super) run: [gpui::Font; 4],
    /// The resolved `run` faces. A run glyph from any other font is a
    /// fallback that may overhang its cell, so it is shaped and clipped alone.
    run_ids: std::cell::OnceCell<[gpui::FontId; 4]>,
}

impl Fonts {
    pub(super) fn new(base: &gpui::Font) -> Self {
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

    pub(super) fn run_ids(&self, window: &gpui::Window) -> [gpui::FontId; 4] {
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
pub(in crate::terminal) struct Scene {
    pub(super) frame: Arc<GridFrame>,
    pub(super) bounds: Bounds<Pixels>,
    pub(super) scale: f32,
    pub(super) settings: Settings,
    pub(super) geometry: Geometry,
    /// The faces `settings.font` asks for, kept while the scene is patched.
    pub(super) fonts: Rc<Fonts>,
    /// What `sprites` were resolved under. `None` when the window cannot
    /// retain glyphs, and then every ink paints from its shaped line.
    pub(super) raster: Option<crate::platform::Raster>,
    pub(super) background: Hsla,
    pub(super) backgrounds: Vec<RowQuad>,
    pub(super) inks: Vec<RowInk>,
    /// Resolved glyphs, one entry per row that has any, in row order.
    pub(super) sprites: Vec<RowSprites>,
    /// Underlines, strike, and overline, in row order.
    pub(super) decorations: Vec<RowQuad>,
    /// Solid block cursor, painted over cell backgrounds and under glyphs.
    pub(super) cursor_blocks: Vec<(Bounds<Pixels>, Hsla)>,
    /// Bar, underline, and hollow cursors, painted over glyphs.
    pub(super) cursor_overlays: Vec<(Bounds<Pixels>, Hsla)>,
    pub(super) glyphs: Vec<GlyphObservation>,
}

impl Scene {
    /// Whether this scene paints `frame` in `bounds` exactly as a fresh one
    /// would. The frame is compared by identity: a publication is immutable,
    /// and the scene's own reference keeps its allocation from being reused.
    pub(super) fn reusable(
        &self,
        frame: &Arc<GridFrame>,
        bounds: Bounds<Pixels>,
        (scale, raster): (f32, Option<crate::platform::Raster>),
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
pub(in crate::terminal) type SceneCache = Rc<RefCell<Option<Rc<Scene>>>>;

pub(in crate::terminal) struct Prepared {
    pub(super) report: Observation,
    /// Painted when there is no scene (no frame, or a stale one), unless the
    /// host paints the background.
    pub(super) fallback: Option<Hsla>,
    pub(super) scene: Option<Rc<Scene>>,
    /// One per translucent glyph ink, in ink order. They wrap per-draw GPUI
    /// elements, so they are rebuilt even for a reused scene.
    pub(super) layers: Vec<OpacityLayer>,
    /// How far the grid paints above its whole-row position, in device
    /// pixels: zero, or minus a sub-row scroll fraction of a cell.
    pub(super) shift: Pixels,
}

/// Walks one frame's cells into a [`Scene`].
pub(super) struct Builder {
    pub(super) scene: Scene,
    pub(super) fonts: Rc<Fonts>,
    /// `scene.settings` as the runtime's paint rules read them.
    pub(super) theme: Theme<Hsla>,
    /// Default foreground and background after theme and reverse video.
    pub(super) defaults: (Hsla, Hsla),
    /// The cell under a solid block cursor, whose glyph takes the background.
    pub(super) solid_cursor: Option<(u16, u16)>,
    /// The open run of same-coloured cells on one row, merged into one quad.
    pub(super) run: Option<BackgroundRun>,
    pub(super) pending: Option<PendingRun>,
    /// Glyphs resolved for the row being built.
    pub(super) row_sprites: Vec<crate::platform::Sprite>,
}
