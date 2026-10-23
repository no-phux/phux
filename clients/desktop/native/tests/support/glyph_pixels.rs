//! Main-thread GPU fixture support; shared runtime and pixel helpers live in pixels.rs.

use super::*;

/// A supported non-ASCII suffix makes the entire row use platform shaping,
/// without changing the printable ASCII prefix. Compare pixels on the real
/// text system, including GPUI's near-grid offsets at fractional font advances.
pub(super) fn cached_ascii_matches_platform_pixels() {
    for family in [phux_desktop::fonts::BUNDLED, phux_desktop::fonts::SYMBOLS] {
        for size in [14., 16.] {
            for face in ["0", "1", "3", "1;3", "mixed"] {
                let payload = |suffix: &str| {
                    (0..3)
                        .map(|row| {
                            let text: String = (0..32)
                                .map(|col| {
                                    let ch = char::from(b' ' + ((row * 32 + col) % 95) as u8);
                                    if face == "mixed" {
                                        let style = ["0", "1", "3", "1;3"][col % 4];
                                        format!("\x1b[0;{style};{}m{ch}", 31 + col % 7)
                                    } else {
                                        ch.to_string()
                                    }
                                })
                                .collect();
                            format!(
                                "\x1b[{};1H\x1b[{}m{text}{suffix}",
                                row + 1,
                                if face == "mixed" { "0" } else { face }
                            )
                        })
                        .collect::<String>()
                };
                let mut prefs = settings(false);
                prefs.font_size = size;
                prefs.font = gpui::font(family);
                let (client, view) = publish_at(payload(" ").as_bytes(), 36, 4);
                let ascii_before = phux_desktop::perf::ASCII_LINES.get();
                let cached = paint(client, view, prefs.clone());
                assert_eq!(
                    phux_desktop::perf::ASCII_LINES.get() - ascii_before,
                    3,
                    "every oracle row must exercise the ASCII cache: {family} {face} {size}"
                );
                let (client, view) = publish_at(payload("é").as_bytes(), 36, 4);
                let before = phux_desktop::perf::SHAPED.get();
                let platform = paint(client, view, prefs);
                assert_eq!(
                    phux_desktop::perf::SHAPED.get() - before,
                    3,
                    "suffix must stay in the primary font, not split back to ASCII"
                );
                let geometry = cached.painted.geometry;
                let width = device(geometry.cell_width * 32., geometry.scale);
                let height = device(geometry.cell_height * 3., geometry.scale);
                let region = |shot: &Shot| {
                    image::imageops::crop_imm(&shot.image, 0, 0, width, height).to_image()
                };
                assert_eq!(
                    region(&cached).as_raw(),
                    region(&platform).as_raw(),
                    "cached ASCII differs from platform shaping: size={size} face={face}"
                );
            }
        }
    }
}

pub(super) fn glyphs_land_in_their_columns() {
    let (client, view) = publish(b"\x1b[?25l\x1b[HAB C");
    let frame = frame_of(&client, view);
    assert_eq!(frame.row_text(0), "AB C");
    let shot = paint(client, view, settings(false));
    assert_ink(&cell_pixels(&shot, 0, 0), "column 0 (A)");
    assert_ink(&cell_pixels(&shot, 0, 1), "column 1 (B)");
    assert_blank(&cell_pixels(&shot, 0, 2), "column 2 (the gap)");
    assert_ink(&cell_pixels(&shot, 0, 3), "column 3 (C)");
}

/// Faint text and box drawing share runs with plain text and spaces, and
/// still land in their own columns; faint ink is visibly dimmer.
pub(super) fn faint_text_and_box_drawing_keep_their_columns() {
    let (client, view) = publish(
        "\u{1b}[?25l\u{1b}[HH \u{1b}[2mH\u{1b}[0m H\r\n\u{2500}\u{2502} \u{2500}".as_bytes(),
    );
    let frame = frame_of(&client, view);
    assert_eq!(frame.row_text(0), "H H H");
    let shot = paint(client, view, settings(false));
    let brightest = |pixels: &[[u8; 4]]| {
        pixels
            .iter()
            .map(|pixel| distance(*pixel, BACKGROUND))
            .max()
            .unwrap_or(0)
    };
    let plain = cell_pixels(&shot, 0, 0);
    let faint = cell_pixels(&shot, 0, 2);
    assert_ink(&plain, "column 0 (plain H)");
    assert_blank(&cell_pixels(&shot, 0, 1), "column 1 (the gap)");
    assert_ink(&cell_pixels(&shot, 0, 4), "column 4 (plain H after a gap)");
    assert!(
        brightest(&faint) * 4 < brightest(&plain) * 3 && brightest(&faint) > 40,
        "faint ink {} against plain {}",
        brightest(&faint),
        brightest(&plain)
    );
    assert_ink(&cell_pixels(&shot, 1, 0), "column 0 (horizontal rule)");
    assert_ink(&cell_pixels(&shot, 1, 1), "column 1 (vertical rule)");
    assert_blank(&cell_pixels(&shot, 1, 2), "column 2 (the gap)");
    assert_ink(&cell_pixels(&shot, 1, 3), "column 3 (horizontal rule)");
}

pub(super) fn wide_character_occupies_two_columns() {
    let (client, view) = publish("\u{1b}[?25l\u{1b}[HX中Y".as_bytes());
    let frame = frame_of(&client, view);
    assert_eq!(frame.row_text(0), "X中Y");
    assert_eq!(frame.cell(0, 1).expect("wide head").wide, 1);
    let spacer = frame.cell(0, 2).expect("spacer").wide;
    assert!(matches!(spacer, 2 | 3), "spacer wide={spacer}");
    assert_eq!(frame.cell_text(0, 3), b"Y");
    let shot = paint(client, view, settings(false));
    assert_ink(&cell_pixels(&shot, 0, 0), "column 0 (X)");
    assert_ink(&cell_pixels(&shot, 0, 1), "wide head");
    assert_ink(
        &cell_pixels(&shot, 0, 3),
        "column 3 (Y follows the wide cell)",
    );
    assert_blank(&cell_pixels(&shot, 0, 4), "column 4 stays empty");
}

pub(super) fn block_cursor_fills_its_cell() {
    // Steady block, then CUP to row 2 column 6 (1-based).
    let (client, view) = publish(b"\x1b[2 q\x1b[2;6H");
    let frame = frame_of(&client, view);
    assert!(frame.cursor.visible);
    assert_eq!(frame.cursor.style, CursorStyle::Block);
    assert_eq!((frame.cursor.row, frame.cursor.col), (1, 5));
    let shot = paint(client, view, settings(true));
    assert_mostly(&cell_pixels(&shot, 1, 5), CURSOR, "cursor cell");
    assert_mostly(
        &cell_pixels(&shot, 1, 6),
        BACKGROUND,
        "cell after the cursor",
    );
    assert_mostly(
        &cell_pixels(&shot, 0, 0),
        BACKGROUND,
        "origin is not the cursor",
    );
}

pub(super) fn selection_paints_the_selection_background() {
    let (client, view) = publish(b"\x1b[?25l\x1b[HSELECT");
    let found = client
        .engine()
        .expect("engine")
        .search_view(view, "SELECT".into(), true)
        .expect("search");
    let matched = found.first().expect("SELECT is on screen");
    client
        .engine()
        .expect("engine")
        .set_view_selection(view, matched.start, matched.end, false)
        .expect("select");
    let frame = frame_of(&client, view);
    assert_ne!(frame.cell(0, 0).expect("S").flags & CELL_SELECTED, 0);
    assert_eq!(frame.cell(0, 8).expect("gap").flags & CELL_SELECTED, 0);
    let shot = paint(client, view, settings(false));
    let selected = cell_pixels(&shot, 0, 0);
    // The glyph covers the rest of the cell, so this is lower than a solid swatch.
    assert_share(&selected, SELECTION, 0.6, "selected cell background");
    assert!(
        selected
            .iter()
            .any(|pixel| distance(*pixel, SELECTION) > 80),
        "the selected glyph must still be ink on top of the selection"
    );
    assert_share(
        &cell_pixels(&shot, 0, 5),
        SELECTION,
        0.6,
        "last selected column",
    );
    assert_blank(&cell_pixels(&shot, 0, 8), "unselected gap");
}

pub(super) fn palette_recolors_ansi_and_keeps_true_color() {
    // ANSI red background (palette index 1), then a true-color glyph.
    let (client, view) = publish(b"\x1b[?25l\x1b[H\x1b[41m \x1b[0m\x1b[38;2;0;220;30mT");
    let frame = frame_of(&client, view);
    let ansi = frame.cell(0, 0).expect("ansi cell");
    let ansi_meta = frame.buffer.metadata[0];
    assert!(!ansi_meta.background_color_is_default);
    let published = [ansi.background_r, ansi.background_g, ansi.background_b];
    assert_eq!(
        published,
        [
            frame.colors.palette[1].r,
            frame.colors.palette[1].g,
            frame.colors.palette[1].b
        ]
    );
    assert_ne!(
        published, AMBER,
        "the theme color must differ from the terminal palette or the remap is invisible"
    );
    let colored = frame.cell(0, 1).expect("true-color cell");
    assert_eq!(frame.buffer.metadata[1].foreground_kind, COLOR_KIND_RGB);
    assert_eq!(
        [
            colored.foreground_r,
            colored.foreground_g,
            colored.foreground_b
        ],
        TRUE_GREEN
    );
    let shot = paint(client, view, settings(false));
    assert_mostly(
        &cell_pixels(&shot, 0, 0),
        AMBER,
        "ANSI red takes the theme palette",
    );
    let glyph = cell_pixels(&shot, 0, 1);
    let green = glyph
        .iter()
        .filter(|pixel| near(**pixel, TRUE_GREEN))
        .count();
    assert!(
        green >= 4,
        "true color stayed green ({green} pixels), center {:?}",
        glyph[glyph.len() / 2]
    );
    // The glyph covers the rest. A true-color background fill would miss this.
    assert_share(
        &glyph,
        BACKGROUND,
        0.7,
        "true color does not repaint the background",
    );
}

pub(super) fn nerd_symbols_are_not_missing_glyph_boxes_in_any_face() {
    let mut payload = String::from("\u{1b}[?25l");
    for (face_row, face) in ["0", "1", "3", "1;3"].into_iter().enumerate() {
        for (column, icon) in ['\u{f013}', '\u{f0206}'].into_iter().enumerate() {
            // Keep each reference glyph at the same device-pixel x origin.
            // Different primary-font widths put later columns at different
            // subpixel phases on FreeType, even with the exact same font.
            // U+0378 is unassigned: some ink alone is not proof of an icon.
            payload.push_str(&format!(
                "\u{1b}[{};1H\u{1b}[0;{face}m{icon}\u{0378}",
                face_row * 2 + column + 1
            ));
        }
    }
    let (client, view) = publish_at(payload.as_bytes(), 20, 8);
    let shot = paint(Arc::clone(&client), view, settings(false));
    let mut explicit = settings(false);
    explicit.font = gpui::font(phux_desktop::fonts::SYMBOLS);
    let expected = paint(client, view, explicit);
    for row in 0..8 {
        let missing = cell_pixels(&shot, row, 1);
        let col = 0;
        let icon = cell_pixels(&shot, row, col);
        assert_ink(&icon, "Nerd Font symbol");
        assert_ne!(
            icon, missing,
            "Nerd icon column {col}, SGR face row {row} resolved to the missing-glyph box"
        );
        assert_eq!(
            ink_mask(&shot, row, col),
            ink_mask(&expected, row, col),
            "Nerd icon column {col}, SGR face row {row} must render the bundled symbol outline"
        );
    }
}

/// Compare the actual symbol outline, independent of primary-font cell width
/// and baseline. A LastResort box can encode the missing codepoint, so merely
/// differing from another missing glyph is not evidence of a rendered icon.
pub(super) fn ink_mask(shot: &Shot, row: u16, col: u16) -> (u32, u32, Vec<bool>) {
    let geometry = shot.painted.geometry;
    let bounds = geometry.cell_bounds(row, col, 1);
    let (mut left, mut top) = (u32::MAX, u32::MAX);
    let (mut right, mut bottom) = (0, 0);
    for y in device(bounds.top(), geometry.scale)..device(bounds.bottom(), geometry.scale) {
        for x in device(bounds.left(), geometry.scale)..device(bounds.right(), geometry.scale) {
            if distance(shot.image.get_pixel(x, y).0, BACKGROUND) > 80 {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }
    assert!(
        right > left && bottom > top,
        "symbol has no visible outline"
    );
    let mask = (top..bottom)
        .flat_map(|y| {
            (left..right).map(move |x| distance(shot.image.get_pixel(x, y).0, BACKGROUND) > 80)
        })
        .collect();
    (right - left, bottom - top, mask)
}

pub(super) fn solid_blocks_cover_shared_row_edges_with_extra_line_height() {
    let (client, view) = publish(
        concat!(
            "\u{1b}[?25l\u{1b}[H██\u{1b}[2;1H██\u{1b}[3;1H▀▄",
            "\u{1b}[4;1H\u{1b}[2m██",
            "\u{1b}[1;5H\u{1b}[0;4;58;2;0;220;30m█",
        )
        .as_bytes(),
    );
    let mut config = settings(false);
    config.line_height = 1.8;
    let shot = paint(client, view, config);
    let geometry = shot.painted.geometry;
    let first = geometry.cell_bounds(0, 0, 2);
    let second = geometry.cell_bounds(1, 0, 2);
    let left = device(first.left(), geometry.scale);
    let right = device(first.right(), geometry.scale);
    let top = device(first.top(), geometry.scale);
    let bottom = device(second.bottom(), geometry.scale);
    for y in top..bottom {
        for x in left..right {
            assert!(
                near(shot.image.get_pixel(x, y).0, FOREGROUND),
                "solid block seam at device pixel ({x}, {y})"
            );
        }
    }
    for (col, upper) in [(0, true), (1, false)] {
        let bounds = geometry.cell_bounds(2, col, 1);
        let x = device(bounds.center().x, geometry.scale);
        for (fraction, filled) in [(0.25, upper), (0.75, !upper)] {
            let y = device(bounds.top() + bounds.size.height * fraction, geometry.scale);
            let expected = if filled { FOREGROUND } else { BACKGROUND };
            assert!(near(shot.image.get_pixel(x, y).0, expected));
        }
    }
    let faint = geometry.cell_bounds(3, 0, 2);
    let center = shot
        .image
        .get_pixel(
            device(faint.center().x, geometry.scale),
            device(faint.center().y, geometry.scale),
        )
        .0;
    assert!(
        !near(center, FOREGROUND) && !near(center, BACKGROUND),
        "faint block was not dimmed"
    );
    let expected = [center[0], center[1], center[2]];
    for y in device(faint.top(), geometry.scale)..device(faint.bottom(), geometry.scale) {
        for x in device(faint.left(), geometry.scale)..device(faint.right(), geometry.scale) {
            assert!(
                near(shot.image.get_pixel(x, y).0, expected),
                "translucent block seam"
            );
        }
    }
    let decorated = geometry.cell_bounds(0, 4, 1);
    let underline_y = device(decorated.bottom(), geometry.scale) - 2;
    assert!(
        near(
            shot.image
                .get_pixel(device(decorated.center().x, geometry.scale), underline_y,)
                .0,
            TRUE_GREEN,
        ),
        "colored underline must remain above solid block ink"
    );
}
