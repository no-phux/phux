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

/// Everything a terminal UI draws, for eyes: `PHUX_PIXELS_DUMP=gallery.png
/// cargo test --test pixels -- --ignored text_gallery`.
pub(super) fn text_gallery() {
    let rows = [
        "\u{23fa} \u{1b}[1mBash\u{1b}[0m(ls)  \u{2722}\u{2733}\u{2736}\u{273b}\u{273d}\u{00b7} Gitifying\u{2026} \u{2191}\u{2193}\u{2190}\u{2192} \u{2713}\u{2717}\u{2714}\u{2718} \u{25b6}\u{25b8}\u{276f}\u{203a}",
        "  \u{23bf}  Shell cwd was reset   \u{23bf}\u{23be}\u{23b8}\u{23b9} \u{2500}\u{2500}\u{2500}\u{2500} \u{2026} +10 lines",
        "\u{256d}\u{2500}\u{2500}\u{2500}\u{256e} \u{250c}\u{2500}\u{252c}\u{2500}\u{2510} \u{250f}\u{2501}\u{2533}\u{2501}\u{2513} \u{2554}\u{2550}\u{2566}\u{2550}\u{2557} \u{2504}\u{2508}\u{254c} \u{2571}\u{2572}\u{2573}",
        "\u{2502}abc\u{2502} \u{251c}\u{2500}\u{253c}\u{2500}\u{2524} \u{2523}\u{2501}\u{254b}\u{2501}\u{252b} \u{2560}\u{2550}\u{256c}\u{2550}\u{2563} \u{2506}\u{250a}\u{254e} \u{2574}\u{2575}\u{2576}\u{2577}",
        "\u{2570}\u{2500}\u{2500}\u{2500}\u{256f} \u{2514}\u{2500}\u{2534}\u{2500}\u{2518} \u{2517}\u{2501}\u{253b}\u{2501}\u{251b} \u{255a}\u{2550}\u{2569}\u{2550}\u{255d} \u{2591}\u{2592}\u{2593}\u{2588}\u{2580}\u{2584}\u{258c}\u{2590}",
        "\u{28ff}\u{2847}\u{28b8}\u{2809}\u{28c0} \u{e0b0}\u{e0b2}\u{e0b4}\u{e0b6} \u{f013}\u{f07b} \u{1b}[1mbold\u{1b}[0m \u{1b}[3mitalic\u{1b}[0m \u{1b}[1;3mboth\u{1b}[0m \u{1b}[4munder\u{1b}[0m \u{1b}[9mstrike\u{1b}[0m",
        "\u{4e2d}\u{6587}\u{5b57} \u{1f600}\u{1f680} e\u{301} n\u{303} \u{2192}\u{21d2}\u{27f6} \u{2264}\u{2265}\u{2260} \u{00bd} \u{2122} \u{00a9} \u{2022}\u{25e6}\u{25aa}\u{25cb}\u{25cf}\u{25c9}",
        "\u{2581}\u{2582}\u{2583}\u{2584}\u{2585}\u{2586}\u{2587}\u{2588} \u{2596}\u{2597}\u{2598}\u{259d} \u{1fb00}\u{1fb01}\u{1fb02} \u{25e2}\u{25e3}\u{25e4}\u{25e5} \u{2580}\u{2580}\u{2584}\u{2584}",
    ];
    let mut payload = String::from("\u{1b}[?25l");
    for (row, text) in rows.iter().enumerate() {
        payload.push_str(&format!("\u{1b}[{};1H{text}", row + 1));
    }
    let (client, view) = publish_at(payload.as_bytes(), 60, rows.len() as u16);
    // The owner's Ghostty config: 15 pt with adjust-cell-height = 12%.
    let mut look = settings(false);
    if std::env::var("PHUX_GALLERY_OWNER").is_ok() {
        look.font_size = 15.;
        look.cell_height_scale = 1.12;
    }
    let shot = paint_sized(client, view, look, size(px(640.), px(200.)));
    let path = std::env::var("PHUX_PIXELS_DUMP").unwrap_or_else(|_| "gallery.png".into());
    shot.image.save(&path).expect("dump");
}

/// With taller cells than the face's line height, drawn lines still join
/// across rows and Powerline separators still fill their cell.
pub(super) fn lines_join_across_tall_rows() {
    let payload = "\u{1b}[?25l\u{1b}[H\
        \u{256d}\u{2500}\u{2556}\u{e0b0}\r\n\
        \u{2502} \u{2551}\u{e0b0}\r\n\
        \u{2570}\u{2500}\u{255c}\u{e0b0}";
    let (client, view) = publish_at(payload.as_bytes(), 10, 3);
    let mut look = settings(false);
    look.cell_height_scale = 1.3;
    let shot = paint(client, view, look);
    let geometry = shot.painted.geometry;
    let inked = |x: u32, y: u32| distance(shot.image.get_pixel(x, y).0, BACKGROUND) > 80;
    // The left border and the double line run unbroken across both row
    // boundaries (past the corners' curves).
    for col in [0, 2] {
        let mid = geometry.cell_bounds(1, col, 1);
        let top = device(mid.top(), geometry.scale) - 3;
        let bottom = device(mid.bottom(), geometry.scale) + 3;
        let stem = (device(mid.left(), geometry.scale)..device(mid.right(), geometry.scale))
            .find(|x| (top..bottom).all(|y| inked(*x, y)));
        assert!(stem.is_some(), "column {col} breaks between rows");
    }
    // A Powerline arrow's base spans its cell's full height in every row.
    for row in 0..3 {
        let cell = geometry.cell_bounds(row, 3, 1);
        let x = device(cell.left(), geometry.scale) + 1;
        for y in device(cell.top(), geometry.scale) + 1..device(cell.bottom(), geometry.scale) - 1 {
            assert!(inked(x, y), "row {row} separator gap at y={y}");
        }
    }
}
