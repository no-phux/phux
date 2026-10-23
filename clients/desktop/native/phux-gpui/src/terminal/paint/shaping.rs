//! Collect cell text into runs, shape glyphs, and record fixture observations.

use super::model::TextStyle;
use super::*;

impl Builder {
    pub(super) fn prepare_ink(
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
        for &region in regions {
            self.scene.decorations.push(RowQuad {
                row,
                bounds: block_bounds(bounds, region, self.scene.geometry.scale),
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

    pub(super) fn flush_glyph_run(&mut self, window: &gpui::Window) {
        if let Some(run) = self.pending.take() {
            self.shape_run(run, window);
        }
    }

    /// Shape `run` as one line, one cell-font glyph per cell. A run that does
    /// not shape that way (a ligature survived, or a glyph fell back to
    /// another font or to emoji) is split: its ASCII stretches are shaped as
    /// runs again and every other cell alone.
    fn shape_run(&mut self, mut run: PendingRun, window: &gpui::Window) {
        if run.text.is_empty() {
            return;
        }
        let geometry = self.scene.geometry;
        let text = SharedString::from(std::mem::take(&mut run.text));
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
        let font_size = px(self.scene.settings.font_size);
        let line = self
            .fonts
            .ascii_line(
                &text,
                &run.spans,
                font_size,
                geometry.cell_width,
                self.scene.frame.cols,
                window,
            )
            .unwrap_or_else(|| {
                crate::perf::PLATFORM_SHAPED.incr();
                window.text_system().shape_line(
                    text.clone(),
                    font_size,
                    &runs,
                    Some(geometry.cell_width),
                )
            });
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
        self.shape_fallback(run, &text, window);
    }

    fn shape_fallback(&mut self, run: PendingRun, text: &str, window: &gpui::Window) {
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
            self.flush_ascii_part(&mut part, window);
            self.shape_cell((run.row, col, 1), ch.to_string(), style, false, window);
        }
        self.flush_ascii_part(&mut part, window);
    }

    fn flush_ascii_part(&mut self, part: &mut Option<PendingRun>, window: &gpui::Window) {
        if let Some(part) = part.take() {
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
        crate::perf::PLATFORM_SHAPED.incr();
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
}

/// True when each character of `text` became one non-emoji glyph from one
/// of `fonts`, in order.
pub(super) fn glyphs_match_cells(line: &ShapedLine, text: &str, fonts: &[gpui::FontId]) -> bool {
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
