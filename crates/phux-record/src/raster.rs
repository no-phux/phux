//! `RenderedFrame` to RGB pixels.
//!
//! The input is `phux_core`'s dense [`RenderedFrame`], not a libghostty
//! handle, so tests use hand-built frames.
//!
//! Every pixel written is exactly some cell's resolved fg or bg; the only
//! colour arithmetic is `faint`, adding one colour per (fg, bg) pair
//! (ADR-0060). That keeps GIF palettes exact. [`Rasterizer::colors_of`]
//! reports a frame's colours without drawing it and shares `paint_of` and
//! `glyph_of` with [`Rasterizer::draw`] so the two cannot drift.

use std::collections::HashSet;

use phux_core::screen::{CellColor, CellStyle, RenderedCell, RenderedFrame};

use crate::font::{BitmapFont, SPLEEN_8X16, bold_row, boxdraw};

/// The resolved colour table an export is drawn against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Theme {
    pub(crate) fg: [u8; 3],
    pub(crate) bg: [u8; 3],
    /// The 256-entry palette: 16 ANSI names, the 6x6x6 cube, the grey ramp.
    pub(crate) palette: [[u8; 3]; 256],
}

impl Default for Theme {
    /// The standard xterm-256 table.
    fn default() -> Self {
        const ANSI: [[u8; 3]; 16] = [
            [0x00, 0x00, 0x00],
            [0x80, 0x00, 0x00],
            [0x00, 0x80, 0x00],
            [0x80, 0x80, 0x00],
            [0x00, 0x00, 0x80],
            [0x80, 0x00, 0x80],
            [0x00, 0x80, 0x80],
            [0xc0, 0xc0, 0xc0],
            [0x80, 0x80, 0x80],
            [0xff, 0x00, 0x00],
            [0x00, 0xff, 0x00],
            [0xff, 0xff, 0x00],
            [0x00, 0x00, 0xff],
            [0xff, 0x00, 0xff],
            [0x00, 0xff, 0xff],
            [0xff, 0xff, 0xff],
        ];
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

        let mut palette = [[0_u8; 3]; 256];
        palette[..16].copy_from_slice(&ANSI);
        for (i, slot) in palette[16..232].iter_mut().enumerate() {
            *slot = [LEVELS[i / 36], LEVELS[i / 6 % 6], LEVELS[i % 6]];
        }
        for (step, slot) in (0_u8..).zip(&mut palette[232..]) {
            let level = 8 + 10 * step;
            *slot = [level; 3];
        }
        Self {
            fg: [0xd0, 0xd0, 0xd0],
            bg: [0x00, 0x00, 0x00],
            palette,
        }
    }
}

/// A row-major RGB pixel buffer.
///
/// The pixel at `(x, y)` is `pixels[y * width + x]`. Accesses are
/// bounds-checked so a bad index never panics mid-export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Surface {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Vec<[u8; 3]>,
}

/// What a cell's grapheme resolves to, shared by the paint and histogram
/// paths.
#[derive(Debug, Clone, Copy)]
enum Glyph {
    /// A blank cell, or the empty right half of a wide cluster.
    Blank,
    /// A codepoint the procedural renderer draws.
    Boxed(char),
    /// A bitmap from the vendored face.
    Bitmap(&'static [u8; 16]),
    /// No coverage anywhere: a hollow box, never a blank.
    Tofu,
}

/// A cell's resolved foreground and background, after every attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Paint {
    fg: [u8; 3],
    bg: [u8; 3],
}

/// Paints [`RenderedFrame`]s onto a [`Surface`] with a fixed-cell font.
#[derive(Debug)]
pub(crate) struct Rasterizer {
    font: &'static BitmapFont,
    theme: Theme,
}

impl Rasterizer {
    /// Build a rasterizer over the vendored face and `theme`.
    pub(crate) const fn new(theme: Theme) -> Self {
        Self {
            font: &SPLEEN_8X16,
            theme,
        }
    }

    pub(crate) const fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Pixel size of one cell: `(width, height)`.
    pub(crate) const fn cell_size(&self) -> (u32, u32) {
        (self.font.cell_w, self.font.cell_h)
    }

    /// Allocate a background-filled surface sized for `cols` x `rows` cells.
    pub(crate) fn surface_for(&self, cols: u16, rows: u16) -> Surface {
        let (cell_w, cell_h) = self.cell_size();
        let width = u32::from(cols).saturating_mul(cell_w);
        let height = u32::from(rows).saturating_mul(cell_h);
        let len = usize::try_from(width)
            .unwrap_or(usize::MAX)
            .saturating_mul(usize::try_from(height).unwrap_or(usize::MAX));
        Surface {
            width,
            height,
            pixels: vec![self.theme.bg; len],
        }
    }

    /// Paint `frame` onto `surface`, letterboxing a smaller frame against the
    /// theme background.
    ///
    /// All backgrounds go down before any glyph: a wide cluster paints into
    /// the next column, whose own background would otherwise erase it.
    pub(crate) fn draw(&self, frame: &RenderedFrame, surface: &mut Surface) {
        let (cell_w, cell_h) = self.cell_size();
        surface.pixels.fill(self.theme.bg);

        for row in 0..frame.rows {
            for col in 0..frame.cols {
                let Some(cell) = frame.cell(row, col) else {
                    continue;
                };
                let paint = self.paint_of(&cell.style, cursor_at(frame, row, col));
                let slot = Slot::new(row, col, cell_w, cell_h);
                fill_rect(surface, slot.x, slot.y, slot.w, slot.h, paint.bg);
            }
        }

        for row in 0..frame.rows {
            for col in 0..frame.cols {
                let Some(cell) = frame.cell(row, col) else {
                    continue;
                };
                let paint = self.paint_of(&cell.style, cursor_at(frame, row, col));
                // The base of a wide cluster gets a double-width box.
                let slot = Slot::new(row, col, cell_w, cell_h).widened(is_wide_base(
                    frame,
                    row,
                    col,
                    &cell.grapheme,
                ));
                self.draw_glyph(surface, slot, cell, paint.fg);
                decorate(surface, slot, &cell.style, paint.fg);
            }
        }
    }

    /// Insert every color `draw` would emit for `frame` into `out`.
    ///
    /// The GIF global table is built from this; it must match `draw` exactly.
    pub(crate) fn colors_of(&self, frame: &RenderedFrame, out: &mut HashSet<[u8; 3]>) {
        for row in 0..frame.rows {
            for col in 0..frame.cols {
                let Some(cell) = frame.cell(row, col) else {
                    continue;
                };
                let paint = self.paint_of(&cell.style, cursor_at(frame, row, col));
                let glyph = self.glyph_of(&cell.grapheme);
                // Background survives unless the glyph covers the whole cell.
                if !fills_cell(glyph) {
                    out.insert(paint.bg);
                }
                if emits_ink(glyph) || has_decoration(&cell.style) {
                    out.insert(paint.fg);
                }
            }
        }
    }

    /// Resolve one cell's colors, applying every attribute exactly once.
    ///
    /// Order matters: base colours, `inverse` (the cursor is one more
    /// inversion), `faint`, then `invisible`.
    fn paint_of(&self, style: &CellStyle, cursor: bool) -> Paint {
        let mut fg = self.resolve(style.fg, self.theme.fg);
        let mut bg = self.resolve(style.bg, self.theme.bg);
        if style.inverse != cursor {
            std::mem::swap(&mut fg, &mut bg);
        }
        if style.faint {
            fg = blend(fg, bg);
        }
        if style.invisible {
            fg = bg;
        }
        Paint { fg, bg }
    }

    /// `CellColor` to RGB; a palette index resolves through the theme.
    fn resolve(&self, color: CellColor, default: [u8; 3]) -> [u8; 3] {
        match color {
            CellColor::Default => default,
            CellColor::Palette { index } => self
                .theme
                .palette
                .get(usize::from(index))
                .copied()
                .unwrap_or(default),
            CellColor::Rgb { r, g, b } => [r, g, b],
        }
    }

    /// Classify a cell's grapheme once: box glyph, bitmap, tofu, or blank.
    fn glyph_of(&self, grapheme: &str) -> Glyph {
        let Some(ch) = grapheme.chars().next() else {
            return Glyph::Blank;
        };
        if ch == ' ' {
            return Glyph::Blank;
        }
        // The lookup order documented on `boxdraw::covers_fallback`.
        if boxdraw::covers(ch) {
            return Glyph::Boxed(ch);
        }
        if let Some(bitmap) = self.font.glyph(ch) {
            return Glyph::Bitmap(bitmap);
        }
        if boxdraw::covers_fallback(ch) {
            return Glyph::Boxed(ch);
        }
        Glyph::Tofu
    }

    fn draw_glyph(&self, surface: &mut Surface, slot: Slot, cell: &RenderedCell, fg: [u8; 3]) {
        let Slot {
            x: x0,
            y: y0,
            w: box_w,
            h: cell_h,
        } = slot;
        match self.glyph_of(&cell.grapheme) {
            Glyph::Blank => {}
            Glyph::Boxed(ch) => {
                let mut put = |px: u32, py: u32| {
                    put_pixel(surface, x0.saturating_add(px), y0.saturating_add(py), fg);
                };
                boxdraw::draw(ch, box_w, cell_h, &mut put);
            }
            Glyph::Bitmap(bitmap) => {
                // Centre rather than stretch in a wide box.
                let inset = box_w.saturating_sub(self.font.cell_w) / 2;
                for (dy, byte) in bitmap.iter().enumerate() {
                    let Ok(dy) = u32::try_from(dy) else { continue };
                    if dy >= cell_h {
                        break;
                    }
                    let row = if cell.style.bold {
                        bold_row(*byte)
                    } else {
                        *byte
                    };
                    for dx in 0..8_u32 {
                        if row & (0x80 >> dx) == 0 {
                            continue;
                        }
                        put_pixel(
                            surface,
                            x0.saturating_add(inset).saturating_add(dx),
                            y0.saturating_add(dy),
                            fg,
                        );
                    }
                }
            }
            Glyph::Tofu => {
                // A hollow box, one pixel inset, never a blank.
                if box_w < 3 || cell_h < 3 {
                    return;
                }
                let (right, bottom) = (box_w - 2, cell_h - 2);
                for x in 1..=right {
                    put_pixel(surface, x0 + x, y0 + 1, fg);
                    put_pixel(surface, x0 + x, y0 + bottom, fg);
                }
                for y in 1..=bottom {
                    put_pixel(surface, x0 + 1, y0 + y, fg);
                    put_pixel(surface, x0 + right, y0 + y, fg);
                }
            }
        }
    }
}

/// Whether a classified glyph paints any foreground pixels. Every procedural
/// glyph does (asserted in `boxdraw`'s tests).
fn emits_ink(glyph: Glyph) -> bool {
    match glyph {
        Glyph::Blank => false,
        Glyph::Boxed(_) | Glyph::Tofu => true,
        Glyph::Bitmap(bitmap) => bitmap.iter().any(|row| *row != 0),
    }
}

/// Whether a classified glyph covers every pixel of its cell.
fn fills_cell(glyph: Glyph) -> bool {
    match glyph {
        Glyph::Boxed(ch) => ch == '\u{2588}',
        Glyph::Bitmap(bitmap) => bitmap.iter().all(|row| *row == 0xff),
        Glyph::Blank | Glyph::Tofu => false,
    }
}

/// Whether any line decoration paints foreground across the cell.
const fn has_decoration(style: &CellStyle) -> bool {
    style.underline || style.strikethrough || style.overline
}

/// Whether `(row, col)` is where the composited cursor sits.
fn cursor_at(frame: &RenderedFrame, row: u16, col: u16) -> bool {
    frame
        .cursor
        .as_ref()
        .is_some_and(|c| c.visible && c.y == row && c.x == col)
}

/// Underline, strikethrough, overline, drawn after the glyph. `blink` and
/// `italic` are deliberately ignored at this resolution.
fn decorate(surface: &mut Surface, slot: Slot, style: &CellStyle, fg: [u8; 3]) {
    if slot.h == 0 {
        return;
    }
    if style.underline {
        fill_rect(surface, slot.x, slot.y + slot.h - 1, slot.w, 1, fg);
    }
    if style.strikethrough {
        fill_rect(surface, slot.x, slot.y + slot.h / 2, slot.w, 1, fg);
    }
    if style.overline {
        fill_rect(surface, slot.x, slot.y, slot.w, 1, fg);
    }
}

/// Where one cell lands on the surface, in pixels. `w` is two cells for the
/// base of a double-width cluster.
#[derive(Debug, Clone, Copy)]
struct Slot {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Slot {
    fn new(row: u16, col: u16, cell_w: u32, cell_h: u32) -> Self {
        Self {
            x: u32::from(col).saturating_mul(cell_w),
            y: u32::from(row).saturating_mul(cell_h),
            w: cell_w,
            h: cell_h,
        }
    }

    const fn widened(self, wide: bool) -> Self {
        if wide {
            Self {
                w: self.w.saturating_mul(2),
                ..self
            }
        } else {
            self
        }
    }
}

/// Whether the cell at `(row, col)` is the base of a double-width cluster.
///
/// phux-core's convention: the trailing column of a wide cluster holds the
/// empty string.
fn is_wide_base(frame: &RenderedFrame, row: u16, col: u16, grapheme: &str) -> bool {
    if grapheme.is_empty() || grapheme == " " {
        return false;
    }
    frame
        .cell(row, col.saturating_add(1))
        .is_some_and(|next| next.grapheme.is_empty())
}

/// Move `fg` 40% of the way toward `bg`: the pipeline's only colour blend.
fn blend(fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
    let mix = |a: u8, b: u8| {
        let value = (u16::from(a) * 3 + u16::from(b) * 2) / 5;
        u8::try_from(value).unwrap_or(u8::MAX)
    };
    [mix(fg[0], bg[0]), mix(fg[1], bg[1]), mix(fg[2], bg[2])]
}

/// Write one pixel, silently dropping anything outside the surface.
fn put_pixel(surface: &mut Surface, x: u32, y: u32, color: [u8; 3]) {
    if x >= surface.width || y >= surface.height {
        return;
    }
    let Ok(idx) = usize::try_from(u64::from(y) * u64::from(surface.width) + u64::from(x)) else {
        return;
    };
    if let Some(pixel) = surface.pixels.get_mut(idx) {
        *pixel = color;
    }
}

/// Fill `w` x `h` pixels from `(x0, y0)`, clipped to the surface.
fn fill_rect(surface: &mut Surface, x0: u32, y0: u32, w: u32, h: u32, color: [u8; 3]) {
    for y in y0..y0.saturating_add(h) {
        if y >= surface.height {
            break;
        }
        for x in x0..x0.saturating_add(w) {
            if x >= surface.width {
                break;
            }
            put_pixel(surface, x, y, color);
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_core::screen::CursorState;

    const FG: [u8; 3] = [0x11, 0x22, 0x33];
    const BG: [u8; 3] = [0x44, 0x55, 0x66];

    fn set(frame: &mut RenderedFrame, row: u16, col: u16, grapheme: &str, style: CellStyle) {
        let cell = frame.cell_mut(row, col).expect("cell in range");
        cell.grapheme = grapheme.to_owned();
        cell.style = style;
    }

    /// A style with explicit truecolor fg/bg, independent of the theme.
    fn styled() -> CellStyle {
        CellStyle {
            fg: CellColor::Rgb {
                r: FG[0],
                g: FG[1],
                b: FG[2],
            },
            bg: CellColor::Rgb {
                r: BG[0],
                g: BG[1],
                b: BG[2],
            },
            ..CellStyle::default()
        }
    }

    /// Render one cell holding `grapheme` in `style`.
    fn one(grapheme: &str, style: CellStyle) -> Surface {
        let mut frame = RenderedFrame::blank(1, 1);
        set(&mut frame, 0, 0, grapheme, style);
        render(&frame)
    }

    fn render(frame: &RenderedFrame) -> Surface {
        let raster = Rasterizer::new(Theme::default());
        let mut surface = raster.surface_for(frame.cols, frame.rows);
        raster.draw(frame, &mut surface);
        surface
    }

    fn px(surface: &Surface, x: u32, y: u32) -> [u8; 3] {
        surface.pixels[(y * surface.width + x) as usize]
    }

    #[test]
    fn palette_index_resolves_through_the_xterm_table() {
        let theme = Theme::default();
        assert_eq!(theme.palette[196], [0xff, 0x00, 0x00]);
        assert_eq!(theme.palette[231], [0xff, 0xff, 0xff]);
        assert_eq!(theme.palette[255], [0xee, 0xee, 0xee]);
        let style = CellStyle {
            bg: CellColor::Palette { index: 196 },
            ..CellStyle::default()
        };
        assert_eq!(px(&one(" ", style), 0, 0), theme.palette[196]);
    }

    #[test]
    fn attributes_resolve_in_order() {
        let normal = one("A", styled());
        let inverted = one(
            "A",
            CellStyle {
                inverse: true,
                ..styled()
            },
        );
        for (a, b) in normal.pixels.iter().zip(&inverted.pixels) {
            assert_eq!(*b, if *a == FG { BG } else { FG }, "inverse did not swap");
        }
        let invisible = one(
            "A",
            CellStyle {
                invisible: true,
                ..styled()
            },
        );
        assert!(
            invisible.pixels.iter().all(|p| *p == BG),
            "invisible showed ink"
        );
        let faint = one(
            "\u{2588}",
            CellStyle {
                faint: true,
                ..styled()
            },
        );
        assert_eq!(px(&faint, 0, 0), [0x25, 0x36, 0x47], "faint blend");
        // Bold widens the glyph without introducing a colour.
        let thin = one("l", styled());
        let thick = one(
            "l",
            CellStyle {
                bold: true,
                ..styled()
            },
        );
        let ink = |s: &Surface| s.pixels.iter().filter(|p| **p == FG).count();
        assert!(ink(&thick) > ink(&thin), "bold did not widen the glyph");
        assert!(thick.pixels.iter().all(|p| *p == FG || *p == BG));
    }

    #[test]
    fn decorations_fill_exactly_one_pixel_row() {
        let base = styled();
        let cases = [
            (
                CellStyle {
                    underline: true,
                    ..base
                },
                15,
            ),
            (
                CellStyle {
                    strikethrough: true,
                    ..base
                },
                8,
            ),
            (
                CellStyle {
                    overline: true,
                    ..base
                },
                0,
            ),
        ];
        for (style, row) in cases {
            let surface = one(" ", style);
            for y in 0..16 {
                for x in 0..8 {
                    let want = if y == row { FG } else { BG };
                    assert_eq!(px(&surface, x, y), want, "row {row}: ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn wide_glyph_spans_two_cells_and_tail_keeps_its_own_bg() {
        const TAIL_BG: [u8; 3] = [0x01, 0x02, 0x03];
        let tail = CellStyle {
            bg: CellColor::Rgb {
                r: TAIL_BG[0],
                g: TAIL_BG[1],
                b: TAIL_BG[2],
            },
            ..CellStyle::default()
        };
        let mut frame = RenderedFrame::blank(3, 1);
        set(&mut frame, 0, 0, "\u{2588}", styled());
        set(&mut frame, 0, 1, "", tail);
        let surface = render(&frame);
        for x in 0..16 {
            assert_eq!(px(&surface, x, 8), FG, "wide glyph gap at x={x}");
        }
        assert_eq!(px(&surface, 16, 0), Theme::default().bg);

        // A narrow glyph in a wide box is centred, leaving the tail's own
        // background at the far edge.
        set(&mut frame, 0, 0, "A", styled());
        assert_eq!(px(&render(&frame), 15, 8), TAIL_BG);
    }

    #[test]
    fn unmapped_codepoint_draws_a_hollow_box_not_a_blank() {
        let surface = one("\u{6f22}", styled());
        assert_eq!(px(&surface, 1, 1), FG);
        assert_eq!(px(&surface, 6, 14), FG);
        assert_eq!(px(&surface, 4, 8), BG);
        assert_eq!(px(&surface, 0, 0), BG);
    }

    #[test]
    fn a_visible_cursor_inverts_exactly_its_cell() {
        let mut frame = RenderedFrame::blank(2, 1);
        set(&mut frame, 0, 0, "A", styled());
        set(&mut frame, 0, 1, "B", styled());
        let plain = render(&frame);

        frame.cursor = Some(CursorState {
            x: 0,
            y: 0,
            visible: false,
        });
        assert_eq!(render(&frame).pixels, plain.pixels, "hidden cursor drew");

        frame.cursor = Some(CursorState {
            x: 0,
            y: 0,
            visible: true,
        });
        let with_cursor = render(&frame);
        for y in 0..16 {
            for x in 0..16 {
                let before = px(&plain, x, y);
                let want = match (x < 8, before == FG) {
                    (false, _) => before,
                    (true, true) => BG,
                    (true, false) => FG,
                };
                assert_eq!(px(&with_cursor, x, y), want, "({x}, {y})");
            }
        }
    }

    #[test]
    fn colors_of_returns_exactly_the_colors_draw_emits() {
        // The drift guard: every branch `draw` can take.
        let mut f = RenderedFrame::blank(6, 2);
        set(&mut f, 0, 0, "A", styled());
        let palette_fg = CellStyle {
            fg: CellColor::Palette { index: 42 },
            ..CellStyle::default()
        };
        set(&mut f, 0, 1, "\u{2500}", palette_fg);
        let underlined = CellStyle {
            underline: true,
            ..styled()
        };
        set(&mut f, 0, 2, "\u{6f22}", underlined);
        set(&mut f, 0, 3, "", CellStyle::default());
        let faint = CellStyle {
            faint: true,
            ..styled()
        };
        set(&mut f, 0, 4, "g", faint);
        let inverse = CellStyle {
            inverse: true,
            ..styled()
        };
        set(&mut f, 0, 5, "\u{2588}", inverse);
        set(&mut f, 1, 0, " ", styled());
        let indexed_bg = CellStyle {
            bg: CellColor::Palette { index: 17 },
            ..CellStyle::default()
        };
        set(&mut f, 1, 1, "\u{2593}", indexed_bg);
        set(&mut f, 1, 2, "W", styled());
        set(&mut f, 1, 3, "", styled());
        f.cursor = Some(CursorState {
            x: 0,
            y: 1,
            visible: true,
        });

        let raster = Rasterizer::new(Theme::default());
        let mut surface = raster.surface_for(f.cols, f.rows);
        raster.draw(&f, &mut surface);
        let painted: HashSet<[u8; 3]> = surface.pixels.iter().copied().collect();
        let mut reported = HashSet::new();
        raster.colors_of(&f, &mut reported);
        assert_eq!(reported, painted, "colors_of drifted from draw");
    }

    #[test]
    fn draw_letterboxes_a_frame_smaller_than_the_surface() {
        let raster = Rasterizer::new(Theme::default());
        let mut surface = raster.surface_for(4, 2);
        let mut f = RenderedFrame::blank(2, 1);
        set(&mut f, 0, 0, "\u{2588}", styled());
        raster.draw(&f, &mut surface);
        assert_eq!(px(&surface, 0, 0), FG);
        assert_eq!(px(&surface, 24, 16), Theme::default().bg);
    }

    #[test]
    fn the_fallback_tier_never_steals_a_codepoint_the_face_can_draw() {
        let raster = Rasterizer::new(Theme::default());
        for ch in ['A', '\u{2192}', '\u{25b2}', '\u{25cf}', '\u{e0b0}'] {
            assert!(
                matches!(raster.glyph_of(&ch.to_string()), Glyph::Bitmap(_)),
                "{ch:?} did not come from the face"
            );
        }
        for ch in ['\u{2500}', '\u{2588}', '\u{276f}', '\u{2713}', '\u{26a0}'] {
            assert!(
                matches!(raster.glyph_of(&ch.to_string()), Glyph::Boxed(_)),
                "{ch:?} fell through to the face or to tofu"
            );
        }
        assert!(matches!(raster.glyph_of("\u{6f22}"), Glyph::Tofu));
    }
}
