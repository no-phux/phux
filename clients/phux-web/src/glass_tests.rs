//! Actual-engine pixel checks for the opt-in glass appearance.
#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
use super::*;
use phux_vt_web::Vt;
use wasm_bindgen_test::wasm_bindgen_test;

fn surface() -> (CanvasRenderingContext2d, Metrics) {
    let canvas: web_sys::HtmlCanvasElement = web_sys::window()
        .unwrap()
        .document()
        .unwrap()
        .create_element("canvas")
        .unwrap()
        .dyn_into()
        .unwrap();
    canvas.set_width(60);
    canvas.set_height(40);
    let context = canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    (
        context,
        Metrics {
            cell_w: 10.0,
            cell_h: 20.0,
            font: "14px monospace".into(),
        },
    )
}
fn pixel(ctx: &CanvasRenderingContext2d, col: i32) -> Vec<u8> {
    ctx.get_image_data(col * 10 + 2, 2, 1, 1)
        .unwrap()
        .data()
        .to_vec()
}
fn row_pixels(ctx: &CanvasRenderingContext2d) -> Vec<u8> {
    ctx.get_image_data(0, 0, 50, 20).unwrap().data().to_vec()
}

#[wasm_bindgen_test]
async fn glass_keeps_explicit_ansi_black_and_inverse_but_clears_default_cells() {
    let vt = Vt::load().await.unwrap();
    let term = vt.terminal(5, 2);
    term.write(b"\x1b[48;2;255;0;0m \x1b[0m \x1b[7m \x1b[0m \x1b[48;2;0;0;0m ");
    let grid = term.grid();
    assert!(grid.cells[2].inverse, "real C ABI retains inverse flag");
    assert!(!grid.cells[1].inverse);
    let (ctx, metrics) = surface();
    render_selected_appearance(&ctx, &grid, &metrics, false, &Overlay::default(), true);
    assert_eq!(pixel(&ctx, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&ctx, 1), [0, 0, 0, 0]);
    assert_eq!(
        pixel(&ctx, 2),
        [grid.default_fg.r, grid.default_fg.g, grid.default_fg.b, 255]
    );
    assert_eq!(pixel(&ctx, 3), [0, 0, 0, 0]);
    assert_eq!(pixel(&ctx, 4), [0, 0, 0, 255]);
    // Public render_selected retains its historical opaque defaults.
    render_selected(&ctx, &grid, &metrics, false, &Overlay::default());
    assert_eq!(
        pixel(&ctx, 1),
        [grid.default_bg.r, grid.default_bg.g, grid.default_bg.b, 255]
    );
}

#[wasm_bindgen_test]
async fn glass_preserves_explicit_background_even_when_rgb_equals_terminal_default() {
    let vt = Vt::load().await.unwrap();
    let term = vt.terminal(5, 2);
    let bg = term.grid().default_bg;
    term.write(format!("\x1b[48;2;{};{};{}m \x1b[0m ", bg.r, bg.g, bg.b).as_bytes());
    let grid = term.grid();
    assert!(grid.cells[0].bg.is_some());
    let (ctx, metrics) = surface();
    render_selected_appearance(&ctx, &grid, &metrics, false, &Overlay::default(), true);
    assert_eq!(pixel(&ctx, 0), [bg.r, bg.g, bg.b, 255]);
    assert_eq!(pixel(&ctx, 1), [0, 0, 0, 0]);
}

#[wasm_bindgen_test]
async fn glass_selection_search_and_their_removal_keep_correct_alpha() {
    let vt = Vt::load().await.unwrap();
    let term = vt.terminal(5, 2);
    let grid = term.grid();
    let (ctx, metrics) = surface();
    let marks = [Mark {
        cells: 1..2,
        current: true,
    }];
    let overlay = Overlay {
        selected: 0..1,
        marks: &marks,
    };
    render_selected_appearance(&ctx, &grid, &metrics, false, &overlay, true);
    assert_eq!(
        pixel(&ctx, 0),
        [grid.default_fg.r, grid.default_fg.g, grid.default_fg.b, 255]
    );
    assert_eq!(
        pixel(&ctx, 1),
        [CURRENT_MARK_BG.r, CURRENT_MARK_BG.g, CURRENT_MARK_BG.b, 255]
    );
    assert_eq!(pixel(&ctx, 2), [0, 0, 0, 0]);
    render_selected_appearance(&ctx, &grid, &metrics, false, &Overlay::default(), true);
    assert_eq!(pixel(&ctx, 0), [0, 0, 0, 0]);
    assert_eq!(pixel(&ctx, 1), [0, 0, 0, 0]);
}

#[wasm_bindgen_test]
async fn glass_cursor_row_clears_blink_without_wide_glyph_trails() {
    let vt = Vt::load().await.unwrap();
    let term = vt.terminal(5, 2);
    term.write("中\x1b[1;2H".as_bytes());
    let grid = term.grid();
    let (ctx, metrics) = surface();
    render_selected_appearance(&ctx, &grid, &metrics, false, &Overlay::default(), true);
    let before = row_pixels(&ctx);
    render_cursor_row_appearance(&ctx, &grid, &metrics, true, &Overlay::default(), true);
    assert_ne!(row_pixels(&ctx), before, "cursor is actually painted");
    for _ in 0..4 {
        render_cursor_row_appearance(&ctx, &grid, &metrics, false, &Overlay::default(), true);
        assert_eq!(
            row_pixels(&ctx),
            before,
            "cursor off restores glyph and alpha exactly"
        );
        render_cursor_row_appearance(&ctx, &grid, &metrics, true, &Overlay::default(), true);
    }
    term.write(b"\x1b[2J\x1b[H");
    render_selected_appearance(
        &ctx,
        &term.grid(),
        &metrics,
        false,
        &Overlay::default(),
        true,
    );
    assert!(
        row_pixels(&ctx).iter().all(|channel| *channel == 0),
        "erase leaves no previous glyph/cursor pixels"
    );
}
