//! One row's text runs as elements. ASCII stays one shaped string on the
//! cell grid; everything else gets its column back: a line, shade, braille or
//! Powerline character is drawn from the cell's edges, and any other glyph is
//! shaped alone, set smaller when a fallback face's glyph is wider than its
//! cells, and clipped to them, so it can't shift the text after it or spill
//! onto its neighbours.

use phux_client_runtime::draw::TextRun;
use phux_client_runtime::draw::boxes::{self, Bend, Piece, Rect};
use rune::text::advance;
use rune::{Color, El, TextStyle, el, text};

use super::{Metrics, faint, missing};
use crate::perf;

/// How far back a piece looks for one to join: a double line alternates
/// between its two bars, and a cell may hold four arms.
const JOIN_REACH: usize = 6;

/// A filled rectangle in device pixels, within the row.
struct Quad {
    rect: Rect,
    color: Color,
}

/// What one row's text comes to.
pub(super) struct Marks<'m> {
    metrics: &'m Metrics,
    els: Vec<El>,
    quads: Vec<Quad>,
}

impl<'m> Marks<'m> {
    pub(super) const fn new(metrics: &'m Metrics) -> Self {
        Self {
            metrics,
            els: Vec::new(),
            quads: Vec::new(),
        }
    }

    pub(super) fn into_els(self) -> Vec<El> {
        let scale = self.metrics.scale;
        let mut els = self.els;
        els.extend(self.quads.into_iter().map(|quad| {
            let (x, y, width, height) = points(quad.rect, scale);
            el().absolute(x, y).size(width, height).fill(quad.color)
        }));
        els
    }

    /// Add one run, whose text is `content`.
    pub(super) fn run(&mut self, content: &str, run: &TextRun<Color>) {
        let style = self.metrics.style(run.face.bold, run.face.italic);
        let color = faint(run.color, run.faint);
        if content.is_ascii() {
            self.ascii(content, run.col, style, color);
            return;
        }
        if run.lone {
            if content.starts_with(super::images::PLACEHOLDER) {
                // The image layer draws this cell.
                return;
            }
            if content.chars().any(missing::nothing_draws) {
                self.empty_box(run.col, run.cells, color);
            } else {
                self.glyph(content, run.col, run.cells, style, color);
            }
            return;
        }
        let mut plain: Option<(usize, u16)> = None;
        for (col, (at, ch)) in (run.col..).zip(content.char_indices()) {
            if ch.is_ascii() {
                plain.get_or_insert((at, col));
                continue;
            }
            if let Some((from, first)) = plain.take() {
                self.ascii(&content[from..at], first, style, color);
            }
            if ch == super::images::PLACEHOLDER {
                continue;
            }
            if missing::nothing_draws(ch) {
                self.empty_box(col, 1, color);
            } else if !self.drawn(ch, col, color) {
                let end = at + ch.len_utf8();
                self.glyph(&content[at..end], col, 1, style, color);
            }
        }
        if let Some((from, first)) = plain {
            self.ascii(&content[from..], first, style, color);
        }
    }

    /// Characters that each fill one cell, without the blanks at their ends.
    fn ascii(&mut self, content: &str, col: u16, style: TextStyle, color: Color) {
        let trimmed = content.trim_end();
        let glyphs = trimmed.trim_start();
        if glyphs.is_empty() {
            return;
        }
        let col = col + (trimmed.len() - glyphs.len()) as u16;
        let x = f32::from(col) * self.metrics.cell_w;
        perf::SHAPED.incr();
        self.els.push(
            text(glyphs)
                .absolute(x, 0.0)
                .font(style)
                .color(color)
                .single_line(),
        );
    }

    /// A glyph alone, fitted to and cut at the edges of its cells.
    fn glyph(&mut self, content: &str, col: u16, cells: u16, style: TextStyle, color: Color) {
        let m = self.metrics;
        let room = f32::from(cells) * m.cell_w;
        let style = fitted(content, style, room);
        perf::SHAPED.incr();
        self.els.push(
            el().absolute(f32::from(col) * m.cell_w, 0.0)
                .size(room, m.cell_h)
                .clip_rounded(0.0)
                .child(text(content).font(style).color(color).single_line()),
        );
    }

    /// What a character no face has looks like: an empty box in its cells,
    /// drawn here so it is never shaped (`missing`).
    fn empty_box(&mut self, col: u16, cells: u16, color: Color) {
        let m = self.metrics;
        let device = |points: f32| (points * m.scale).round() as i32;
        let (left, right) = (
            device(f32::from(col) * m.cell_w),
            device(f32::from(col + cells) * m.cell_w),
        );
        let inset = ((right - left) / 6).max(1);
        let (left, right) = (left + inset, right - inset);
        let (top, bottom) = (device(m.cell_h * 0.2), device(m.cell_h * 0.8));
        let line = device(1.0).max(1);
        for rect in [
            (left, top, right, top + line),
            (left, bottom - line, right, bottom),
            (left, top, left + line, bottom),
            (right - line, top, right, bottom),
        ] {
            self.quads.push(Quad { rect, color });
        }
    }

    /// Draw `ch` from its cell's edges, when it is drawn rather than set.
    fn drawn(&mut self, ch: char, col: u16, color: Color) -> bool {
        let m = self.metrics;
        let device = |points: f32| (points * m.scale).round() as i32;
        let cell = boxes::Cell {
            left: device(f32::from(col) * m.cell_w),
            top: 0,
            right: device(f32::from(col + 1) * m.cell_w),
            bottom: device(m.cell_h),
        };
        let Some(pieces) = boxes::pieces(ch, cell) else {
            return false;
        };
        for piece in pieces {
            self.piece(piece, color);
        }
        true
    }

    fn piece(&mut self, piece: Piece, color: Color) {
        if let Some(line) = piece.bend {
            self.els
                .push(bent(piece.rect, line, self.metrics.scale, color));
            return;
        }
        let color = color.alpha(color.a * piece.coverage);
        let rect = piece.rect;
        let joined = self.quads.iter_mut().rev().take(JOIN_REACH).find(|quad| {
            let (_, top, right, bottom) = quad.rect;
            quad.color == color && right == rect.0 && top == rect.1 && bottom == rect.3
        });
        match joined {
            Some(quad) => quad.rect.2 = rect.2,
            None => self.quads.push(Quad { rect, color }),
        }
    }
}

/// `style` for `content` set alone in `room` points: smaller when its glyph,
/// usually a fallback face's (⌘ from a font without it), is wider, as other
/// terminals scale it, so it shows whole instead of cut off. The tracking
/// that makes the chosen face's advance a cell only pads after a lone glyph.
fn fitted(content: &str, style: TextStyle, room: f32) -> TextStyle {
    let wide = advance(content, style);
    // A hundredth of a point: the chosen face's own glyphs measure the cell.
    if wide <= room + 0.01 {
        return style;
    }
    TextStyle {
        size: style.size * (room - style.tracking) / (wide - style.tracking),
        ..style
    }
}

/// `(x, y, width, height)` in points.
fn points((left, top, right, bottom): Rect, scale: f32) -> (f32, f32, f32, f32) {
    let point = |device: i32| device as f32 / scale;
    (
        point(left),
        point(top),
        point(right - left),
        point(bottom - top),
    )
}

/// A rounded corner. A ring runs past the two open sides and the piece's
/// rectangle cuts it off there. `El::border_sides` could draw just the two
/// sides, but Rune caps a radius at half the box's shorter edge, so on the
/// piece's own box the arc comes out tighter; the larger ring keeps it.
fn bent(rect: Rect, line: Bend, scale: f32, color: Color) -> El {
    let (x, y, width, height) = points(rect, scale);
    let thickness = line.thickness as f32 / scale;
    let radius = line.radius as f32 / scale;
    let past = 2.0 * thickness;
    let mut corners = [0.0; 4];
    corners[usize::from(line.corner)] = radius;
    let left = if matches!(line.corner, 1 | 2) {
        -past
    } else {
        0.0
    };
    let top = if line.corner >= 2 { -past } else { 0.0 };
    el().absolute(x, y)
        .size(width, height)
        .clip_rounded(0.0)
        .child(
            el().absolute(left, top)
                .size(width + past, height + past)
                .corners(corners)
                .border(thickness, color),
        )
}

#[cfg(test)]
mod bend_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client_runtime::draw::Face;

    fn run(content: &str, lone: bool) -> TextRun<Color> {
        TextRun {
            row: 0,
            col: 2,
            cells: content.chars().count() as u16,
            text: 0..content.len(),
            face: Face::default(),
            color: Color::WHITE,
            faint: false,
            lone,
        }
    }

    fn marks<'m>(metrics: &'m Metrics, content: &str, lone: bool) -> Marks<'m> {
        let mut marks = Marks::new(metrics);
        marks.run(content, &run(content, lone));
        marks
    }

    /// A Kitty Unicode placeholder cell is the image's, never a glyph.
    #[test]
    fn placeholder_cells_draw_no_text() {
        let m = Metrics::new(13.0, 2.0);
        assert!(marks(&m, "\u{10EEEE}\u{305}\u{305}", true).els.is_empty());
        assert_eq!(marks(&m, "ab\u{10EEEE}\u{10EEEE}cd", false).els.len(), 2);
    }

    #[test]
    fn ascii_stays_one_string_and_others_get_their_own_cells() {
        let m = Metrics::new(13.0, 2.0);
        assert_eq!(marks(&m, "plain text here", false).els.len(), 1);
        assert_eq!(marks(&m, "ab→cd→", false).els.len(), 4);
        assert_eq!(marks(&m, "  ", false).els.len(), 0);
        assert_eq!(marks(&m, "é✓", true).els.len(), 1);
    }

    /// A character no face has is boxed, not shaped, and the symbol after it
    /// still shapes. The grid owns the missing character's box rather than
    /// relying on whichever face the text system uses for `.notdef`.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_character_no_face_has_is_boxed_and_never_shaped() {
        let m = Metrics::new(13.0, 2.0);
        let row = marks(&m, "\u{378}⏵", false);
        assert_eq!(row.quads.len(), 4, "a box of four sides");
        assert_eq!(row.els.len(), 1, "only ⏵ is shaped");
        let lone = marks(&m, "\u{FDD0}", true);
        assert_eq!((lone.quads.len(), lone.els.len()), (4, 0));
    }

    #[test]
    fn a_line_across_cells_is_one_rectangle() {
        let m = Metrics::new(13.0, 2.0);
        let single = marks(&m, "────", false);
        assert!(single.els.is_empty());
        assert_eq!(single.quads.len(), 1);
        let (left, _, right, _) = single.quads[0].rect;
        let cell = (m.cell_w * m.scale).round() as i32;
        assert_eq!((left, right), (2 * cell, 6 * cell));
        assert_eq!(marks(&m, "════", false).quads.len(), 2);
    }

    /// Symbols (including ⏵, which coding agents print in their mode line)
    /// draw real glyphs in every offered family, not `.notdef` or LastResort.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn symbols_the_chosen_font_lacks_fall_back_to_a_system_face() {
        use super::super::{FAMILIES, FONT_CHOICES, FONTS, mono_family};
        use skrifa::MetadataProvider;

        let _fonts = rune::text::with_families(FAMILIES.0, FAMILIES.1);
        let style = Metrics::new(13.0, 2.0).style(false, false);
        for family in FONT_CHOICES {
            for uncovered_first in [false, true] {
                let mut ts = rune::text::TextSystem::new(FONTS, (FAMILIES.0, &mono_family(family)));
                // Each glyph and its actual face, shaped alone as `Marks::glyph` does.
                let mut shaped = |content: &str| {
                    let layout = ts.layout_rich(content, &[], style, 2.0, None);
                    let line = layout.lines().next().expect("one line");
                    let run = line.runs().next().expect("one run");
                    let glyph = run
                        .clusters()
                        .flat_map(|c| c.glyphs())
                        .next()
                        .expect("one glyph");
                    (glyph.id, run.font().clone())
                };
                if uncovered_first {
                    assert_eq!(shaped("\u{378}").0, 0, "unassigned is .notdef");
                }
                // Misc Technical, Dingbats, Geometric Shapes.
                let symbols = ["⏵", "⏴", "⏎", "✔", "❯", "▶", "◆"].map(|s| (s, shaped(s)));
                // Rune no longer loads LastResort. A valid face can supply `.notdef`
                // here and a real symbol above, so its identity is not a tofu oracle.
                assert_eq!(shaped("\u{378}").0, 0, "unassigned stays .notdef");
                for (symbol, (id, font)) in symbols {
                    let face = skrifa::FontRef::from_index(font.data.as_ref(), font.index)
                        .expect("the shaped face");
                    let map = face.charmap();
                    let ch = symbol.chars().next().expect("one character");
                    assert_ne!(id, 0, "{symbol} in {family:?} is .notdef");
                    assert_eq!(
                        map.map(ch).map(|glyph| glyph.to_u32()),
                        Some(id),
                        "{symbol} in {family:?} must be its face's real glyph"
                    );
                    assert!(
                        map.map('\u{378}').is_none(),
                        "{symbol} in {family:?} must not use a last-resort face"
                    );
                }
            }
        }
    }

    /// A symbol from a face wider than the cell is set smaller and shows
    /// whole, in every family Settings offers; one that fits is untouched.
    #[test]
    fn a_glyph_wider_than_its_cells_is_set_smaller_to_fit() {
        use super::super::{FAMILIES, FONT_CHOICES, mono_family};
        for family in FONT_CHOICES {
            let _fonts = rune::text::with_families(FAMILIES.0, &mono_family(family));
            let m = Metrics::new(13.0, 2.0);
            let style = m.style(false, false);
            for symbol in ["⌘", "⌥", "⇧", "⌃", "M"] {
                for room in [m.cell_w, m.cell_w / 2.0] {
                    let fit = fitted(symbol, style, room);
                    let wide = advance(symbol, fit);
                    assert!(
                        wide <= room + 0.01,
                        "{symbol} in {family:?}: {wide} > {room}"
                    );
                }
            }
            assert_eq!(fitted("é", style, m.cell_w).size, style.size);
        }
    }

    #[test]
    fn a_rounded_corner_is_not_a_fill() {
        let m = Metrics::new(13.0, 2.0);
        let corner = marks(&m, "╭", false);
        assert_eq!((corner.els.len(), corner.quads.len()), (1, 0));
    }
}
