//! Logical terminal geometry, independent of the canvas's device-pixel backing store.

use wasm_bindgen::JsValue;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

use crate::Metrics;

/// The painted canvas extent in CSS pixels, not backing-store pixels.
#[derive(Clone, Copy, Debug)]
pub struct CanvasGeometry {
    /// Logical width.
    pub width: f64,
    /// Logical height.
    pub height: f64,
    /// Device pixel ratio used for the backing store.
    pub dpr: f64,
}

impl CanvasGeometry {
    /// Compute a logical extent without changing the terminal's cell coordinates.
    #[must_use]
    pub fn new(metrics: &Metrics, cols: u16, rows: u16, dpr: f64) -> Self {
        Self {
            width: f64::from(cols) * metrics.cell_w,
            height: f64::from(rows) * metrics.cell_h,
            dpr: valid_dpr(dpr),
        }
    }

    /// The displayed cell size, including any CSS scaling of the logical canvas.
    #[must_use]
    pub fn css_cell(&self, metrics: &Metrics, width: f64, height: f64) -> (f64, f64) {
        (
            metrics.cell_w * width / self.width.max(1.0),
            metrics.cell_h * height / self.height.max(1.0),
        )
    }

    /// Size the backing store and reset its transform (never compound scaling).
    /// CSS dimensions remain logical; no grid or wire coordinates change.
    ///
    /// # Errors
    /// Returns DOM style or canvas transform errors.
    pub fn prepare(
        &self,
        canvas: &HtmlCanvasElement,
        ctx: &CanvasRenderingContext2d,
    ) -> Result<(), JsValue> {
        let width = (self.width * self.dpr).round().max(1.0) as u32;
        let height = (self.height * self.dpr).round().max(1.0) as u32;
        if canvas.width() != width {
            canvas.set_width(width);
        }
        if canvas.height() != height {
            canvas.set_height(height);
        }
        canvas
            .style()
            .set_property("width", &format!("{}px", self.width))?;
        canvas
            .style()
            .set_property("height", &format!("{}px", self.height))?;
        // Rounded backing dimensions can differ slightly from logical size * DPR.
        ctx.set_transform(
            f64::from(width) / self.width.max(1.0),
            0.0,
            0.0,
            f64::from(height) / self.height.max(1.0),
            0.0,
            0.0,
        )
    }
}

/// Publish measured logical metrics for browser wrappers without a WASM API change.
/// Read `canvas.dataset.phuxCellWidth` and `canvas.dataset.phuxCellHeight`.
///
/// # Errors
/// Returns a DOM attribute error.
pub fn publish_metrics(canvas: &HtmlCanvasElement, metrics: &Metrics) -> Result<(), JsValue> {
    canvas.set_attribute("data-phux-cell-width", &metrics.cell_w.to_string())?;
    canvas.set_attribute("data-phux-cell-height", &metrics.cell_h.to_string())
}

/// Draw a one-logical-pixel neutral boundary, without changing pane geometry.
/// The focused pane differs only in luminance, not border thickness.
pub fn paint_pane_border(
    ctx: &CanvasRenderingContext2d,
    metrics: &Metrics,
    rect: &crate::PaneRect,
    active: bool,
) {
    ctx.set_stroke_style_str(if active { "#5b5b65" } else { "#27272d" });
    ctx.set_line_width(1.0);
    ctx.stroke_rect(
        f64::from(rect.x) * metrics.cell_w + 0.5,
        f64::from(rect.y) * metrics.cell_h + 0.5,
        (f64::from(rect.cols) * metrics.cell_w - 1.0).max(0.0),
        (f64::from(rect.rows) * metrics.cell_h - 1.0).max(0.0),
    );
}

pub(crate) fn device_pixel_ratio() -> f64 {
    valid_dpr(web_sys::window().map_or(1.0, |window| window.device_pixel_ratio()))
}

fn valid_dpr(dpr: f64) -> f64 {
    if dpr.is_finite() && dpr > 0.0 {
        dpr
    } else {
        1.0
    }
}
