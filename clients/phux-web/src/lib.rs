//! phux-web — the phux browser client.
//!
//! Renders terminals to a `<canvas>` using the ghostty-vt engine (via
//! [`phux_vt_web`]), and (subsequent milestones) speaks the phux wire over a
//! WebSocket and routes keyboard input back. This module currently provides the
//! canvas renderer over the engine's styled [`Grid`].

#![deny(missing_docs)]

pub mod client;
pub mod session;

pub use session::{Outcome, Session};

use phux_vt_web::{Grid, Rgb};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::CanvasRenderingContext2d;

/// JS entry point: connect to `ws_url` and render the attached terminal into the
/// canvas element with id `canvas_id`, sized `cols`×`rows`.
///
/// # Errors
/// Fails if the canvas element is missing or the connection can't be set up.
#[wasm_bindgen]
pub async fn start(ws_url: String, canvas_id: String, cols: u16, rows: u16) -> Result<(), JsValue> {
    let canvas = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(&canvas_id))
        .ok_or_else(|| JsValue::from_str("canvas element not found"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()?;
    let client = client::run(&ws_url, canvas, cols, rows).await?;
    // Preserve the original fire-and-forget API. Socket close still removes
    // the canvas listener and cursor timer.
    std::mem::forget(client);
    Ok(())
}

/// A hosted session controller returned to JavaScript.
#[wasm_bindgen]
pub struct HostedClient {
    client: Option<client::Client>,
}

#[wasm_bindgen]
impl HostedClient {
    /// Close the socket and synchronously remove all browser handlers and timers.
    pub fn close(&mut self) {
        if let Some(client) = &mut self.client {
            client.close();
        }
    }
}

/// Hosted JS entry point. Unlike [`start`], this requires the hosted session
/// control preamble before accepting binary phux wire frames and reports safe,
/// structured lifecycle events through `callback`.
///
/// # Errors
/// Fails if the canvas element is missing or the connection can't be set up.
#[wasm_bindgen]
pub async fn start_hosted(
    ws_url: String,
    canvas_id: String,
    cols: u16,
    rows: u16,
    callback: js_sys::Function,
) -> Result<HostedClient, JsValue> {
    let canvas = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(&canvas_id))
        .ok_or_else(|| JsValue::from_str("canvas element not found"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()?;
    let client = client::run_hosted(&ws_url, canvas, cols, rows, callback).await?;
    Ok(HostedClient {
        client: Some(client),
    })
}

/// Cell geometry + font for the canvas renderer. A monospace cell grid: every
/// cell is `cell_w`×`cell_h` device pixels.
#[derive(Clone, Debug)]
pub struct Metrics {
    /// Cell width in pixels.
    pub cell_w: f64,
    /// Cell height in pixels.
    pub cell_h: f64,
    /// CSS font string, e.g. `"14px monospace"`.
    pub font: String,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            cell_w: 8.0,
            cell_h: 16.0,
            font: "14px monospace".to_owned(),
        }
    }
}

/// Paint a [`Grid`] onto a 2D canvas context: a background rect per cell, then
/// the glyph in its resolved foreground. Cells fall back to the grid defaults.
/// When `cursor_on` is true and the grid's cursor is visible, an inverted block
/// cursor is drawn over the cursor cell (the caller toggles `cursor_on` to blink).
pub fn render(ctx: &CanvasRenderingContext2d, grid: &Grid, m: &Metrics, cursor_on: bool) {
    ctx.set_font(&m.font);
    ctx.set_text_baseline("top");

    let cols = usize::from(grid.cols);
    for row in 0..usize::from(grid.rows) {
        for col in 0..cols {
            let Some(cell) = grid.cells.get(row * cols + col) else {
                continue;
            };
            let x = col as f64 * m.cell_w;
            let y = row as f64 * m.cell_h;

            let bg = cell.bg.unwrap_or(grid.default_bg);
            ctx.set_fill_style_str(&css(bg));
            ctx.fill_rect(x, y, m.cell_w, m.cell_h);

            if cell.ch != ' ' && cell.ch != '\0' {
                let fg = cell.fg.unwrap_or(grid.default_fg);
                ctx.set_fill_style_str(&css(fg));
                let mut buf = [0u8; 4];
                let _ = ctx.fill_text(cell.ch.encode_utf8(&mut buf), x, y);
            }
        }
    }

    // Inverted block cursor: fill the cell with the foreground color, then
    // redraw its glyph in the background color on top.
    if cursor_on && grid.cursor_visible {
        let (col, row) = (usize::from(grid.cursor_col), usize::from(grid.cursor_row));
        if col < cols && row < usize::from(grid.rows) {
            let x = col as f64 * m.cell_w;
            let y = row as f64 * m.cell_h;
            let cell = grid.cells.get(row * cols + col);
            let fg = cell.and_then(|c| c.fg).unwrap_or(grid.default_fg);
            ctx.set_fill_style_str(&css(fg));
            ctx.fill_rect(x, y, m.cell_w, m.cell_h);
            if let Some(c) = cell
                && c.ch != ' '
                && c.ch != '\0'
            {
                let bg = c.bg.unwrap_or(grid.default_bg);
                ctx.set_fill_style_str(&css(bg));
                let mut buf = [0u8; 4];
                let _ = ctx.fill_text(c.ch.encode_utf8(&mut buf), x, y);
            }
        }
    }
}

fn css(c: Rgb) -> String {
    format!("rgb({} {} {})", c.r, c.g, c.b)
}
