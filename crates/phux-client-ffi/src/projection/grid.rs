//! One reading of a published [`GridFrame`].
//!
//! The two encoders want different things from the same frame: the C lane
//! lends pointers straight into the retained `Arc<GridFrame>`, and the `UniFFI`
//! lane copies the cells into a byte vector because Swift and Kotlin cannot
//! borrow. Both start from [`view`], so "what a frame says" is decided once
//! and only "how it crosses" differs.
//!
//! Cells cross as the pinned 36-byte `phux_client_core::grid::Cell` layout
//! plus the shared UTF-8 arena every cell addresses — never as one foreign
//! record per cell.

use phux_client_core::grid::{Cell, Cursor, GridDamage};
use phux_client_runtime::publication::{GridFrame, Rgb, Scrollbar};

/// The width of one serialized [`Cell`], with its padding bytes explicit.
pub const CELL_BYTES: usize = 36;

/// One published frame, read once.
#[derive(Debug, Clone, Copy)]
pub struct GridView<'a> {
    /// Increases by one per publish of this terminal, starting at one.
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
    /// The dense `rows * cols` cells.
    pub cells: &'a [Cell],
    /// The arena every cell's `utf8_offset`/`utf8_len` addresses.
    pub utf8: &'a [u8],
    /// Cursor placement and shape.
    pub cursor: &'a Cursor,
    /// The scrollable area behind the viewport.
    pub scrollbar: Scrollbar,
    /// The effective render foreground.
    pub default_fg: Rgb,
    /// The effective render background.
    pub default_bg: Rgb,
    /// How much changed since the previous generation.
    pub damage: GridDamage,
}

/// Read one published frame.
#[must_use]
pub fn view(frame: &GridFrame) -> GridView<'_> {
    GridView {
        generation: frame.generation,
        stream_id: frame.stream_id,
        bootstrap_id: frame.bootstrap_id,
        last_seq: frame.last_seq,
        cols: frame.cols,
        rows: frame.rows,
        cells: &frame.buffer.cells,
        utf8: &frame.buffer.utf8,
        cursor: &frame.cursor,
        scrollbar: frame.scrollbar,
        default_fg: frame.colors.foreground,
        default_bg: frame.colors.background,
        damage: frame.damage,
    }
}

/// The rows this generation changed, ascending.
#[must_use]
pub fn dirty_rows(frame: &GridFrame) -> Vec<u16> {
    frame.dirty_rows().collect()
}

/// The inclusive first and last changed row, or `None` when nothing changed.
#[must_use]
pub fn dirty_row_span(frame: &GridFrame) -> Option<(u16, u16)> {
    let mut rows = frame.dirty_rows();
    let first = rows.next()?;
    Some((first, rows.last().unwrap_or(first)))
}

/// Serialize cells without reading Rust padding.
///
/// Bytes 18-19 and 35 are explicitly zero, matching the shared layout's
/// pinned offsets, so a foreign decoder reads the same bytes the C lane
/// lends by pointer.
#[must_use]
pub fn encode_cells(cells: &[Cell]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(cells.len() * CELL_BYTES);
    for cell in cells {
        bytes.extend_from_slice(&cell.utf8_offset.to_ne_bytes());
        bytes.extend_from_slice(&cell.utf8_len.to_ne_bytes());
        bytes.extend_from_slice(&cell.content_tag.to_ne_bytes());
        bytes.extend_from_slice(&cell.hyperlink_offset.to_ne_bytes());
        bytes.extend_from_slice(&cell.hyperlink_len.to_ne_bytes());
        bytes.push(cell.wide);
        bytes.push(cell.semantic_content);
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&cell.flags.to_ne_bytes());
        bytes.extend_from_slice(&[
            cell.foreground_r,
            cell.foreground_g,
            cell.foreground_b,
            cell.background_r,
            cell.background_g,
            cell.background_b,
            cell.underline,
            cell.underline_r,
            cell.underline_g,
            cell.underline_b,
            cell.reserved,
            0,
        ]);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::{CELL_BYTES, encode_cells};
    use phux_client_core::grid::Cell;

    #[test]
    fn a_cell_serializes_to_the_pinned_width() {
        assert_eq!(encode_cells(&[Cell::default()]).len(), CELL_BYTES);
        assert_eq!(size_of::<Cell>(), CELL_BYTES);
    }

    #[test]
    fn the_padding_bytes_are_explicitly_zero() {
        let cell = Cell {
            utf8_offset: 0x0102_0304,
            utf8_len: 5,
            ..Cell::default()
        };
        let bytes = encode_cells(&[cell]);
        assert_eq!(&bytes[0..4], &0x0102_0304_u32.to_ne_bytes());
        assert_eq!(&bytes[4..6], &5_u16.to_ne_bytes());
        assert_eq!(&bytes[18..20], &[0, 0]);
        assert_eq!(bytes[35], 0);
    }

    #[test]
    fn an_empty_grid_serializes_to_nothing() {
        assert!(encode_cells(&[]).is_empty());
    }
}
