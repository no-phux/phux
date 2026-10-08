//! In-browser render test (headless Chrome): drive the real ghostty-vt engine,
//! read the styled grid, paint it to a real canvas, and read a pixel back.

use phux_vt_web::{Rgb, Vt};
use phux_web::{Metrics, Overlay, render, render_cursor_row};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn bundled_font_is_loaded_before_cell_measurement() {
    phux_web::load_terminal_font()
        .await
        .expect("load bundled Paper Mono");
    // A second load must reuse the face, including on reconnect.
    phux_web::load_terminal_font()
        .await
        .expect("reuse bundled Paper Mono");
    let document = web_sys::window().unwrap().document().unwrap();
    assert!(document.fonts().check("14px \"Paper Mono\"").unwrap());
    let canvas: HtmlCanvasElement = document
        .create_element("canvas")
        .unwrap()
        .dyn_into()
        .unwrap();
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    let metrics = Metrics::measure(&ctx).unwrap();
    assert!(metrics.font.contains("Paper Mono"));
    let narrow = ctx.measure_text("iiii").unwrap().width();
    let wide = ctx.measure_text("WWWW").unwrap().width();
    assert!(
        (narrow - wide).abs() < 0.01,
        "bundled face must be fixed-pitch"
    );
    assert_eq!(
        metrics.cell_w,
        ctx.measure_text("M").unwrap().width().ceil()
    );
    assert!(metrics.cell_h >= 16.0);
}

#[wasm_bindgen_test]
async fn renders_engine_grid_to_canvas() {
    let vt = Vt::load().await.expect("load ghostty-vt engine");
    let term = vt.terminal(4, 2);
    // A red-background space at cell (0,0).
    term.write(b"\x1b[48;2;255;0;0m \x1b[0m");
    let grid = term.grid();

    let document = web_sys::window().unwrap().document().unwrap();
    let canvas: HtmlCanvasElement = document
        .create_element("canvas")
        .unwrap()
        .dyn_into()
        .unwrap();
    canvas.set_width(64);
    canvas.set_height(64);
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();

    let m = Metrics {
        cell_w: 10.0,
        cell_h: 16.0,
        font: "14px monospace".to_owned(),
    };
    render(&ctx, &grid, &m, false);

    // Sample a pixel inside cell (0,0): it should be the red background.
    let pixel = ctx.get_image_data(3, 3, 1, 1).unwrap();
    let d = pixel.data();
    assert!(
        d[0] > 200 && d[1] < 60 && d[2] < 60,
        "cell(0,0) should render red bg; got rgba({},{},{},{})",
        d[0],
        d[1],
        d[2],
        d[3],
    );
}

fn blank_canvas(width: u32, height: u32) -> CanvasRenderingContext2d {
    let document = web_sys::window().unwrap().document().unwrap();
    let canvas: HtmlCanvasElement = document
        .create_element("canvas")
        .unwrap()
        .dyn_into()
        .unwrap();
    canvas.set_width(width);
    canvas.set_height(height);
    canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap()
}

/// Pixels of the box at `(x, y)` sized `(width, height)` that are close
/// to `color`.
fn near(
    ctx: &CanvasRenderingContext2d,
    (x, y): (i32, i32),
    (width, height): (i32, i32),
    color: Rgb,
) -> usize {
    let data = ctx.get_image_data(x, y, width, height).unwrap().data();
    data.chunks(4)
        .filter(|px| {
            let close = |a: u8, b: u8| a.abs_diff(b) < 48;
            close(px[0], color.r) && close(px[1], color.g) && close(px[2], color.b)
        })
        .count()
}

/// A double-width (CJK) glyph spans its own cell and the spacer cell after
/// it. With cells 4 px wide, a 14 px font's glyph for it (or its
/// missing-glyph box, whatever fonts the browser has) always reaches into
/// the spacer, so the right half shows only if the spacer's background did
/// not paint over it: on a plain paint, beside the cursor, and on a blink.
#[wasm_bindgen_test]
async fn a_wide_character_paints_across_its_spacer_cell() {
    let vt = Vt::load().await.expect("load ghostty-vt engine");
    let term = vt.terminal(4, 1);
    term.write("\u{4e2d}".as_bytes());
    let m = Metrics {
        cell_w: 4.0,
        cell_h: 16.0,
        font: "14px monospace".to_owned(),
    };
    let ctx = blank_canvas(16, 16);
    let (spacer, size) = ((4, 0), (4, 16));

    let grid = term.grid();
    let fg = grid.default_fg;
    render(&ctx, &grid, &m, false);
    assert!(near(&ctx, spacer, size, fg) > 0, "right half painted");

    // The cursor on the wide character inverts its own cell and leaves the
    // glyph's right half drawn in the foreground.
    term.write(b"\r");
    let grid = term.grid();
    assert_eq!((grid.cursor_col, grid.cursor_row), (0, 0));
    render(&ctx, &grid, &m, true);
    assert!(
        near(&ctx, spacer, size, fg) > 0,
        "right half beside the cursor"
    );

    // A blink redraws the cursor's row in both phases.
    for cursor_on in [false, true, false] {
        render_cursor_row(&ctx, &grid, &m, cursor_on, &Overlay::default());
        assert!(
            near(&ctx, spacer, size, fg) > 0,
            "right half after a blink (cursor {cursor_on})"
        );
    }
}
