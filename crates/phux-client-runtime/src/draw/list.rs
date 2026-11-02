//! A whole frame as rectangles and text: [`DrawList::build`].

use std::ops::Range;

use phux_client_core::grid::{CELL_OVERLINE, CELL_STRIKETHROUGH};

use super::{
    BlockRegion, CellColors, CursorPaint, DecorationKind, Face, Theme, Underline, Visibility,
    block_regions, cell_colors, cursor, default_colors, hidden,
};
use crate::publication::{Cell, GridFrame};

/// Cells `start..end` of `row` in one colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackgroundRun<C> {
    /// Row.
    pub row: u16,
    /// First column.
    pub start: u16,
    /// Column after the last.
    pub end: u16,
    /// Fill.
    pub color: C,
}

/// Text drawn from `col` on `row` across `cells` columns.
///
/// A run of several cells holds one character per column, each placed at
/// its own column, not at the font's advance; blanks between glyphs of one
/// style are spaces in the run. A wide character or a grapheme of several
/// characters is a run of its own, marked [`Self::lone`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextRun<C> {
    /// Row.
    pub row: u16,
    /// First column.
    pub col: u16,
    /// Columns covered.
    pub cells: u16,
    /// Byte range of the text in [`DrawList::text`].
    pub text: Range<usize>,
    /// Font face.
    pub face: Face,
    /// Glyph colour.
    pub color: C,
    /// Half alpha.
    pub faint: bool,
    /// One glyph across all of `cells`, not one character per column.
    pub lone: bool,
}

/// A block element drawn as filled regions of its cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockCell<C> {
    /// Row.
    pub row: u16,
    /// Column.
    pub col: u16,
    /// The parts of the cell to fill.
    pub regions: &'static [BlockRegion],
    /// Fill.
    pub color: C,
    /// Half alpha.
    pub faint: bool,
}

/// A decoration across cells `start..end` of `row`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decoration<C> {
    /// Row.
    pub row: u16,
    /// First column.
    pub start: u16,
    /// Column after the last.
    pub end: u16,
    /// Which line.
    pub kind: DecorationKind,
    /// Colour.
    pub color: C,
    /// Half alpha.
    pub faint: bool,
}

/// One frame as rectangles and text in cell coordinates. See the module
/// documentation for the paint order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawList<C> {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
    /// Fill the whole grid with this first.
    pub background: C,
    /// Cells whose background is not [`Self::background`].
    pub backgrounds: Vec<BackgroundRun<C>>,
    /// Glyph runs.
    pub texts: Vec<TextRun<C>>,
    /// Block elements.
    pub blocks: Vec<BlockCell<C>>,
    /// Underlines, strikethrough and overlines, merged across adjacent cells.
    pub decorations: Vec<Decoration<C>>,
    /// The cursor, if it shows.
    pub cursor: Option<CursorPaint<C>>,
    /// The text every [`TextRun`] slices.
    pub text: String,
}

impl<C: Copy + PartialEq> DrawList<C> {
    /// Apply the paint rules to every cell of `frame`'s viewport.
    #[must_use]
    pub fn build(frame: &GridFrame, theme: &Theme<C>, visibility: Visibility) -> Self {
        let defaults = default_colors(frame, theme);
        let cursor = cursor(frame, theme, visibility);
        let mut builder = Builder {
            list: Self {
                cols: frame.cols,
                rows: frame.rows,
                background: defaults.1,
                backgrounds: Vec::new(),
                texts: Vec::new(),
                blocks: Vec::new(),
                decorations: Vec::new(),
                cursor,
                text: String::new(),
            },
            solid: cursor
                .filter(|cursor| cursor.solid)
                .map(|cursor| (cursor.row, cursor.col)),
            extendable: false,
            blanks: 0,
        };
        for row in 0..frame.rows {
            for col in 0..frame.cols {
                builder.cell(frame, theme, visibility, defaults, (row, col));
            }
        }
        builder.list
    }
}

struct Builder<C> {
    list: DrawList<C>,
    /// The head of a solid cursor, whose glyph takes the cell background.
    solid: Option<(u16, u16)>,
    /// The last text run holds one single-width character per cell, so the
    /// next such cell may extend it.
    extendable: bool,
    /// Blanks since the last text run's end, spaces in it if a glyph follows.
    blanks: u16,
}

impl<C: Copy + PartialEq> Builder<C> {
    fn cell(
        &mut self,
        frame: &GridFrame,
        theme: &Theme<C>,
        visibility: Visibility,
        defaults: (C, C),
        (row, col): (u16, u16),
    ) {
        let Some(cell) = frame.cell(row, col) else {
            return;
        };
        let index = usize::from(row) * usize::from(frame.cols) + usize::from(col);
        let metadata = frame
            .buffer
            .metadata
            .get(index)
            .copied()
            .unwrap_or_default();
        let colors = cell_colors(cell, metadata, frame, theme, defaults);
        self.background(row, col, colors.background);
        if hidden(cell, visibility) {
            return;
        }
        self.ink(frame, cell, (row, col), colors);
        self.decorations(cell, (row, col), colors);
    }

    fn background(&mut self, row: u16, col: u16, color: C) {
        if let Some(run) = self.list.backgrounds.last_mut()
            && run.row == row
            && run.end == col
            && run.color == color
        {
            run.end += 1;
            return;
        }
        if color != self.list.background {
            self.list.backgrounds.push(BackgroundRun {
                row,
                start: col,
                end: col + 1,
                color,
            });
        }
    }

    fn ink(
        &mut self,
        frame: &GridFrame,
        cell: &Cell,
        (row, col): (u16, u16),
        colors: CellColors<C>,
    ) {
        // Spacers of a wide character hold no glyph; the head draws it.
        if matches!(cell.wide, 2 | 3) {
            return;
        }
        let text = frame.cell_text(row, col);
        if text.is_empty() || text == b" " {
            self.blanks = self.blanks.saturating_add(1);
            return;
        }
        let color = if self.solid == Some((row, col)) {
            colors.background
        } else {
            colors.foreground
        };
        if cell.wide == 0
            && let Some(regions) = block_regions(text)
        {
            self.list.blocks.push(BlockCell {
                row,
                col,
                regions,
                color,
                faint: colors.faint,
            });
            return;
        }
        let Ok(text) = std::str::from_utf8(text) else {
            return;
        };
        let face = Face::of(cell);
        let single = cell.wide == 0 && text.chars().nth(1).is_none();
        if single
            && self.extendable
            && let Some(run) = self.list.texts.last_mut()
            && run.row == row
            && run.col + run.cells + self.blanks == col
            && run.face == face
            && run.color == color
            && run.faint == colors.faint
        {
            let blanks = usize::from(self.blanks);
            self.list.text.extend(std::iter::repeat_n(' ', blanks));
            self.list.text.push_str(text);
            run.text.end = self.list.text.len();
            run.cells += self.blanks + 1;
            self.blanks = 0;
            return;
        }
        self.extendable = single;
        self.blanks = 0;
        let start = self.list.text.len();
        self.list.text.push_str(text);
        self.list.texts.push(TextRun {
            row,
            col,
            cells: if cell.wide == 1 { 2 } else { 1 },
            text: start..self.list.text.len(),
            face,
            color,
            faint: colors.faint,
            lone: !single,
        });
    }

    fn decorations(&mut self, cell: &Cell, (row, col): (u16, u16), colors: CellColors<C>) {
        if let Some(underline) = Underline::of(cell) {
            self.decoration(
                row,
                col,
                DecorationKind::Underline(underline),
                colors.underline,
                colors.faint,
            );
        }
        if cell.flags & CELL_STRIKETHROUGH != 0 {
            self.decoration(
                row,
                col,
                DecorationKind::Strikethrough,
                colors.foreground,
                colors.faint,
            );
        }
        if cell.flags & CELL_OVERLINE != 0 {
            self.decoration(
                row,
                col,
                DecorationKind::Overline,
                colors.foreground,
                colors.faint,
            );
        }
    }

    /// Extend the same line from the previous cell, or start one.
    fn decoration(&mut self, row: u16, col: u16, kind: DecorationKind, color: C, faint: bool) {
        if let Some(line) = self
            .list
            .decorations
            .iter_mut()
            .rev()
            // The previous cell's lines end at `col`; this cell's own, pushed
            // just before, end at `col + 1` and must not stop the search.
            .take_while(|line| line.row == row && (line.end == col || line.end == col + 1))
            .find(|line| line.kind == kind && line.end == col)
            && line.color == color
            && line.faint == faint
        {
            line.end += 1;
            return;
        }
        self.list.decorations.push(Decoration {
            row,
            start: col,
            end: col + 1,
            kind,
            color,
            faint,
        });
    }
}
