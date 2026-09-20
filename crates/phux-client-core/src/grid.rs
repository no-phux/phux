//! The one terminal cell layout every binding projects (ADR-0133 decision 3).
//!
//! A [`GridProjector`] walks libghostty's render state once per build and
//! flattens the viewport into a [`GridBuffer`]: one plain-old-data [`Cell`]
//! per viewport cell in row-major order, the UTF-8 arena their text and
//! hyperlink URIs address, and one [`CellMetadata`] record per cell carrying
//! the color provenance the resolved RGB fields lose. The C ABI lends a
//! pointer to that buffer; the mobile bridge copies it as byte vectors once
//! it re-pins (ADR-0133 decision 3). Neither defines a second cell.
//!
//! [`Cell`] is `#[repr(C)]` and its field order is part of the native ABI
//! (`crates/phux-client-ffi/include/phux/client.h`, `PhuxTerminalCell`,
//! ABI v2). The layout test in `grid/tests.rs` pins its size and every field
//! offset; changing either is a C ABI break, not a refactor.

use std::fmt;

use libghostty_vt::Terminal;
use libghostty_vt::render::{
    CellIterator, Colors, CursorViewport, CursorVisualStyle, Dirty, RenderState, RowIterator,
    Snapshot,
};
use libghostty_vt::screen::CellWide;
use thiserror::Error;

mod flatten;
#[cfg(test)]
mod tests;

/// `Cell::flags` bit: SGR bold.
pub const CELL_BOLD: u32 = 1 << 0;
/// `Cell::flags` bit: SGR italic.
pub const CELL_ITALIC: u32 = 1 << 1;
/// `Cell::flags` bit: SGR faint.
pub const CELL_FAINT: u32 = 1 << 2;
/// `Cell::flags` bit: SGR blink.
pub const CELL_BLINK: u32 = 1 << 3;
/// `Cell::flags` bit: SGR inverse.
pub const CELL_INVERSE: u32 = 1 << 4;
/// `Cell::flags` bit: SGR invisible.
pub const CELL_INVISIBLE: u32 = 1 << 5;
/// `Cell::flags` bit: SGR strikethrough.
pub const CELL_STRIKETHROUGH: u32 = 1 << 6;
/// `Cell::flags` bit: SGR overline.
pub const CELL_OVERLINE: u32 = 1 << 7;
/// `Cell::flags` bit: the cell is inside the active selection.
pub const CELL_SELECTED: u32 = 1 << 8;
/// `Cell::flags` bit: the cell is protected (DECSCA).
pub const CELL_PROTECTED: u32 = 1 << 9;
/// `Cell::flags` bit: the cell carries an OSC 8 hyperlink; see
/// `Cell::hyperlink_offset` and `Cell::hyperlink_len`.
pub const CELL_HYPERLINK: u32 = 1 << 10;

/// `CellMetadata::foreground_kind`: the terminal's default foreground.
pub const COLOR_KIND_DEFAULT: u8 = 0;
/// `CellMetadata::foreground_kind`: a 256-color palette index.
pub const COLOR_KIND_PALETTE: u8 = 1;
/// `CellMetadata::foreground_kind`: a direct RGB color.
pub const COLOR_KIND_RGB: u8 = 2;

/// One viewport cell, flattened: text and hyperlink URI as spans into the
/// owning [`GridBuffer`]'s UTF-8 arena, every color palette-resolved to RGB.
///
/// The `content_tag`, `wide`, `semantic_content`, and `underline` fields
/// carry libghostty's `CellContentTag`, `CellWide`, `CellSemanticContent`,
/// and `Underline` discriminants; the header names them.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Cell {
    /// Byte offset of this cell's text in the UTF-8 arena.
    pub utf8_offset: u32,
    /// Byte length of this cell's text; zero for an empty or background-only
    /// cell and for a wide-character spacer.
    pub utf8_len: u16,
    /// libghostty `CellContentTag` discriminant.
    pub content_tag: u16,
    /// Byte offset of the hyperlink URI in the UTF-8 arena; meaningful only
    /// with `CELL_HYPERLINK` set.
    pub hyperlink_offset: u32,
    /// Byte length of the hyperlink URI; zero without `CELL_HYPERLINK`.
    pub hyperlink_len: u32,
    /// libghostty `CellWide` discriminant.
    pub wide: u8,
    /// libghostty `CellSemanticContent` discriminant.
    pub semantic_content: u8,
    /// The `CELL_*` flag word.
    pub flags: u32,
    /// Resolved foreground, red.
    pub foreground_r: u8,
    /// Resolved foreground, green.
    pub foreground_g: u8,
    /// Resolved foreground, blue.
    pub foreground_b: u8,
    /// Resolved background, red.
    pub background_r: u8,
    /// Resolved background, green.
    pub background_g: u8,
    /// Resolved background, blue.
    pub background_b: u8,
    /// libghostty `Underline` discriminant.
    pub underline: u8,
    /// Resolved underline color, red.
    pub underline_r: u8,
    /// Resolved underline color, green.
    pub underline_g: u8,
    /// Resolved underline color, blue.
    pub underline_b: u8,
    /// Always zero; the last byte of the 36-byte record (its trailing `u8`
    /// run begins at offset 24, after the 4-aligned `flags`).
    pub reserved: u8,
}

/// Color provenance a [`Cell`]'s palette-resolved RGB fields lose, so a
/// renderer can apply palette policy (bold-as-bright, theme remaps) without
/// guessing the index back from the resolved color.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CellMetadata {
    /// One of the `COLOR_KIND_*` values.
    pub foreground_kind: u8,
    /// Meaningful only for `COLOR_KIND_PALETTE`.
    pub foreground_palette_index: u8,
    /// The style named no underline color; `underline_*` fell back to the
    /// resolved foreground.
    pub underline_color_is_default: bool,
    /// Neither the style nor a background-only content tag named a
    /// background; `background_*` is the terminal default.
    pub background_color_is_default: bool,
}

/// The cursor shape libghostty reports for the viewport, numbered as the C
/// ABI's `PhuxCursorStyle`.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorStyle {
    /// Bar cursor (DECSCUSR 5, 6).
    #[default]
    Bar = 0,
    /// Block cursor (DECSCUSR 1, 2).
    Block = 1,
    /// Underline cursor (DECSCUSR 3, 4).
    Underline = 2,
    /// Hollow block, as an unfocused terminal draws it.
    BlockHollow = 3,
}

impl From<CursorVisualStyle> for CursorStyle {
    fn from(style: CursorVisualStyle) -> Self {
        match style {
            CursorVisualStyle::Block => Self::Block,
            CursorVisualStyle::Underline => Self::Underline,
            CursorVisualStyle::BlockHollow => Self::BlockHollow,
            _ => Self::Bar,
        }
    }
}

/// How many cells the glyph under the cursor occupies, and which of them the
/// cursor is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorWidth {
    /// A single-width cell, or no cursor on the viewport.
    #[default]
    Narrow,
    /// The head cell of a wide character.
    Wide,
    /// The spacer tail of a wide character.
    WideTail,
}

impl CursorWidth {
    /// The cursor covers a wide character, from either of its cells.
    #[must_use]
    pub const fn is_wide(self) -> bool {
        matches!(self, Self::Wide | Self::WideTail)
    }
}

/// Where and how the cursor is drawn on one projected viewport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    /// The cursor is on the viewport and the terminal is not hiding it.
    pub visible: bool,
    /// Viewport column; zero when the cursor is off the viewport.
    pub col: u16,
    /// Viewport row; zero when the cursor is off the viewport.
    pub row: u16,
    /// Shape to draw.
    pub style: CursorStyle,
    /// The terminal asked for a blinking cursor.
    pub blinking: bool,
    /// Width of the glyph under the cursor.
    pub width: CursorWidth,
}

/// How much of the viewport changed since the previous projection, as
/// libghostty's render state reports it (`Snapshot::dirty`).
///
/// A projector's first projection is always [`GridDamage::Full`], and every
/// row of a full projection is marked dirty, so a consumer that keys its
/// work on damage never misses the first paint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GridDamage {
    /// Nothing changed; a consumer may skip the frame.
    Clean,
    /// Some rows changed; `GridBuffer::row_dirty` says which.
    Rows,
    /// Global state changed (a resize, a palette change, a scroll of the
    /// viewport); every row is dirty.
    #[default]
    Full,
}

impl From<Dirty> for GridDamage {
    fn from(dirty: Dirty) -> Self {
        match dirty {
            Dirty::Clean => Self::Clean,
            Dirty::Partial => Self::Rows,
            Dirty::Full => Self::Full,
        }
    }
}

/// The dense, row-major viewport a [`GridProjector`] fills.
///
/// `cells` and `metadata` are parallel: index `row * cols + col` in each.
/// `utf8` is the arena every cell's text and hyperlink spans address; it is
/// valid UTF-8 as a whole because each span is appended as whole scalars.
/// `row_dirty` has one flag per viewport row: whether libghostty reported
/// the row changed since the previous projection of the same projector.
#[derive(Debug, Default)]
pub struct GridBuffer {
    /// One record per viewport cell.
    pub cells: Vec<Cell>,
    /// The shared text and hyperlink arena.
    pub utf8: Vec<u8>,
    /// One provenance record per viewport cell.
    pub metadata: Vec<CellMetadata>,
    /// One flag per viewport row: changed since the previous projection.
    pub row_dirty: Vec<bool>,
}

impl GridBuffer {
    fn clear(&mut self) {
        self.cells.clear();
        self.utf8.clear();
        self.metadata.clear();
        self.row_dirty.clear();
    }

    /// The text of the cell at `index`, as a UTF-8 slice of the arena.
    #[must_use]
    pub fn cell_text(&self, index: usize) -> &[u8] {
        self.cells.get(index).map_or(&[], |cell| {
            let start = cell.utf8_offset as usize;
            &self.utf8[start..start + usize::from(cell.utf8_len)]
        })
    }
}

/// One projected viewport: its geometry, cursor, and colors by value, and
/// the projector's buffer on loan until the next projection.
#[derive(Debug)]
pub struct GridSnapshot<'a> {
    /// Viewport width in cells.
    pub cols: u16,
    /// Viewport height in cells.
    pub rows: u16,
    /// Cursor placement and shape.
    pub cursor: Cursor,
    /// The default colors, cursor color, and palette the cells were
    /// resolved against.
    pub colors: Colors,
    /// How much changed since this projector's previous projection.
    pub damage: GridDamage,
    /// The dense `rows * cols` buffer.
    pub buffer: &'a GridBuffer,
}

/// Why a projection could not be completed. The buffer is left cleared or
/// partially filled and must not be read.
#[derive(Debug, Error)]
pub enum GridError {
    /// libghostty refused a render-state, iterator, or cell query.
    #[error("libghostty render query failed: {0}")]
    Engine(#[from] libghostty_vt::Error),
    /// A span, coordinate, or count outgrew the integer that carries it; the
    /// payload names both, as in `"render column exceeds u16"`.
    #[error("{0}")]
    Overflow(&'static str),
    /// libghostty reported a codepoint that is not a Unicode scalar value.
    #[error("invalid terminal codepoint {0:#x}")]
    InvalidCodepoint(u32),
    /// The iterators did not yield exactly `cols * rows` cells; the layout
    /// promises a dense viewport, so a short grid is a defect, not data.
    #[error("render iterators produced {produced} cells for a {expected}-cell viewport")]
    SparseViewport {
        /// Cells the walk produced.
        produced: usize,
        /// `cols * rows`.
        expected: usize,
    },
}

/// Owns one libghostty render trio and the buffer it flattens into.
///
/// Construct one per terminal and keep it: the render state, both iterators,
/// the buffer, and the per-cell scratch are all reused across projections, so
/// a steady-state build allocates only when the viewport or a grapheme
/// outgrows what earlier builds reserved.
pub struct GridProjector {
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    buffer: GridBuffer,
    scratch: flatten::CellScratch,
    /// Whether a projection has completed: the first one is always full.
    projected: bool,
}

impl fmt::Debug for GridProjector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GridProjector")
            .field("buffer", &self.buffer)
            .finish_non_exhaustive()
    }
}

impl GridProjector {
    /// Allocate the render state and iterators for one terminal.
    pub fn new() -> Result<Self, GridError> {
        Ok(Self {
            state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            buffer: GridBuffer::default(),
            scratch: flatten::CellScratch::new(),
            projected: false,
        })
    }

    /// Exchange the projector's buffer for `other`.
    ///
    /// This is the double-buffering seam: after a projection the caller
    /// takes the freshly filled buffer out and hands back the one it
    /// finished reading, so a steady state publishes without copying cells.
    /// The next projection clears and refills whatever buffer it holds; its
    /// damage is still relative to this projector's previous projection.
    pub const fn swap_buffer(&mut self, other: &mut GridBuffer) {
        std::mem::swap(&mut self.buffer, other);
    }

    /// The buffer the last successful [`project`](Self::project) filled.
    ///
    /// After a failed projection it holds whatever the walk had appended and
    /// must not be read as a viewport.
    #[must_use]
    pub const fn buffer(&self) -> &GridBuffer {
        &self.buffer
    }

    /// Flatten the terminal's current viewport into the buffer and return the
    /// snapshot that borrows it.
    pub fn project(
        &mut self,
        terminal: &Terminal<'static, '_>,
    ) -> Result<GridSnapshot<'_>, GridError> {
        let (cols, rows, cursor, colors, damage) = self.fill(terminal)?;
        Ok(GridSnapshot {
            cols,
            rows,
            cursor,
            colors,
            damage,
            buffer: &self.buffer,
        })
    }

    /// The part of a projection that holds the libghostty snapshot: every
    /// borrow of the render state ends before `project` lends the buffer.
    fn fill(
        &mut self,
        terminal: &Terminal<'static, '_>,
    ) -> Result<(u16, u16, Cursor, Colors, GridDamage), GridError> {
        let snapshot = self.state.update(terminal)?;
        let cols = snapshot.cols()?;
        let rows = snapshot.rows()?;
        let colors = snapshot.colors()?;
        let damage = if self.projected {
            GridDamage::from(snapshot.dirty()?)
        } else {
            GridDamage::Full
        };
        self.buffer.clear();
        let expected = usize::from(cols)
            .checked_mul(usize::from(rows))
            .ok_or(GridError::Overflow("viewport cell count exceeds usize"))?;
        self.buffer.cells.reserve(expected);
        self.buffer.metadata.reserve(expected);
        flatten::fill_grid_cells(
            &mut self.rows,
            &mut self.cells,
            &snapshot,
            &flatten::CellContext {
                terminal,
                colors: &colors,
            },
            &mut self.buffer,
            &mut self.scratch,
        )?;
        let produced = self.buffer.cells.len();
        if produced != expected {
            return Err(GridError::SparseViewport { produced, expected });
        }
        if damage == GridDamage::Full {
            self.buffer.row_dirty.iter_mut().for_each(|row| *row = true);
        }
        // The render state accumulates dirty flags until a renderer clears
        // them (the libghostty contract); this projection consumed them.
        snapshot.set_dirty(Dirty::Clean)?;
        let cursor = read_cursor(&snapshot, &self.buffer, cols)?;
        self.projected = true;
        Ok((cols, rows, cursor, colors, damage))
    }
}

/// Read the cursor placement and shape the snapshot reports, and whether the
/// cell under it is a wide character's head or tail.
fn read_cursor(
    snapshot: &Snapshot<'static, '_>,
    buffer: &GridBuffer,
    cols: u16,
) -> Result<Cursor, GridError> {
    let viewport = snapshot.cursor_viewport()?;
    let (col, row, width) = viewport.map_or((0, 0, CursorWidth::Narrow), |at| {
        (at.x, at.y, cursor_width(at, buffer, cols))
    });
    Ok(Cursor {
        visible: snapshot.cursor_visible()? && viewport.is_some(),
        col,
        row,
        style: snapshot.cursor_visual_style()?.into(),
        blinking: snapshot.cursor_blinking()?,
        width,
    })
}

/// Width of the glyph under a cursor that is on the viewport: libghostty says
/// when the cursor sits on a wide character's spacer tail; the projected cell
/// says when it sits on the head.
fn cursor_width(at: CursorViewport, buffer: &GridBuffer, cols: u16) -> CursorWidth {
    if at.at_wide_tail {
        return CursorWidth::WideTail;
    }
    let index = usize::from(at.y) * usize::from(cols) + usize::from(at.x);
    let on_wide_head = buffer
        .cells
        .get(index)
        .is_some_and(|cell| cell.wide == CellWide::Wide as u8);
    if on_wide_head {
        CursorWidth::Wide
    } else {
        CursorWidth::Narrow
    }
}
