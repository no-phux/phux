//! Reuse a bounded printable-ASCII glyph table instead of reshaping random rows.

use super::model::Span;
use super::*;

const FIRST: u8 = b' ';
const LAST: u8 = b'~';

impl Fonts {
    /// Terminal runs disable contextual alternates and ligatures, and force
    /// each glyph onto its cell. For printable ASCII from the requested face,
    /// only glyph IDs and vertical offsets survive shaping; colours are separate.
    /// A missing/fallback glyph rejects the table and keeps the platform path.
    pub(super) fn ascii_line(
        &self,
        text: &SharedString,
        spans: &[Span],
        font_size: Pixels,
        cell_width: Pixels,
        cols: u16,
        window: &gpui::Window,
    ) -> Option<ShapedLine> {
        if !text.bytes().all(|byte| (FIRST..=LAST).contains(&byte)) {
            return None;
        }
        let mut layout = gpui::LineLayout {
            font_size,
            len: text.len(),
            width: cell_width * text.len() as f32,
            ..Default::default()
        };
        let mut offset = 0;
        for span in spans {
            let table = self.ascii[span.style.face]
                .get_or_init(|| {
                    self.ascii_table(span.style.face, font_size, cell_width, cols, window)
                })
                .as_ref()?;
            layout.ascent = layout.ascent.max(table.ascent);
            layout.descent = layout.descent.max(table.descent);
            let source = &table.runs[0];
            let glyphs = text.as_bytes()[offset..offset + span.len]
                .iter()
                .enumerate()
                .map(|(index, byte)| {
                    let index = offset + index;
                    let mut glyph = source.glyphs[usize::from(byte - FIRST)].clone();
                    // GPUI preserves natural offsets within one pixel of the
                    // grid. Reuse its shaped ruler, not index * cell_width.
                    glyph.position.x = source.glyphs[index].position.x;
                    glyph.index = index;
                    glyph
                })
                .collect();
            layout.runs.push(gpui::ShapedRun {
                font_id: source.font_id,
                glyphs,
            });
            offset += span.len;
        }
        let mut line = ShapedLine::default();
        *line = Arc::new(layout);
        line.text = text.clone();
        crate::perf::ASCII_LINES.incr();
        Some(line)
    }

    fn ascii_table(
        &self,
        face: usize,
        font_size: Pixels,
        cell_width: Pixels,
        cols: u16,
        window: &gpui::Window,
    ) -> Option<ShapedLine> {
        // Only the bundled, audited fixed-pitch faces have context-independent
        // ASCII IDs and equal advances across styles. User fonts keep native
        // shaping (including any contextual substitutions or kerning).
        if !matches!(
            self.run[face].family.as_ref(),
            crate::fonts::BUNDLED | crate::fonts::SYMBOLS
        ) {
            return None;
        }
        let alphabet: String = (FIRST..=LAST).map(char::from).collect();
        let text = alphabet.repeat(
            usize::from(cols)
                .max(alphabet.len())
                .div_ceil(alphabet.len()),
        );
        crate::perf::PLATFORM_SHAPED.incr();
        let line = window.text_system().shape_line(
            text.clone().into(),
            font_size,
            &[gpui::TextRun {
                len: text.len(),
                font: self.run[face].clone(),
                color: gpui::black(),
                ..Default::default()
            }],
            Some(cell_width),
        );
        (line.runs.len() == 1
            && super::shaping::glyphs_match_cells(&line, &text, &[self.run_ids(window)[face]]))
        .then_some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::TextStyle;
    use super::*;

    #[gpui::test]
    fn unsupported_fonts_and_non_ascii_keep_platform_shaping(cx: &mut gpui::TestAppContext) {
        let window = cx.add_window(|_, _| gpui::Empty);
        window
            .update(cx, |_, window, _| {
                let fonts = Fonts::new(&gpui::font("user-provided-font"));
                let spans = [Span {
                    len: 1,
                    style: TextStyle {
                        face: 0,
                        color: gpui::black(),
                        opacity: 1.,
                    },
                }];
                for text in ["A", "é", "界", "e\u{301}", "\n", "\u{7f}", "🙂"] {
                    assert!(
                        fonts
                            .ascii_line(&text.into(), &spans, px(16.), px(9.), 80, window)
                            .is_none()
                    );
                }
            })
            .unwrap();
    }
}
