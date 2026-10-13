//! DPR regression tests in a real browser; no server or production socket.

use phux_vt_web::Vt;
use phux_web::canvas::{CanvasGeometry, paint_pane_border, publish_metrics};
use phux_web::{Metrics, Overlay, PaneRect, render, render_cursor_row};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

wasm_bindgen_test_configure!(run_in_browser);

fn canvas() -> (HtmlCanvasElement, CanvasRenderingContext2d) {
    let document = web_sys::window().unwrap().document().unwrap();
    let canvas: HtmlCanvasElement = document
        .create_element("canvas")
        .unwrap()
        .dyn_into()
        .unwrap();
    let ctx = canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    (canvas, ctx)
}

fn metrics() -> Metrics {
    Metrics {
        cell_w: 9.0,
        cell_h: 16.0,
        font: "14px monospace".into(),
    }
}

fn pixel(ctx: &CanvasRenderingContext2d, x: i32, y: i32) -> Vec<u8> {
    ctx.get_image_data(x, y, 1, 1).unwrap().data().to_vec()
}

#[wasm_bindgen_test]
fn dpr_backing_store_keeps_css_extent_and_measured_dataset() {
    let (canvas, ctx) = canvas();
    let metrics = metrics();
    publish_metrics(&canvas, &metrics).unwrap();
    let geometry = CanvasGeometry::new(&metrics, 96, 24, 2.0);
    geometry.prepare(&canvas, &ctx).unwrap();
    assert_eq!((canvas.width(), canvas.height()), (1728, 768));
    assert_eq!(canvas.style().get_property_value("width").unwrap(), "864px");
    assert_eq!(
        canvas.style().get_property_value("height").unwrap(),
        "384px"
    );
    assert_eq!(
        canvas.get_attribute("data-phux-cell-width").as_deref(),
        Some("9")
    );
    assert_eq!(
        canvas.get_attribute("data-phux-cell-height").as_deref(),
        Some("16")
    );
    assert_eq!(metrics.cell_px(), (9, 16), "wire cells stay logical");
    assert!(
        geometry.width <= 872.0,
        "96 measured cells fit the observed dialog"
    );
}

fn red_rect(ctx: &CanvasRenderingContext2d) {
    ctx.set_fill_style_str("#ff0000");
    ctx.fill_rect(9.0, 16.0, 9.0, 16.0);
}

#[wasm_bindgen_test]
fn repeated_prepare_and_one_axis_resize_do_not_compound_transforms() {
    let (canvas, ctx) = canvas();
    let metrics = metrics();
    let geometry = CanvasGeometry::new(&metrics, 4, 3, 2.0);
    geometry.prepare(&canvas, &ctx).unwrap();
    ctx.translate(13.0, 17.0).unwrap();
    geometry.prepare(&canvas, &ctx).unwrap();
    red_rect(&ctx);
    assert_eq!(pixel(&ctx, 19, 33), vec![255, 0, 0, 255]);
    CanvasGeometry::new(&metrics, 5, 3, 2.0)
        .prepare(&canvas, &ctx)
        .unwrap();
    red_rect(&ctx);
    assert_eq!(pixel(&ctx, 19, 33), vec![255, 0, 0, 255]);
    CanvasGeometry::new(&metrics, 5, 4, 1.0)
        .prepare(&canvas, &ctx)
        .unwrap();
    red_rect(&ctx);
    assert_eq!(pixel(&ctx, 10, 17), vec![255, 0, 0, 255]);
    assert_eq!(pixel(&ctx, 28, 33), vec![0, 0, 0, 0]);
}

#[wasm_bindgen_test]
fn css_hit_testing_and_wire_pixels_do_not_depend_on_dpr() {
    let metrics = metrics();
    for dpr in [1.0, 1.25, 2.0, 3.0] {
        let geometry = CanvasGeometry::new(&metrics, 80, 24, dpr);
        // Deliberately CSS-scaled: selection and input must share this scale.
        let (w, h) = geometry.css_cell(&metrics, 360.0, 192.0);
        assert_eq!((w, h), (4.5, 8.0));
        let pane = PaneRect {
            x: 41,
            y: 0,
            cols: 39,
            rows: 24,
        };
        let x = 47.5 * w - f64::from(pane.x) * w;
        assert_eq!(
            phux_web::selection::cell_at(x, 3.5 * h, w, h, pane.cols, pane.rows),
            (6, 3)
        );
        assert_eq!(phux_web::input::surface_pixel(x, w, 9, pane.cols), 58.0);
    }
}

#[wasm_bindgen_test]
async fn cursor_row_and_translated_pane_render_in_logical_coordinates() {
    let vt = Vt::load().await.unwrap();
    let term = vt.terminal(4, 2);
    term.write(b"\x1b[48;2;255;0;0m \x1b[0m\x1b[1;1H");
    let grid = term.grid();
    let (canvas, ctx) = canvas();
    let metrics = metrics();
    CanvasGeometry::new(&metrics, 9, 2, 2.0)
        .prepare(&canvas, &ctx)
        .unwrap();
    ctx.save();
    ctx.translate(5.0 * metrics.cell_w, 0.0).unwrap();
    render(&ctx, &grid, &metrics, true);
    render_cursor_row(&ctx, &grid, &metrics, false, &Overlay::default());
    ctx.restore();
    assert_eq!(pixel(&ctx, 93, 3), vec![255, 0, 0, 255]);
    assert_eq!(pixel(&ctx, 3, 3), vec![0, 0, 0, 0]);
}

#[wasm_bindgen_test]
fn pane_borders_are_one_logical_pixel_and_neutral() {
    let (canvas, ctx) = canvas();
    let metrics = metrics();
    CanvasGeometry::new(&metrics, 4, 2, 2.0)
        .prepare(&canvas, &ctx)
        .unwrap();
    let rect = PaneRect {
        x: 0,
        y: 0,
        cols: 4,
        rows: 2,
    };
    paint_pane_border(&ctx, &metrics, &rect, true);
    assert_eq!(pixel(&ctx, 20, 0), vec![91, 91, 101, 255]);
    assert_eq!(pixel(&ctx, 20, 1), vec![91, 91, 101, 255]);
    assert_eq!(pixel(&ctx, 20, 2), vec![0, 0, 0, 0]);
    paint_pane_border(&ctx, &metrics, &rect, false);
    assert_eq!(pixel(&ctx, 20, 0), vec![39, 39, 45, 255]);
}

#[wasm_bindgen_test]
fn invalid_dpr_falls_back_to_logical_resolution() {
    let metrics = metrics();
    for dpr in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(CanvasGeometry::new(&metrics, 4, 2, dpr).dpr, 1.0);
    }
}
