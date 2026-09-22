use std::mem::{align_of, offset_of, size_of};

use libghostty_vt::Terminal;
use libghostty_vt::screen::{CellContentTag, CellWide};

use super::{
    CELL_BLINK, CELL_BOLD, CELL_FAINT, CELL_HYPERLINK, CELL_INVERSE, CELL_INVISIBLE, CELL_ITALIC,
    CELL_OVERLINE, CELL_PROTECTED, CELL_SELECTED, CELL_STRIKETHROUGH, COLOR_KIND_DEFAULT,
    COLOR_KIND_PALETTE, COLOR_KIND_RGB, Cell, CellMetadata, CursorStyle, CursorWidth, GridBuffer,
    GridDamage, GridProjector,
};

/// `Cell` is `PhuxTerminalCell` in `include/phux/client.h` (ABI v2). Its size
/// and every field offset are the C contract; a change here is an ABI break.
#[test]
fn cell_layout_matches_the_c_abi() {
    assert_eq!(size_of::<Cell>(), 36);
    assert_eq!(align_of::<Cell>(), 4);
    assert_eq!(offset_of!(Cell, utf8_offset), 0);
    assert_eq!(offset_of!(Cell, utf8_len), 4);
    assert_eq!(offset_of!(Cell, content_tag), 6);
    assert_eq!(offset_of!(Cell, hyperlink_offset), 8);
    assert_eq!(offset_of!(Cell, hyperlink_len), 12);
    assert_eq!(offset_of!(Cell, wide), 16);
    assert_eq!(offset_of!(Cell, semantic_content), 17);
    assert_eq!(offset_of!(Cell, flags), 20);
    assert_eq!(offset_of!(Cell, foreground_r), 24);
    assert_eq!(offset_of!(Cell, foreground_g), 25);
    assert_eq!(offset_of!(Cell, foreground_b), 26);
    assert_eq!(offset_of!(Cell, background_r), 27);
    assert_eq!(offset_of!(Cell, background_g), 28);
    assert_eq!(offset_of!(Cell, background_b), 29);
    assert_eq!(offset_of!(Cell, underline), 30);
    assert_eq!(offset_of!(Cell, underline_r), 31);
    assert_eq!(offset_of!(Cell, underline_g), 32);
    assert_eq!(offset_of!(Cell, underline_b), 33);
    assert_eq!(offset_of!(Cell, reserved), 34);
}

/// `CellMetadata` is `PhuxGridCellMetadata` in the same header.
#[test]
fn cell_metadata_layout_matches_the_c_abi() {
    assert_eq!(size_of::<CellMetadata>(), 4);
    assert_eq!(offset_of!(CellMetadata, foreground_kind), 0);
    assert_eq!(offset_of!(CellMetadata, foreground_palette_index), 1);
    assert_eq!(offset_of!(CellMetadata, underline_color_is_default), 2);
    assert_eq!(offset_of!(CellMetadata, background_color_is_default), 3);
}

#[test]
fn cursor_style_numbers_match_the_c_abi() {
    assert_eq!(CursorStyle::Bar as u32, 0);
    assert_eq!(CursorStyle::Block as u32, 1);
    assert_eq!(CursorStyle::Underline as u32, 2);
    assert_eq!(CursorStyle::BlockHollow as u32, 3);
}

/// The `CELL_*` bits are `PHUX_CLIENT_CELL_*` and the `COLOR_KIND_*` values
/// are `PHUX_GRID_COLOR_*` in the header; both sides are independent literals,
/// so this test is what keeps them from drifting apart.
#[test]
fn cell_flag_bits_and_color_kinds_match_the_c_abi() {
    assert_eq!(CELL_BOLD, 1 << 0);
    assert_eq!(CELL_ITALIC, 1 << 1);
    assert_eq!(CELL_FAINT, 1 << 2);
    assert_eq!(CELL_BLINK, 1 << 3);
    assert_eq!(CELL_INVERSE, 1 << 4);
    assert_eq!(CELL_INVISIBLE, 1 << 5);
    assert_eq!(CELL_STRIKETHROUGH, 1 << 6);
    assert_eq!(CELL_OVERLINE, 1 << 7);
    assert_eq!(CELL_SELECTED, 1 << 8);
    assert_eq!(CELL_PROTECTED, 1 << 9);
    assert_eq!(CELL_HYPERLINK, 1 << 10);
    assert_eq!(COLOR_KIND_DEFAULT, 0);
    assert_eq!(COLOR_KIND_PALETTE, 1);
    assert_eq!(COLOR_KIND_RGB, 2);
}

fn seeded_terminal(cols: u16, rows: u16, bytes: &[u8]) -> Terminal<'static, 'static> {
    let mut terminal = Terminal::new(cols, rows).expect("terminal");
    terminal.vt_write(bytes);
    terminal
}

#[test]
fn projects_a_dense_viewport_over_one_utf8_arena() {
    // Row 0: "hi", a bold red "B", then a wide CJK glyph; row 1 stays empty.
    let terminal = seeded_terminal(6, 2, "hi\x1b[1;31mB\x1b[m\u{6f22}".as_bytes());
    let mut projector = GridProjector::new().expect("projector");
    let snapshot = projector.project(&terminal, 0).expect("project");

    assert_eq!((snapshot.cols, snapshot.rows), (6, 2));
    let buffer = snapshot.buffer;
    assert_eq!(buffer.cells.len(), 12, "one record per viewport cell");
    assert_eq!(buffer.metadata.len(), buffer.cells.len());
    assert_eq!(
        buffer.utf8,
        "hiB\u{6f22}".as_bytes(),
        "empty cells add no text"
    );

    let text = |cell: &Cell| {
        let start = cell.utf8_offset as usize;
        &buffer.utf8[start..start + usize::from(cell.utf8_len)]
    };
    assert_eq!(text(&buffer.cells[0]), b"h");
    assert_eq!(text(&buffer.cells[1]), b"i");
    assert_eq!(text(&buffer.cells[2]), b"B");
    assert_eq!(text(&buffer.cells[3]), "\u{6f22}".as_bytes());
    assert_eq!(
        buffer.cells[4].utf8_len, 0,
        "the wide spacer carries no text"
    );
    assert_eq!(buffer.cells[3].wide, CellWide::Wide as u8);
    assert_eq!(buffer.cells[4].wide, CellWide::SpacerTail as u8);
    for cell in &buffer.cells[5..] {
        assert_eq!(cell.utf8_len, 0);
        assert_eq!(cell.content_tag, CellContentTag::Codepoint as u16);
    }

    assert_eq!(buffer.cells[2].flags & CELL_BOLD, CELL_BOLD);
    assert_eq!(buffer.cells[1].flags & CELL_BOLD, 0);
    assert_eq!(buffer.metadata[2].foreground_kind, COLOR_KIND_PALETTE);
    assert_eq!(buffer.metadata[2].foreground_palette_index, 1);
    assert_eq!(buffer.metadata[1].foreground_kind, COLOR_KIND_DEFAULT);
    let red = snapshot.colors.palette[1];
    assert_eq!(
        (
            buffer.cells[2].foreground_r,
            buffer.cells[2].foreground_g,
            buffer.cells[2].foreground_b
        ),
        (red.r, red.g, red.b),
        "palette colors resolve through the snapshot palette"
    );

    assert!(snapshot.cursor.visible);
    assert_eq!((snapshot.cursor.col, snapshot.cursor.row), (5, 0));
    assert_eq!(snapshot.cursor.width, CursorWidth::Narrow);
}

#[test]
fn cursor_reports_the_wide_glyph_under_it() {
    // Write the glyph, then step back onto its head cell.
    let terminal = seeded_terminal(4, 1, "\u{6f22}\x1b[2D".as_bytes());
    let mut projector = GridProjector::new().expect("projector");
    let snapshot = projector.project(&terminal, 0).expect("project");
    assert_eq!((snapshot.cursor.col, snapshot.cursor.row), (0, 0));
    assert_eq!(snapshot.cursor.width, CursorWidth::Wide);
    assert!(snapshot.cursor.width.is_wide());
}

#[test]
fn hyperlink_uris_share_the_arena_and_set_the_flag() {
    let terminal = seeded_terminal(4, 1, b"\x1b]8;;https://phux.sh\x1b\\L\x1b]8;;\x1b\\x");
    let mut projector = GridProjector::new().expect("projector");
    let snapshot = projector.project(&terminal, 0).expect("project");
    let buffer = snapshot.buffer;

    let linked = &buffer.cells[0];
    assert_eq!(linked.flags & CELL_HYPERLINK, CELL_HYPERLINK);
    let start = linked.hyperlink_offset as usize;
    assert_eq!(
        &buffer.utf8[start..start + linked.hyperlink_len as usize],
        b"https://phux.sh"
    );
    let plain = &buffer.cells[1];
    assert_eq!(plain.flags & CELL_HYPERLINK, 0);
    assert_eq!((plain.hyperlink_offset, plain.hyperlink_len), (0, 0));
    assert_eq!(buffer.utf8, b"Lhttps://phux.shx");
}

#[test]
fn reprojecting_reuses_the_buffer_and_tracks_the_cursor() {
    let mut terminal = seeded_terminal(3, 2, b"a");
    let mut projector = GridProjector::new().expect("projector");
    let first = projector.project(&terminal, 0).expect("project");
    assert_eq!(first.buffer.utf8, b"a");
    assert_eq!((first.cursor.col, first.cursor.row), (1, 0));

    terminal.vt_write(b"\r\nbc");
    let second = projector.project(&terminal, 0).expect("project");
    assert_eq!(second.buffer.cells.len(), 6);
    assert_eq!(second.buffer.utf8, b"abc");
    assert_eq!((second.cursor.col, second.cursor.row), (2, 1));
    assert_eq!(second.cursor.style, CursorStyle::Block);
}

/// The first projection is full with every row dirty; a later one reports
/// exactly the rows libghostty saw change, and clears its flags so an
/// unchanged terminal projects clean.
#[test]
fn damage_and_row_dirty_track_changes_between_projections() {
    let mut terminal = seeded_terminal(4, 3, b"a");
    let mut projector = GridProjector::new().expect("projector");
    let first = projector.project(&terminal, 0).expect("project");
    assert_eq!(first.damage, GridDamage::Full);
    assert_eq!(first.buffer.row_dirty, vec![true, true, true]);

    let unchanged = projector.project(&terminal, 0).expect("project");
    assert_eq!(unchanged.damage, GridDamage::Clean);
    assert!(unchanged.buffer.row_dirty.iter().all(|dirty| !dirty));

    // Move to row 3 and write: only that row is dirty.
    terminal.vt_write(b"\x1b[3;1Hz");
    let third = projector.project(&terminal, 0).expect("project");
    assert_ne!(third.damage, GridDamage::Clean);
    assert!(third.buffer.row_dirty[2], "the written row is dirty");
    if third.damage == GridDamage::Rows {
        assert!(!third.buffer.row_dirty[1], "an untouched row stays clean");
    }
    assert_eq!(third.buffer.utf8, b"az");
}

/// `swap_buffer` is the double-buffering seam: the caller takes the filled
/// buffer out and the projector keeps working with the one handed back.
#[test]
fn swap_buffer_hands_the_projection_out_without_copying() {
    let terminal = seeded_terminal(3, 1, b"xy");
    let mut projector = GridProjector::new().expect("projector");
    projector.project(&terminal, 0).expect("project");
    let mut taken = GridBuffer::default();
    projector.swap_buffer(&mut taken);
    assert_eq!(taken.utf8, b"xy");
    assert_eq!(taken.cell_text(1), b"y");
    assert!(projector.buffer().cells.is_empty());
    let again = projector.project(&terminal, 0).expect("project");
    assert_eq!(again.buffer.utf8, b"xy");
    assert_eq!(again.damage, GridDamage::Clean);
}

/// phux-u8zm / phux-5pyx: a resize rebuilds the pooled trio, so cells that
/// did not exist at the old width are part of the next projection and the
/// damage is a full repaint.
#[test]
fn resize_rebuilds_the_pool_past_the_old_width() {
    let mut terminal = seeded_terminal(4, 2, b"ab");
    let mut projector = GridProjector::new().expect("projector");
    let first = projector.project(&terminal, 0).expect("project");
    assert_eq!((first.cols, first.rows), (4, 2));
    assert_eq!(first.buffer.cell_text(0), b"a");

    terminal.resize(8, 3, 0, 0).expect("resize");
    terminal.vt_write(b"\x1b[1;5HX");
    let after = projector.project(&terminal, 0).expect("project");
    assert_eq!((after.cols, after.rows), (8, 3));
    assert_eq!(after.buffer.cells.len(), 8 * 3);
    assert_eq!(after.damage, GridDamage::Full);
    assert_eq!(after.buffer.cell_text(0), b"a");
    assert_eq!(after.buffer.cell_text(4), b"X");
}

/// phux-994s: a generation change rebuilds at identical geometry, so the
/// next projection is full instead of serving the previous generation's
/// already-cleared cache as clean.
#[test]
fn generation_change_rebuilds_at_identical_geometry() {
    let terminal = seeded_terminal(4, 1, b"ab");
    let mut projector = GridProjector::new().expect("projector");
    let first = projector.project(&terminal, 1).expect("project");
    assert_eq!(first.damage, GridDamage::Full);

    let second = projector.project(&terminal, 1).expect("project");
    assert_eq!(second.damage, GridDamage::Clean);

    let third = projector.project(&terminal, 2).expect("project");
    assert_eq!(third.damage, GridDamage::Full);
    assert_eq!(third.buffer.utf8, b"ab");
    assert_eq!(third.buffer.cell_text(0), b"a");
}
