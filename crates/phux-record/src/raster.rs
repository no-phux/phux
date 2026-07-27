//! `RenderedFrame` to RGB pixels.
//!
//! The rasterizer's input boundary is deliberately `phux_core::screen::`
//! [`RenderedFrame`] — dense, row-major cells of grapheme + style — and not a
//! libghostty handle. That keeps every rasterizer test a hand-built frame
//! with no emulator in the loop, and it means the same code paints a frame
//! that came from an offline replay, from a live client snapshot, or from a
//! fixture.
//!
//! # The 1-bit invariant
//!
//! Every pixel this module writes is *exactly* some cell's resolved
//! foreground or resolved background. Nothing is blended, nothing is
//! antialiased, and the only colour arithmetic anywhere is the `faint`
//! attribute, which produces one more colour per (fg, bg) pair rather than
//! 256 (ADR-0060). That is what keeps a whole recording's distinct-colour
//! count under 256, which in turn is what lets the GIF encoder use an exact
//! palette with no quantization and no dithering.
//!
//! [`Rasterizer::colors_of`] exists to report that colour set for a frame
//! *without* rasterizing it, so the two-pass render driver can size its
//! palette before it encodes anything. It and [`Rasterizer::draw`] must never
//! disagree; they are written over one shared resolution helper
//! (`Rasterizer::paint_of`) and one shared glyph classifier
//! (`Rasterizer::glyph_of`) precisely so they cannot drift, and
//! `colors_of_returns_exactly_the_colors_draw_emits` is the guard.

use std::collections::HashSet;

use phux_core::screen::{CellColor, CellStyle, RenderedCell, RenderedFrame};

use crate::font::{BitmapFont, SPLEEN_8X16, bold_row, boxdraw};

/// The resolved color table an export is drawn against.
///
/// Not `Copy`: at 777 bytes it is exactly the sort of value that should move
/// or be borrowed explicitly rather than being silently memcpy'd through
/// every call in the pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// Default foreground, for `CellColor::Default` foregrounds.
    pub fg: [u8; 3],
    /// Default background, for `CellColor::Default` backgrounds.
    pub bg: [u8; 3],
    /// Cursor color.
    pub cursor: [u8; 3],
    /// The full 256-entry palette: 16 ANSI names, the 6x6x6 cube, the
    /// 24-step grey ramp.
    pub palette: [[u8; 3]; 256],
}

impl Default for Theme {
    /// The standard xterm-256 table.
    ///
    /// Indices 0..=15 are the classic ANSI names, 16..=231 the 6x6x6 cube on
    /// the levels `0, 95, 135, 175, 215, 255`, and 232..=255 the grey ramp
    /// `8 + 10 * i`. This is what a terminal with no theme configured shows,
    /// and it is what a palette-index cell must resolve through — the client
    /// deliberately preserves palette *identity* rather than flattening to
    /// RGB, so this lookup is where identity becomes pixels.
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
        let mut idx = 0_usize;
        while idx < 16 {
            palette[idx] = ANSI[idx];
            idx += 1;
        }
        // The 6x6x6 color cube, indices 16..=231.
        let mut r = 0_usize;
        while r < 6 {
            let mut g = 0_usize;
            while g < 6 {
                let mut b = 0_usize;
                while b < 6 {
                    palette[16 + 36 * r + 6 * g + b] = [LEVELS[r], LEVELS[g], LEVELS[b]];
                    b += 1;
                }
                g += 1;
            }
            r += 1;
        }
        // The 24-step grey ramp, indices 232..=255. Kept in `u8` arithmetic
        // (max 8 + 10 * 23 == 238) so no cast is involved at all.
        let mut step = 0_u8;
        while step < 24 {
            let level = 8 + 10 * step;
            palette[232 + step as usize] = [level, level, level];
            step += 1;
        }

        Self {
            fg: [0xd0, 0xd0, 0xd0],
            bg: [0x00, 0x00, 0x00],
            cursor: [0xd0, 0xd0, 0xd0],
            palette,
        }
    }
}

/// A row-major RGB pixel buffer.
///
/// `pixels.len() == width * height`, and the pixel at `(x, y)` is
/// `pixels[y * width + x]`. Every access in this crate goes through `get` /
/// `get_mut`: an out-of-range index must produce nothing, not a panic in the
/// middle of a user's export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row-major RGB pixels.
    pub pixels: Vec<[u8; 3]>,
}

/// What a cell's grapheme resolves to, once.
///
/// Computed by `Rasterizer::glyph_of` and consumed by both the paint path
/// and the colour-histogram path, so the two can never disagree about whether
/// a cell shows ink.
#[derive(Debug, Clone, Copy)]
enum Glyph {
    /// A blank cell, or the empty right half of a wide cluster. Paints
    /// background only.
    Blank,
    /// A codepoint the procedural renderer owns, either outright or as a
    /// fallback the face could not serve.
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
pub struct Rasterizer {
    font: &'static BitmapFont,
    theme: Theme,
}

impl Rasterizer {
    /// Build a rasterizer over the vendored face and `theme`.
    #[must_use]
    pub const fn new(theme: Theme) -> Self {
        Self {
            font: &SPLEEN_8X16,
            theme,
        }
    }

    /// The theme this rasterizer resolves colors against.
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Pixel size of one cell: `(width, height)`.
    #[must_use]
    pub const fn cell_size(&self) -> (u32, u32) {
        (self.font.cell_w, self.font.cell_h)
    }

    /// Allocate a background-filled surface sized for `cols` x `rows` cells.
    #[must_use]
    pub fn surface_for(&self, cols: u16, rows: u16) -> Surface {
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

    /// Paint `frame` onto `surface`.
    ///
    /// The surface may be larger than the frame (a recording that resized
    /// down is letterboxed against the theme background rather than
    /// reallocating mid-animation).
    ///
    /// Backgrounds are laid down for the whole grid *before* any glyph, in a
    /// separate pass. That ordering is load-bearing rather than tidy: a wide
    /// cluster paints into the following column, and phux-core's convention
    /// is that the trailing column carries the empty string with its own
    /// style — so painting cell-by-cell would let the tail's background erase
    /// the right half of every CJK glyph on the line.
    pub fn draw(&self, frame: &RenderedFrame, surface: &mut Surface) {
        let (cell_w, cell_h) = self.cell_size();
        // Letterbox: anything the grid does not cover is theme background, so
        // a frame smaller than the canvas is framed rather than showing the
        // previous frame's pixels.
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
                // A cluster whose next column is the empty string is the left
                // half of a double-width glyph: it gets twice the box, and the
                // 8-wide bitmap is centred in it rather than stretched. Getting
                // this wrong shifts every subsequent column on the line.
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
    /// This is the color histogram the GIF global table is built from, and it
    /// must stay in exact lockstep with [`Rasterizer::draw`] — the two share
    /// one resolution helper precisely so they cannot drift.
    pub fn colors_of(&self, frame: &RenderedFrame, out: &mut HashSet<[u8; 3]>) {
        for row in 0..frame.rows {
            for col in 0..frame.cols {
                let Some(cell) = frame.cell(row, col) else {
                    continue;
                };
                let paint = self.paint_of(&cell.style, cursor_at(frame, row, col));
                let glyph = self.glyph_of(&cell.grapheme);
                // Background survives unless the glyph covers the entire cell
                // — only U+2588 FULL BLOCK and an all-ones bitmap do that.
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
    /// Order is fixed and matters: base colors, then `inverse`, then `faint`,
    /// then `invisible`. A cursor cell is simply one more inversion, applied
    /// with `inverse` — expressing it that way (rather than as a post-pass
    /// that flips finished pixels) is what lets [`Rasterizer::colors_of`]
    /// answer exactly, since the cursor never introduces a color the normal
    /// resolution path did not already account for.
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

    /// `CellColor` to RGB. A palette index resolves through the theme table;
    /// truecolor passes through untouched.
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
            // The empty string is the right half of a wide cluster; the base
            // cell already painted through this column.
            return Glyph::Blank;
        };
        if ch == ' ' {
            return Glyph::Blank;
        }
        // The lookup order, which `boxdraw::covers_fallback` documents and
        // which nothing else may reorder: box/block glyphs beat the face
        // because they join their neighbours exactly at any cell size; the
        // face beats the fallback symbols because a real bitmap is always
        // truer than a hand-drawn approximation of one; only then tofu.
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
                // Centre rather than stretch: a doubled bitmap reads as a
                // rendering bug, an inset one reads as a narrow font, which is
                // the truth.
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
                // A hollow box, one pixel inset. Never a blank: a blank
                // silently corrupts visual column alignment, while a box tells
                // the reader exactly which cell had no coverage.
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

/// Whether a classified glyph paints any foreground pixels at all.
const fn emits_ink(glyph: Glyph) -> bool {
    match glyph {
        Glyph::Blank => false,
        // Every codepoint the procedural renderer covers paints something;
        // `boxdraw`'s `every_covered_codepoint_paints_at_least_one_pixel`
        // holds that invariant so this arm can stay a constant.
        Glyph::Boxed(_) | Glyph::Tofu => true,
        Glyph::Bitmap(bitmap) => {
            let mut i = 0;
            while i < bitmap.len() {
                if bitmap[i] != 0 {
                    return true;
                }
                i += 1;
            }
            false
        }
    }
}

/// Whether a classified glyph covers every pixel of its cell, leaving no
/// background visible.
const fn fills_cell(glyph: Glyph) -> bool {
    match glyph {
        Glyph::Boxed(ch) => matches!(ch, '\u{2588}'),
        Glyph::Bitmap(bitmap) => {
            let mut i = 0;
            while i < bitmap.len() {
                if bitmap[i] != 0xff {
                    return false;
                }
                i += 1;
            }
            true
        }
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

/// Underline, strikethrough, overline. Drawn after the glyph so a descender
/// does not sit on top of its own underline.
///
/// `blink` is deliberately ignored: an exported artifact has no phase, and a
/// GIF that alternated a blinking cell would double its frame count for a
/// detail no reader of a recording is waiting on. `italic` is ignored too —
/// the face has no oblique variant and a sheared bitmap at 8x16 is illegible.
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

/// Where one cell lands on the surface, in pixels.
///
/// `w` is the *painting* width, which is two cells for the base of a
/// double-width cluster; the background pass always uses one cell.
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
/// phux-core's convention: the base cell holds the whole cluster and the
/// trailing column holds the *empty string*, so the trailing column is the
/// signal. A blank base cell is never wide.
fn is_wide_base(frame: &RenderedFrame, row: u16, col: u16, grapheme: &str) -> bool {
    if grapheme.is_empty() || grapheme == " " {
        return false;
    }
    frame
        .cell(row, col.saturating_add(1))
        .is_some_and(|next| next.grapheme.is_empty())
}

/// Scale `fg` 60% of the way toward `bg`, in integer arithmetic.
///
/// This is the only colour blend in the whole pipeline, and it is bounded:
/// one extra colour per distinct (fg, bg) pair, not the 256 an antialiasing
/// rasterizer would produce per pair.
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
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;
    use phux_core::screen::CursorState;

    const FG: [u8; 3] = [0x11, 0x22, 0x33];
    const BG: [u8; 3] = [0x44, 0x55, 0x66];

    /// A frame of blank cells with the default style.
    fn frame(cols: u16, rows: u16) -> RenderedFrame {
        RenderedFrame::blank(cols, rows)
    }

    fn set(frame: &mut RenderedFrame, row: u16, col: u16, grapheme: &str, style: CellStyle) {
        let cell = frame.cell_mut(row, col).expect("cell in range");
        cell.grapheme = grapheme.to_owned();
        cell.style = style;
    }

    /// A style with explicit truecolor fg/bg, so assertions never depend on
    /// the theme defaults.
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

    fn render(frame: &RenderedFrame) -> (Rasterizer, Surface) {
        let raster = Rasterizer::new(Theme::default());
        let mut surface = raster.surface_for(frame.cols, frame.rows);
        raster.draw(frame, &mut surface);
        (raster, surface)
    }

    fn px(surface: &Surface, x: u32, y: u32) -> [u8; 3] {
        let idx = usize::try_from(y * surface.width + x).expect("index fits");
        *surface.pixels.get(idx).expect("pixel in range")
    }

    #[test]
    fn default_theme_palette_matches_xterm_256_at_indices_0_15_and_231() {
        let theme = Theme::default();
        assert_eq!(theme.palette[0], [0x00, 0x00, 0x00]);
        assert_eq!(theme.palette[15], [0xff, 0xff, 0xff]);
        assert_eq!(theme.palette[231], [0xff, 0xff, 0xff]);
        assert_eq!(theme.palette[16], [0x00, 0x00, 0x00]);
        assert_eq!(theme.palette[232], [0x08, 0x08, 0x08]);
        assert_eq!(theme.palette[255], [0xee, 0xee, 0xee]);
    }

    #[test]
    fn surface_for_sizes_cols_times_cell_w_by_rows_times_cell_h() {
        let raster = Rasterizer::new(Theme::default());
        let (cell_w, cell_h) = raster.cell_size();
        assert_eq!((cell_w, cell_h), (8, 16));
        let surface = raster.surface_for(80, 24);
        assert_eq!(surface.width, 80 * cell_w);
        assert_eq!(surface.height, 24 * cell_h);
        let expected = usize::try_from(surface.width * surface.height).expect("area fits in usize");
        assert_eq!(surface.pixels.len(), expected);
    }

    #[test]
    fn palette_index_resolves_through_the_theme() {
        // The client deliberately preserves palette *identity* rather than
        // flattening to RGB, so this lookup is the only place index 196
        // becomes pixels.
        let mut f = frame(1, 1);
        let style = CellStyle {
            bg: CellColor::Palette { index: 196 },
            ..CellStyle::default()
        };
        set(&mut f, 0, 0, " ", style);
        let (raster, surface) = render(&f);
        assert_eq!(px(&surface, 0, 0), raster.theme().palette[196]);
    }

    #[test]
    fn inverse_swaps_fg_and_bg() {
        let mut plain = frame(1, 1);
        set(&mut plain, 0, 0, "A", styled());
        let (_, normal) = render(&plain);

        let mut inverted = frame(1, 1);
        set(
            &mut inverted,
            0,
            0,
            "A",
            CellStyle {
                inverse: true,
                ..styled()
            },
        );
        let (_, flipped) = render(&inverted);

        for (a, b) in normal.pixels.iter().zip(flipped.pixels.iter()) {
            let expected = if *a == FG { BG } else { FG };
            assert_eq!(*b, expected, "inverse did not swap fg/bg");
        }
    }

    #[test]
    fn invisible_makes_glyph_pixels_equal_bg() {
        let mut f = frame(1, 1);
        set(
            &mut f,
            0,
            0,
            "A",
            CellStyle {
                invisible: true,
                ..styled()
            },
        );
        let (_, surface) = render(&f);
        assert!(
            surface.pixels.iter().all(|p| *p == BG),
            "an invisible cell still showed ink"
        );
    }

    #[test]
    fn faint_moves_fg_toward_bg() {
        let mut f = frame(1, 1);
        set(
            &mut f,
            0,
            0,
            "\u{2588}",
            CellStyle {
                faint: true,
                ..styled()
            },
        );
        let (_, surface) = render(&f);
        let dimmed = px(&surface, 0, 0);
        assert_ne!(dimmed, FG, "faint left the foreground untouched");
        assert_ne!(dimmed, BG, "faint collapsed the foreground into the bg");
        // 60% of the way from bg to fg, per channel, integer arithmetic.
        assert_eq!(dimmed, [0x25, 0x36, 0x47]);
    }

    #[test]
    fn underline_fills_the_bottom_pixel_row() {
        let mut f = frame(1, 1);
        set(
            &mut f,
            0,
            0,
            " ",
            CellStyle {
                underline: true,
                ..styled()
            },
        );
        let (raster, surface) = render(&f);
        let (cell_w, cell_h) = raster.cell_size();
        for x in 0..cell_w {
            assert_eq!(px(&surface, x, cell_h - 1), FG, "underline gap at x={x}");
            assert_eq!(px(&surface, x, cell_h - 2), BG, "underline is too thick");
        }
    }

    #[test]
    fn strikethrough_fills_the_middle_row() {
        let mut f = frame(1, 1);
        set(
            &mut f,
            0,
            0,
            " ",
            CellStyle {
                strikethrough: true,
                ..styled()
            },
        );
        let (raster, surface) = render(&f);
        let (cell_w, cell_h) = raster.cell_size();
        for x in 0..cell_w {
            assert_eq!(px(&surface, x, cell_h / 2), FG, "strike gap at x={x}");
        }
        assert_eq!(px(&surface, 0, 0), BG, "strike bled to the top row");
    }

    #[test]
    fn overline_fills_the_top_pixel_row() {
        let mut f = frame(1, 1);
        set(
            &mut f,
            0,
            0,
            " ",
            CellStyle {
                overline: true,
                ..styled()
            },
        );
        let (raster, surface) = render(&f);
        let (cell_w, _) = raster.cell_size();
        for x in 0..cell_w {
            assert_eq!(px(&surface, x, 0), FG, "overline gap at x={x}");
        }
    }

    #[test]
    fn wide_glyph_spans_two_cells_and_tail_keeps_its_own_bg() {
        // phux-core's convention: the base cell carries the whole cluster and
        // the trailing column carries the empty string with its own style.
        const TAIL_BG: [u8; 3] = [0x01, 0x02, 0x03];
        let mut f = frame(3, 1);
        set(&mut f, 0, 0, "\u{2588}", styled());
        set(
            &mut f,
            0,
            1,
            "",
            CellStyle {
                bg: CellColor::Rgb {
                    r: TAIL_BG[0],
                    g: TAIL_BG[1],
                    b: TAIL_BG[2],
                },
                ..CellStyle::default()
            },
        );
        let (raster, surface) = render(&f);
        let (cell_w, cell_h) = raster.cell_size();

        // The doubled block covers both columns...
        for x in 0..cell_w * 2 {
            assert_eq!(px(&surface, x, cell_h / 2), FG, "wide glyph gap at x={x}");
        }
        // ...and the column after the pair is untouched.
        assert_eq!(px(&surface, cell_w * 2, 0), Theme::default().bg);

        // A *narrow* glyph in the same position is centred, leaving the
        // tail's own background visible at the far edge.
        let mut narrow = frame(3, 1);
        set(&mut narrow, 0, 0, "A", styled());
        set(
            &mut narrow,
            0,
            1,
            "",
            CellStyle {
                bg: CellColor::Rgb {
                    r: TAIL_BG[0],
                    g: TAIL_BG[1],
                    b: TAIL_BG[2],
                },
                ..CellStyle::default()
            },
        );
        let (_, surface) = render(&narrow);
        assert_eq!(
            px(&surface, cell_w * 2 - 1, cell_h / 2),
            TAIL_BG,
            "the wide-glyph tail lost its own background"
        );
    }

    #[test]
    fn unmapped_codepoint_draws_a_hollow_box_not_a_blank() {
        let mut f = frame(1, 1);
        set(&mut f, 0, 0, "\u{6f22}", styled());
        let (raster, surface) = render(&f);
        let (cell_w, cell_h) = raster.cell_size();
        // A border, one pixel inset...
        assert_eq!(px(&surface, 1, 1), FG);
        assert_eq!(px(&surface, cell_w - 2, cell_h - 2), FG);
        // ...that is hollow, and does not touch the cell edge.
        assert_eq!(px(&surface, cell_w / 2, cell_h / 2), BG);
        assert_eq!(px(&surface, 0, 0), BG);
    }

    #[test]
    fn visible_cursor_inverts_exactly_one_cell_block() {
        let mut f = frame(2, 1);
        set(&mut f, 0, 0, "A", styled());
        set(&mut f, 0, 1, "B", styled());
        let (raster, plain) = render(&f);

        f.cursor = Some(CursorState {
            x: 0,
            y: 0,
            visible: true,
        });
        let (_, with_cursor) = render(&f);
        let (cell_w, cell_h) = raster.cell_size();

        for y in 0..cell_h {
            for x in 0..cell_w {
                let before = px(&plain, x, y);
                let after = px(&with_cursor, x, y);
                let expected = if before == FG { BG } else { FG };
                assert_eq!(after, expected, "cursor cell not inverted at ({x}, {y})");
            }
            for x in cell_w..cell_w * 2 {
                assert_eq!(
                    px(&plain, x, y),
                    px(&with_cursor, x, y),
                    "cursor leaked into the neighbouring cell at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn hidden_cursor_does_not_invert() {
        let mut f = frame(1, 1);
        set(&mut f, 0, 0, "A", styled());
        let (_, plain) = render(&f);
        f.cursor = Some(CursorState {
            x: 0,
            y: 0,
            visible: false,
        });
        let (_, hidden) = render(&f);
        assert_eq!(plain.pixels, hidden.pixels);
    }

    #[test]
    fn colors_of_returns_exactly_the_colors_draw_emits() {
        // The drift guard. Every branch `draw` can take is exercised here:
        // blank, bitmap, box glyph, tofu, wide base + tail, decorations,
        // faint, inverse, palette index, and the cursor.
        let mut f = frame(6, 2);
        set(&mut f, 0, 0, "A", styled());
        set(
            &mut f,
            0,
            1,
            "\u{2500}",
            CellStyle {
                fg: CellColor::Palette { index: 42 },
                ..CellStyle::default()
            },
        );
        set(
            &mut f,
            0,
            2,
            "\u{6f22}",
            CellStyle {
                underline: true,
                ..styled()
            },
        );
        set(&mut f, 0, 3, "", CellStyle::default());
        set(
            &mut f,
            0,
            4,
            "g",
            CellStyle {
                faint: true,
                ..styled()
            },
        );
        set(
            &mut f,
            0,
            5,
            "\u{2588}",
            CellStyle {
                inverse: true,
                ..styled()
            },
        );
        set(&mut f, 1, 0, " ", styled());
        set(
            &mut f,
            1,
            1,
            "\u{2593}",
            CellStyle {
                bg: CellColor::Palette { index: 17 },
                ..CellStyle::default()
            },
        );
        set(&mut f, 1, 2, "W", styled());
        set(&mut f, 1, 3, "", styled());
        f.cursor = Some(CursorState {
            x: 0,
            y: 1,
            visible: true,
        });

        let (raster, surface) = render(&f);
        let painted: HashSet<[u8; 3]> = surface.pixels.iter().copied().collect();
        let mut reported = HashSet::new();
        raster.colors_of(&f, &mut reported);
        assert_eq!(
            reported,
            painted,
            "colors_of drifted from draw: only in colors_of {:?}, only in draw {:?}",
            reported.difference(&painted).collect::<Vec<_>>(),
            painted.difference(&reported).collect::<Vec<_>>()
        );
    }

    #[test]
    fn draw_letterboxes_a_frame_smaller_than_the_surface() {
        let raster = Rasterizer::new(Theme::default());
        let mut surface = raster.surface_for(4, 2);
        let mut f = frame(2, 1);
        set(&mut f, 0, 0, "\u{2588}", styled());
        raster.draw(&f, &mut surface);
        let (cell_w, cell_h) = raster.cell_size();
        assert_eq!(px(&surface, 0, 0), FG);
        assert_eq!(
            px(&surface, cell_w * 3, cell_h),
            Theme::default().bg,
            "uncovered canvas is not theme background"
        );
    }

    #[test]
    fn bold_selects_a_heavier_bitmap_without_changing_color() {
        let mut plain = frame(1, 1);
        set(&mut plain, 0, 0, "l", styled());
        let (_, thin) = render(&plain);

        let mut bold = frame(1, 1);
        set(
            &mut bold,
            0,
            0,
            "l",
            CellStyle {
                bold: true,
                ..styled()
            },
        );
        let (_, thick) = render(&bold);

        let ink = |s: &Surface| s.pixels.iter().filter(|p| **p == FG).count();
        assert!(ink(&thick) > ink(&thin), "bold did not widen the glyph");
        // libghostty deliberately does not apply bold *color* handling, so an
        // export that brightened here would disagree with phux's own glass.
        assert!(
            thick.pixels.iter().all(|p| *p == FG || *p == BG),
            "bold introduced a third color"
        );
    }

    #[test]
    fn the_fallback_tier_never_steals_a_codepoint_the_face_can_draw() {
        // The whole point of the ordering in `glyph_of`. U+25B2 and U+2192
        // sit right beside members of the fallback set and have real bitmaps;
        // if the tier were an override rather than a fallback they would
        // start rendering as hand-drawn approximations.
        let raster = Rasterizer::new(Theme::default());
        for ch in ['A', '\u{2192}', '\u{25b2}', '\u{25cf}', '\u{e0b0}'] {
            assert!(
                matches!(raster.glyph_of(&ch.to_string()), Glyph::Bitmap(_)),
                "U+{:04X} did not come from the face",
                ch as u32
            );
        }
        // And the two procedural tiers still answer for what they own.
        for ch in ['\u{2500}', '\u{2588}', '\u{276f}', '\u{2713}', '\u{26a0}'] {
            assert!(
                matches!(raster.glyph_of(&ch.to_string()), Glyph::Boxed(_)),
                "U+{:04X} fell through to the face or to tofu",
                ch as u32
            );
        }
        assert!(matches!(raster.glyph_of("\u{6f22}"), Glyph::Tofu));
    }
}
