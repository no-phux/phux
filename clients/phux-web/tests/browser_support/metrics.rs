//! Real loaded-font geometry shared by browser test pointer and pixel probes.
use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

pub fn measured(canvas: &HtmlCanvasElement) -> phux_web::Metrics {
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    phux_web::Metrics::measure(&ctx).unwrap()
}
