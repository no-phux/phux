//! Drives the real ghostty-vt.wasm engine through the Rust driver under node:
//! create a terminal, feed VT bytes, read the grid back as text.

use phux_vt_web::{Rgb, Vt};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn engine_wasm_is_embedded() {
    assert!(phux_vt_web::engine_wasm_len() > 1_000_000);
}

#[wasm_bindgen_test]
async fn writes_and_reads_back_text() {
    let vt = Vt::load().await.expect("load ghostty-vt engine");
    let term = vt.terminal(20, 5);

    term.write(b"Hello, phux");
    let rows = term.rows_text();

    assert!(!rows.is_empty(), "expected rows, got none");
    assert!(
        rows[0].starts_with("Hello, phux"),
        "row 0 should contain the written text; got {:?}",
        rows[0],
    );
}

#[wasm_bindgen_test]
async fn handles_cursor_movement_and_overwrite() {
    let vt = Vt::load().await.expect("load ghostty-vt engine");
    let term = vt.terminal(20, 5);

    // Write, carriage-return to column 0, overwrite the first char.
    term.write(b"world\r");
    term.write(b"W");
    let rows = term.rows_text();
    assert_eq!(rows[0], "World", "CR + overwrite; got {:?}", rows[0]);
}

#[wasm_bindgen_test]
async fn reads_truecolor_grid() {
    let vt = Vt::load().await.expect("load ghostty-vt engine");
    let term = vt.terminal(20, 3);

    // A red "R" via SGR truecolor, then reset.
    term.write(b"\x1b[38;2;255;0;0mR\x1b[0m");
    let grid = term.grid();

    assert_eq!(grid.cols, 20, "cols");
    assert_eq!(grid.rows, 3, "rows");
    assert_eq!(grid.cells.len(), 60, "rectangular cols*rows");

    let cell0 = &grid.cells[0];
    assert_eq!(cell0.ch, 'R', "first cell char");
    assert_eq!(
        cell0.fg,
        Some(Rgb { r: 255, g: 0, b: 0 }),
        "first cell fg = {:?}",
        cell0.fg,
    );
}

#[wasm_bindgen_test]
async fn osc_title_reads_back() {
    let vt = Vt::load().await.expect("load ghostty-vt engine");
    let term = vt.terminal(20, 3);
    assert_eq!(term.title(), "", "no title yet");
    term.write(b"\x1b]2;build: ok \xe2\x9c\x93\x07");
    assert_eq!(term.title(), "build: ok \u{2713}");
    term.write(b"\x1b]0;second\x1b\\");
    assert_eq!(term.title(), "second", "OSC 0 with an ST terminator");
}

#[wasm_bindgen_test]
async fn viewport_scrolls_into_scrollback_and_back() {
    let vt = Vt::load().await.expect("load ghostty-vt engine");
    let term = vt.terminal(20, 5);
    for line in 0..30 {
        term.write(format!("line {line}\r\n").as_bytes());
    }
    let live = term.rows_text();
    assert_eq!(live[0], "line 26", "active area: {live:?}");
    assert!(!term.viewport_scrolled());

    term.scroll_viewport(-10);
    let scrolled = term.rows_text();
    assert_eq!(scrolled[0], "line 16", "ten rows up: {scrolled:?}");
    assert!(term.viewport_scrolled());

    term.scroll_viewport(-1_000);
    assert_eq!(term.rows_text()[0], "line 0", "clamped at the top");

    term.write(b"more\r\n");
    assert_eq!(
        term.rows_text()[0],
        "line 0",
        "output does not yank a scrolled viewport"
    );

    term.scroll_to_bottom();
    assert!(!term.viewport_scrolled());
    assert_eq!(term.rows_text()[3], "more", "back on the active area");
}

#[wasm_bindgen_test]
async fn a_bel_rings_once_and_a_bel_terminated_osc_does_not() {
    let vt = Vt::load().await.expect("load ghostty-vt engine");
    let term = vt.terminal(20, 3);
    let other = vt.terminal(20, 3);
    assert!(!term.take_bell(), "no bell yet");
    term.write(b"\x1b]2;title\x07text");
    assert!(!term.take_bell(), "BEL terminating an OSC is not a bell");
    term.write(b"ding\x07\x07");
    assert!(term.take_bell(), "a BEL rings");
    assert!(!term.take_bell(), "taking the bell clears it");
    assert!(!other.take_bell(), "bells belong to the terminal that rang");
}

#[wasm_bindgen_test]
async fn screen_rows_cover_history_and_the_active_area() {
    let vt = Vt::load().await.expect("load ghostty-vt engine");
    let term = vt.terminal(10, 3);
    term.write("one\r\n\r\n\u{4e2d}\u{6587}ab\r\nwrapwrapwrapXY\r\nlast".as_bytes());
    let rows = term.screen_rows();
    assert_eq!(
        rows,
        [
            "one",
            "",
            "\u{4e2d}\u{6587}ab",
            "wrapwrapwr",
            "apXY",
            "last"
        ],
        "one entry per screen row, oldest first"
    );
    let bar = term.scrollbar();
    assert_eq!((bar.total, bar.offset, bar.len), (6, 3, 3));
    term.scroll_to_row(1);
    assert_eq!(term.rows_text()[0], "", "row 1 is now the viewport's top");
    assert_eq!(term.scrollbar().offset, 1);
    term.scroll_to_row(1_000);
    assert_eq!(term.scrollbar().offset, 3, "clamped to the live screen");
    assert_eq!(vt.codepoint_width('\u{4e2d}'), 2);
    assert_eq!(vt.codepoint_width('a'), 1);
    assert_eq!(vt.codepoint_width('\u{301}'), 0);
}

#[wasm_bindgen_test]
async fn selection_text_is_the_engines_copy_without_wide_spacers() {
    let vt = Vt::load().await.expect("load ghostty-vt engine");
    let term = vt.terminal(10, 5);
    term.write("\u{4e2d}\u{6587}ab\r\n$ echo hi \r\nwrapwrapwrapXY".as_bytes());
    assert_eq!(
        term.selection_text((0, 0), (9, 0)).as_deref(),
        Some("\u{4e2d}\u{6587}ab"),
        "a double-width character copies without its spacer cell"
    );
    assert_eq!(
        term.selection_text((5, 1), (0, 0)).as_deref(),
        Some("\u{4e2d}\u{6587}ab\n$ echo"),
        "either direction, one line per row"
    );
    assert_eq!(
        term.selection_text((0, 2), (3, 3)).as_deref(),
        Some("wrapwrapwrapXY"),
        "a soft wrap copies as one line"
    );
}

#[wasm_bindgen_test]
async fn osc8_hyperlinks_and_mouse_modes_read_back() {
    let vt = Vt::load().await.expect("load ghostty-vt engine");
    let term = vt.terminal(20, 3);
    term.write(b"go \x1b]8;;https://example.com/a\x1b\\LINK\x1b]8;;\x1b\\ end");
    assert_eq!(
        term.hyperlink_at(3, 0).as_deref(),
        Some("https://example.com/a")
    );
    assert_eq!(
        term.hyperlink_at(6, 0).as_deref(),
        Some("https://example.com/a")
    );
    assert_eq!(term.hyperlink_at(2, 0), None, "before the link");
    assert_eq!(term.hyperlink_at(8, 0), None, "after the link");
    assert_eq!(term.hyperlink_at(50, 50), None, "outside the grid");

    assert!(!term.mouse_tracking());
    term.write(b"\x1b[?1002h\x1b[?1006h");
    assert!(term.mouse_tracking());
    assert!(term.dec_mode(1002));
    assert!(!term.dec_mode(1003));
    term.write(b"\x1b[?1002l");
    assert!(!term.mouse_tracking());
}
