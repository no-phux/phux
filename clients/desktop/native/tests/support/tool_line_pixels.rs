//! Claude Code's tool-call lines: `⏺` heads a call and `⎿` hangs its result
//! under it. Neither is in the bundled faces, so they must still sit on the
//! cell grid like the box drawing they imitate.

use super::*;

/// Ink extents inside one cell, in device pixels from its top-left, with the
/// cell's own device size: (left, top, right, bottom, width, height).
fn ink_in_cell(shot: &Shot, row: u16, col: u16) -> (u32, u32, u32, u32, u32, u32) {
    let geometry = shot.painted.geometry;
    let bounds = geometry.cell_bounds(row, col, 1);
    let (x0, y0) = (
        device(bounds.left(), geometry.scale),
        device(bounds.top(), geometry.scale),
    );
    let (x1, y1) = (
        device(bounds.right(), geometry.scale),
        device(bounds.bottom(), geometry.scale),
    );
    let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
    for y in y0..y1 {
        for x in x0..x1 {
            if distance(shot.image.get_pixel(x, y).0, BACKGROUND) > 80 {
                left = left.min(x - x0);
                top = top.min(y - y0);
                right = right.max(x - x0 + 1);
                bottom = bottom.max(y - y0 + 1);
            }
        }
    }
    assert!(right > left, "cell {row},{col} has no ink");
    (left, top, right, bottom, x1 - x0, y1 - y0)
}

pub(super) fn claude_code_tool_lines_sit_on_the_grid() {
    let payload = "\u{1b}[?25l\u{1b}[H\
        \u{23fa} \u{1b}[1mBash\u{1b}[0m(ls -la)\r\n\
        \u{20}\u{20}\u{23bf}  Shell cwd was reset\r\n\
        \u{20}\u{20}\u{23bf}  Updated a.rs (+3 -3)\r\n\
        \u{20}\u{20}\u{20}\u{20} \u{2026} +10 lines\r\n\
        \u{25cf} \u{2514}  reference";
    let (client, view) = publish_at(payload.as_bytes(), 30, 6);
    let shot = paint(client, view, settings(false));
    if let Ok(path) = std::env::var("PHUX_PIXELS_DUMP") {
        shot.image.save(&path).expect("dump");
    }
    // `⎿` is `└` hung from the row above: its stem leaves the cell's top
    // edge and its arm reaches the right edge, like the font's own `└`.
    let corner = ink_in_cell(&shot, 1, 2);
    let reference = ink_in_cell(&shot, 4, 2);
    assert_eq!(corner.1, 0, "⎿ stem reaches the cell top: {corner:?}");
    assert_eq!(
        corner.2, corner.4,
        "⎿ arm reaches the cell's right edge: {corner:?}"
    );
    assert_eq!(
        corner, reference,
        "⎿ matches the font's └ (left, top, right, bottom, w, h)"
    );
    // `⏺` is the font's `●`: the same ink, centred in its cell.
    let bullet = ink_in_cell(&shot, 0, 0);
    let dot = ink_in_cell(&shot, 4, 0);
    assert_eq!(
        bullet, dot,
        "⏺ matches the font's ● (left, top, right, bottom, w, h)"
    );
}

/// Every cell of a run holds its glyph at the same offset, wherever the run
/// starts: no sawtooth where a face's advance differs from the cell width.
pub(super) fn repeated_glyphs_keep_uniform_spacing() {
    let mut payload = String::from("\u{1b}[?25l\u{1b}[H");
    for (row, glyph) in ["a", "\u{2514}", "\u{00b7}"].iter().enumerate() {
        for lead in 0..3 {
            payload.push_str(&format!(
                "\u{1b}[{};{}H{}",
                row * 3 + lead + 1,
                lead + 1,
                glyph.repeat(24)
            ));
        }
    }
    let (client, view) = publish_at(payload.as_bytes(), 30, 9);
    let shot = paint(client, view, settings(false));
    if let Ok(path) = std::env::var("PHUX_PIXELS_DUMP") {
        shot.image.save(&path).expect("dump");
    }
    for row in 0..9u16 {
        let lead = row % 3;
        let first = ink_in_cell(&shot, row, lead);
        for col in lead + 1..lead + 24 {
            assert_eq!(
                ink_in_cell(&shot, row, col),
                first,
                "row {row} column {col} drifts from column {lead}"
            );
        }
    }
}
