use super::*;
use crate::publication::{Cursor, FrameColors, GridBuffer, GridDamage, Scrollbar};
use phux_client_core::grid::{CELL_OVERLINE, CELL_STRIKETHROUGH};
use phux_protocol::ResourceId;

const RED: Rgb = rgb(255, 0, 0);
const BLUE: Rgb = rgb(0, 0, 255);
const WHITE: Rgb = rgb(255, 255, 255);
const GREY: Rgb = rgb(40, 40, 40);

fn theme() -> Theme<Rgb> {
    Theme::frame_colors((WHITE, GREY))
}

/// A `cols`-wide frame of one row per entry, each cell's text one grapheme.
fn frame(rows: &[&[&str]], cols: u16) -> GridFrame {
    let mut buffer = GridBuffer::default();
    for row in rows {
        for col in 0..usize::from(cols) {
            let text = row.get(col).copied().unwrap_or("");
            let offset = u32::try_from(buffer.utf8.len()).unwrap_or(0);
            buffer.utf8.extend_from_slice(text.as_bytes());
            buffer.cells.push(Cell {
                utf8_offset: offset,
                utf8_len: u16::try_from(text.len()).unwrap_or(0),
                ..Cell::default()
            });
            buffer.metadata.push(CellMetadata {
                background_color_is_default: true,
                underline_color_is_default: true,
                ..CellMetadata::default()
            });
        }
    }
    GridFrame {
        terminal_id: ResourceId::local(1),
        generation: 1,
        stream_id: 1,
        bootstrap_id: 1,
        last_seq: 0,
        cols,
        rows: u16::try_from(rows.len()).unwrap_or(0),
        cursor: Cursor::default(),
        scrollbar: Scrollbar::default(),
        colors: FrameColors {
            foreground: WHITE,
            background: rgb(0, 0, 0),
            ..FrameColors::default()
        },
        damage: GridDamage::Full,
        images: Vec::new(),
        buffer,
    }
}

fn cell_mut(frame: &mut GridFrame, row: u16, col: u16) -> (&mut Cell, &mut CellMetadata) {
    let index = usize::from(row) * usize::from(frame.cols) + usize::from(col);
    (
        &mut frame.buffer.cells[index],
        &mut frame.buffer.metadata[index],
    )
}

fn texts(list: &DrawList<Rgb>) -> Vec<(u16, u16, u16, &str)> {
    list.texts
        .iter()
        .map(|run| (run.row, run.col, run.cells, &list.text[run.text.clone()]))
        .collect()
}

#[test]
fn inverse_swaps_true_color_before_selection_and_faint() {
    let frame = frame(&[], 1);
    let theme = theme();
    let mut cell = Cell {
        foreground_r: 255,
        background_b: 255,
        flags: CELL_INVERSE,
        ..Cell::default()
    };
    let metadata = CellMetadata {
        foreground_kind: 2,
        ..CellMetadata::default()
    };
    let defaults = default_colors(&frame, &theme);
    let colors = cell_colors(&cell, metadata, &frame, &theme, defaults);
    assert_eq!((colors.foreground, colors.background), (BLUE, RED));
    assert!(!colors.faint);
    cell.flags |= CELL_SELECTED | CELL_FAINT;
    let colors = cell_colors(&cell, metadata, &frame, &theme, defaults);
    assert_eq!((colors.foreground, colors.background), (WHITE, GREY));
    assert!(colors.faint);
}

#[test]
fn the_theme_maps_palette_provenance_and_yields_to_application_defaults() {
    let mut frame = frame(&[], 1);
    frame.colors.palette[1] = rgb(205, 0, 0);
    let mut ansi = [rgb(0, 0, 0); 16];
    ansi[1] = rgb(1, 2, 3);
    let theme = Theme {
        foreground: Some(rgb(9, 9, 9)),
        background: Some(rgb(8, 8, 8)),
        ansi: Some(ansi),
        ..theme()
    };
    let palette_fg = CellMetadata {
        foreground_kind: COLOR_KIND_PALETTE,
        foreground_palette_index: 1,
        ..CellMetadata::default()
    };
    let red_bg = Cell {
        background_r: 205,
        ..Cell::default()
    };
    let defaults = default_colors(&frame, &theme);
    let colors = cell_colors(&red_bg, palette_fg, &frame, &theme, defaults);
    assert_eq!(
        colors.foreground,
        rgb(1, 2, 3),
        "palette index takes the theme"
    );
    assert_eq!(
        colors.background,
        rgb(1, 2, 3),
        "an ANSI-equal rgb takes the theme"
    );
    assert_eq!(defaults, (rgb(9, 9, 9), rgb(8, 8, 8)));
    frame.colors.reversed = true;
    assert_eq!(default_colors(&frame, &theme), (rgb(8, 8, 8), rgb(9, 9, 9)));
    frame.colors.has_foreground = true;
    assert_eq!(default_colors(&frame, &theme).0, WHITE, "OSC 10 wins");
}

#[test]
fn faces_hidden_cells_and_blink() {
    let bold_italic = Cell {
        flags: CELL_BOLD | CELL_ITALIC,
        ..Cell::default()
    };
    assert_eq!(Face::of(&bold_italic).index(), 3);
    assert_eq!(Face::of(&Cell::default()).index(), 0);
    let blinking = Cell {
        flags: CELL_BLINK,
        ..Cell::default()
    };
    let off = Visibility {
        blink_visible: false,
        ..Visibility::default()
    };
    assert!(!hidden(&blinking, Visibility::default()));
    assert!(hidden(&blinking, off));
    let invisible = Cell {
        flags: CELL_INVISIBLE,
        ..Cell::default()
    };
    assert!(hidden(&invisible, Visibility::default()));
}

#[test]
fn a_wide_tail_cursor_paints_from_its_head_and_unfocused_is_hollow() {
    let mut frame = frame(&[], 1);
    frame.cursor = Cursor {
        visible: true,
        col: 4,
        row: 1,
        width: CursorWidth::WideTail,
        style: CursorStyle::Block,
        blinking: false,
    };
    let theme = theme();
    let focused = cursor(&frame, &theme, Visibility::default()).expect("cursor");
    assert_eq!((focused.row, focused.col, focused.width), (1, 3, 2));
    assert!(focused.solid);
    assert_eq!(focused.color, WHITE, "falls back to the default foreground");
    let unfocused = Visibility {
        focused: false,
        ..Visibility::default()
    };
    let hollow = cursor(&frame, &theme, unfocused).expect("cursor");
    assert_eq!(hollow.style, CursorStyle::BlockHollow);
    assert!(!hollow.solid);
    frame.cursor.blinking = true;
    let off = Visibility {
        blink_visible: false,
        ..Visibility::default()
    };
    assert!(cursor(&frame, &theme, off).is_none());
}

#[test]
fn blocks_are_coverage_and_a_presentation_selector_is_text() {
    assert_eq!(
        block_regions("▀".as_bytes()),
        Some(&[region!(0, 0, 8, 4)][..])
    );
    assert_eq!(block_regions("▚".as_bytes()).map(<[_]>::len), Some(2));
    assert!(block_regions("▀\u{fe0f}".as_bytes()).is_none());
    assert!(block_regions(b"a").is_none());
}

#[test]
fn runs_span_blanks_and_break_at_style_and_wide_cells() {
    let mut frame = frame(&[&["h", "i", "", "y", "o", "世", "", "x"]], 8);
    cell_mut(&mut frame, 0, 4).0.flags = CELL_BOLD;
    cell_mut(&mut frame, 0, 5).0.wide = 1;
    cell_mut(&mut frame, 0, 6).0.wide = 2;
    let list = DrawList::build(&frame, &theme(), Visibility::default());
    assert_eq!(
        texts(&list),
        vec![
            (0, 0, 4, "hi y"),
            (0, 4, 1, "o"),
            (0, 5, 2, "世"),
            (0, 7, 1, "x"),
        ]
    );
    assert!(list.texts[1].face.bold);
    assert!(list.texts[2].lone && !list.texts[0].lone);
}

#[test]
fn blanks_join_a_run_only_up_to_its_last_glyph() {
    // Trailing blanks stay out; a block between glyphs and a hidden blank
    // each end the run; a row never continues the one above.
    let mut frame = frame(&[&["a", " ", "b", " ", ""], &["c", "▀", "d", "", "e"]], 5);
    cell_mut(&mut frame, 1, 3).0.flags = CELL_INVISIBLE;
    let list = DrawList::build(&frame, &theme(), Visibility::default());
    assert_eq!(
        texts(&list),
        vec![
            (0, 0, 3, "a b"),
            (1, 0, 1, "c"),
            (1, 2, 1, "d"),
            (1, 4, 1, "e"),
        ]
    );
}

#[test]
fn backgrounds_merge_into_runs_and_skip_the_default() {
    let mut frame = frame(&[&["a", "b", "c", "d"]], 4);
    for col in 1..3 {
        let (cell, metadata) = cell_mut(&mut frame, 0, col);
        cell.background_r = 255;
        metadata.background_color_is_default = false;
    }
    let list = DrawList::build(&frame, &theme(), Visibility::default());
    assert_eq!(list.background, rgb(0, 0, 0));
    assert_eq!(
        list.backgrounds,
        vec![BackgroundRun {
            row: 0,
            start: 1,
            end: 3,
            color: RED,
        }]
    );
}

#[test]
fn a_solid_cursor_glyph_takes_the_cell_background() {
    let mut frame = frame(&[&["a", "b"]], 2);
    frame.cursor = Cursor {
        visible: true,
        col: 1,
        style: CursorStyle::Block,
        ..Cursor::default()
    };
    let list = DrawList::build(&frame, &theme(), Visibility::default());
    assert_eq!(texts(&list), vec![(0, 0, 1, "a"), (0, 1, 1, "b")]);
    assert_eq!(list.texts[0].color, WHITE);
    assert_eq!(list.texts[1].color, rgb(0, 0, 0));
    assert!(list.cursor.is_some_and(|cursor| cursor.solid));
}

#[test]
fn decorations_merge_across_cells_and_hidden_cells_paint_only_background() {
    let mut frame = frame(&[&["a", "b", "c", "d"]], 4);
    for col in 0..3 {
        cell_mut(&mut frame, 0, col).0.underline = 3;
    }
    cell_mut(&mut frame, 0, 1).0.flags = CELL_STRIKETHROUGH;
    cell_mut(&mut frame, 0, 3).0.flags = CELL_INVISIBLE;
    let list = DrawList::build(&frame, &theme(), Visibility::default());
    let lines: Vec<_> = list
        .decorations
        .iter()
        .map(|line| (line.start, line.end, line.kind))
        .collect();
    assert_eq!(
        lines,
        vec![
            (0, 3, DecorationKind::Underline(Underline::Curly)),
            (1, 2, DecorationKind::Strikethrough),
        ]
    );
    assert_eq!(texts(&list), vec![(0, 0, 3, "abc")]);
}

#[test]
fn block_elements_leave_text_runs() {
    let frame = frame(&[&["a", "█", "b"]], 3);
    let list = DrawList::build(&frame, &theme(), Visibility::default());
    assert_eq!(texts(&list), vec![(0, 0, 1, "a"), (0, 2, 1, "b")]);
    assert_eq!(list.blocks.len(), 1);
    assert_eq!((list.blocks[0].col, list.blocks[0].regions.len()), (1, 1));
}

#[test]
fn a_line_merges_past_one_the_same_cell_starts() {
    let mut frame = frame(&[&["a", "b", "c"]], 3);
    cell_mut(&mut frame, 0, 0).0.flags = CELL_STRIKETHROUGH;
    for col in 1..3 {
        let (cell, _) = cell_mut(&mut frame, 0, col);
        cell.flags = CELL_STRIKETHROUGH | CELL_OVERLINE;
        cell.underline = 1;
    }
    let list = DrawList::build(&frame, &theme(), Visibility::default());
    let lines: Vec<_> = list
        .decorations
        .iter()
        .map(|line| (line.kind, line.start, line.end))
        .collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines.contains(&(DecorationKind::Strikethrough, 0, 3)));
    assert!(lines.contains(&(DecorationKind::Underline(Underline::Single), 1, 3)));
    assert!(lines.contains(&(DecorationKind::Overline, 1, 3)));
}
