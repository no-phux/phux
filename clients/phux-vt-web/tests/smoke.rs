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
