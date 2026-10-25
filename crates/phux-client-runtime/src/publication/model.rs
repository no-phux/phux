//! Immutable published frame model.

use super::{Arc, Cell, Cursor, GridBuffer, GridDamage, ResourceId};
/// The scrollable area behind a published viewport, in rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scrollbar {
    /// Rows in the scrollable area (history plus the active area).
    pub total: u64,
    /// The viewport's first row within that area.
    pub offset: u64,
    /// Rows the viewport shows.
    pub len: u64,
}

impl Scrollbar {
    /// Whether the viewport follows the live tail.
    #[must_use]
    pub const fn at_tail(self) -> bool {
        self.offset.saturating_add(self.len) >= self.total
    }
}

/// One RGB color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

/// The default colors and the palette every cell of a frame was resolved
/// against, copied out of libghostty's render state so a binding needs no
/// engine type to paint a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameColors {
    /// The effective render background.
    pub background: Rgb,
    /// The effective render foreground.
    pub foreground: Rgb,
    /// The terminal explicitly set a default background.
    pub has_background: bool,
    /// The terminal explicitly set a default foreground.
    pub has_foreground: bool,
    /// Reverse-colors mode was active when projected.
    pub reversed: bool,
    /// The cursor color the terminal set, if any.
    pub cursor: Option<Rgb>,
    /// The active 256-color palette.
    pub palette: Box<[Rgb; 256]>,
}

impl Default for FrameColors {
    fn default() -> Self {
        Self {
            background: Rgb::default(),
            foreground: Rgb::default(),
            has_background: false,
            has_foreground: false,
            reversed: false,
            cursor: None,
            palette: Box::new([Rgb::default(); 256]),
        }
    }
}

/// Immutable decoded image pixels, shared across placements and frames.
#[derive(Debug)]
pub struct ImageData {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Straight-alpha RGBA8 pixels in row-major order.
    pub rgba: Arc<[u8]>,
}

/// One image destination in viewport cell coordinates.
#[derive(Debug, Clone)]
pub struct ImagePlacement {
    /// Shared decoded content; identity changes only when content changes.
    pub image: Arc<ImageData>,
    /// Left edge, including fractional pixel offset.
    pub col: f64,
    /// Top edge, including fractional pixel offset.
    pub row: f64,
    /// Destination width in cells.
    pub cols: f64,
    /// Destination height in cells.
    pub rows: f64,
    /// Source pixel rectangle `[x, y, width, height]`.
    pub source: [f64; 4],
    /// Original crop used for filtering, independent of a placeholder cell's slice.
    pub sample_source: [f64; 4],
    /// Kitty z-index.
    pub z: i32,
}

/// One published viewport: immutable once built, shared by `Arc`.
#[derive(Debug)]
pub struct GridFrame {
    /// The terminal this frame projects.
    pub terminal_id: ResourceId,
    /// Increases by one per publish of this presentation, starting at one.
    pub generation: u64,
    /// The logical subscription of the projected replica generation.
    pub stream_id: u64,
    /// The replica generation.
    pub bootstrap_id: u64,
    /// The highest live sequence applied to the replica.
    pub last_seq: u64,
    /// Viewport width in cells.
    pub cols: u16,
    /// Viewport height in cells.
    pub rows: u16,
    /// Cursor placement and shape.
    pub cursor: Cursor,
    /// The scrollable area behind the viewport.
    pub scrollbar: Scrollbar,
    /// The colors the cells were resolved against.
    pub colors: FrameColors,
    /// Image planes in protocol paint order, including smooth-scroll fringe.
    pub images: Vec<ImagePlacement>,
    /// How much changed since the previous generation: `Full` on the first
    /// frame of a replica generation and after a global change, `Rows`
    /// when only `dirty_rows` changed, `Clean` when a publish carried no
    /// grid change (a scroll that did not move, for example).
    pub damage: GridDamage,
    /// The dense `rows * cols` cells, their UTF-8 arena, their provenance,
    /// and the per-row dirty flags for this generation.
    pub buffer: GridBuffer,
}

impl GridFrame {
    /// Rows whose displayed cells changed in this generation; every row on
    /// a full projection. Includes speculative text overlay changes.
    pub fn dirty_rows(&self) -> impl Iterator<Item = u16> + '_ {
        self.buffer
            .row_dirty
            .iter()
            .enumerate()
            .filter(|(_, dirty)| **dirty)
            .filter_map(|(row, _)| u16::try_from(row).ok())
    }

    /// Whether `row` changed in this generation.
    #[must_use]
    pub fn is_row_dirty(&self, row: u16) -> bool {
        self.buffer
            .row_dirty
            .get(usize::from(row))
            .copied()
            .unwrap_or(false)
    }

    /// The cell at `(row, col)`, if inside the viewport.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        self.buffer
            .cells
            .get(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }

    /// The text of the cell at `(row, col)` as a UTF-8 slice of the arena;
    /// empty for a blank cell, a wide spacer, or a position off the grid.
    #[must_use]
    pub fn cell_text(&self, row: u16, col: u16) -> &[u8] {
        if col >= self.cols || row >= self.rows {
            return &[];
        }
        self.buffer
            .cell_text(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }

    /// One row's text with blank cells rendered as spaces and trailing
    /// blanks trimmed. A convenience for tests and diagnostics; a renderer
    /// walks the cells.
    #[must_use]
    pub fn row_text(&self, row: u16) -> String {
        let mut text = String::new();
        for col in 0..self.cols {
            let cell = self.cell_text(row, col);
            if cell.is_empty() {
                let spacer = self
                    .cell(row, col)
                    .is_some_and(|cell| cell.wide == WIDE_SPACER_TAIL);
                if !spacer {
                    text.push(' ');
                }
            } else {
                text.push_str(&String::from_utf8_lossy(cell));
            }
        }
        text.truncate(text.trim_end().len());
        text
    }

    /// Every row's text joined by newlines, trailing blank rows trimmed.
    #[must_use]
    pub fn text(&self) -> String {
        let mut rows: Vec<String> = (0..self.rows).map(|row| self.row_text(row)).collect();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        rows.join("\n")
    }
}

/// libghostty's `CellWide::SpacerTail` discriminant, as `Cell::wide` carries
/// it.
const WIDE_SPACER_TAIL: u8 = 2;
