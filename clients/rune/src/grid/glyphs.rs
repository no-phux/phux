//! One row's text runs as elements. ASCII stays one shaped string on the
//! cell grid; everything else gets its column back: a line, shade, braille or
//! Powerline character is drawn from the cell's edges, and any other glyph is
//! shaped alone and clipped to its cells, so a fallback face's advance can't
//! shift the text after it or spill onto its neighbours.

use phux_client_runtime::draw::TextRun;
use phux_client_runtime::draw::boxes::{self, Bend, Piece, Rect};
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

    /// A glyph alone, cut at the edges of its cells.
    fn glyph(&mut self, content: &str, col: u16, cells: u16, style: TextStyle, color: Color) {
        let m = self.metrics;
        perf::SHAPED.incr();
        self.els.push(
            el().absolute(f32::from(col) * m.cell_w, 0.0)
                .size(f32::from(cells) * m.cell_w, m.cell_h)
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

/// A rounded corner. Rune borders all four sides, so the ring runs past the
/// two open sides and the piece's rectangle cuts it off there.
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

    #[test]
    fn ascii_stays_one_string_and_others_get_their_own_cells() {
        let m = Metrics::new(13.0, 2.0);
        assert_eq!(marks(&m, "plain text here", false).els.len(), 1);
        assert_eq!(marks(&m, "ab→cd→", false).els.len(), 4);
        assert_eq!(marks(&m, "  ", false).els.len(), 0);
        assert_eq!(marks(&m, "é✓", true).els.len(), 1);
    }

    /// A character no face has is boxed, not shaped, and the symbol after it
    /// still shapes: shaping it would put the last-resort font ahead of every
    /// later symbol's face.
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

    /// A symbol every family Settings offers lacks (⏵, which coding agents print in their mode
    /// line) draws from a system face, not as `.notdef` or the last-resort box.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn symbols_the_chosen_font_lacks_fall_back_to_a_system_face() {
        use super::super::{FAMILIES, FONT_CHOICES, FONTS, mono_family};
        let style = Metrics::new(13.0, 2.0).style(false, false);
        for family in FONT_CHOICES {
            let mut ts = rune::text::TextSystem::new(FONTS, (FAMILIES.0, &mono_family(family)));
            // Each glyph id and the font it came from, shaped alone as `Marks::glyph` does.
            let mut shaped = |content: &str| {
                let layout = ts.layout_rich(content, &[], style, 2.0, None);
                let line = layout.lines().next().expect("one line");
                let run = line.runs().next().expect("one run");
                let font = run.font().data.id();
                let glyph = run
                    .clusters()
                    .flat_map(|c| c.glyphs())
                    .next()
                    .expect("one glyph");
                (glyph.id, font)
            };
            // Misc Technical, Dingbats, Geometric Shapes.
            let symbols = ["⏵", "⏴", "⏎", "✔", "❯", "▶", "◆"].map(|s| (s, shaped(s)));
            // Unassigned, so only the last-resort face has it; asked last, so it can't get ahead
            // of the faces above.
            let last_resort = shaped("\u{378}").1;
            for (symbol, (id, font)) in symbols {
                assert!(
                    id != 0 && font != last_resort,
                    "{symbol} in {family:?} is tofu"
                );
            }
        }
    }

    #[test]
    fn a_rounded_corner_is_not_a_fill() {
        let m = Metrics::new(13.0, 2.0);
        let corner = marks(&m, "╭", false);
        assert_eq!((corner.els.len(), corner.quads.len()), (1, 0));
    }
}
