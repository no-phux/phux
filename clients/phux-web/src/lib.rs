//! phux-web — the phux browser client.
//!
//! Renders terminals to a `<canvas>` using the ghostty-vt engine (via
//! [`phux_vt_web`]), and (subsequent milestones) speaks the phux wire over a
//! WebSocket and routes keyboard input back. This module currently provides the
//! canvas renderer over the engine's styled [`Grid`].

#![deny(missing_docs)]

pub mod client;
pub mod framing;
pub mod input;
pub mod selection;
pub mod session;

pub use session::{AgentBadge, Outcome, Session};

use std::cell::RefCell;
use std::ops::Range;

use futures_channel::oneshot;
use futures_util::future::{Either, select};
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
    let canvas = canvas_by_id(&canvas_id)?;
    let client = client::run(&ws_url, canvas, cols, rows).await?;
    client.retain_until_failure();
    Ok(())
}

/// A hosted session controller returned to JavaScript.
#[wasm_bindgen]
pub struct HostedClient {
    client: client::Client,
}

#[wasm_bindgen]
impl HostedClient {
    /// Close the socket and synchronously remove all browser handlers and timers.
    pub fn close(&self) {
        self.client.close();
    }

    /// Resize the terminal to `cols`x`rows` cells (for example after the
    /// host element changes size). The canvas follows the server's new
    /// geometry.
    pub fn resize(&self, cols: u16, rows: u16) {
        self.client.resize(cols, rows);
    }
}

/// Hosted JS entry point. Unlike [`start`], this requires the hosted session
/// control preamble before accepting binary phux wire frames and reports safe,
/// structured lifecycle events through `callback`. An optional `signal` cancels
/// establishment, releasing the socket even before the attach completes.
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
    signal: Option<web_sys::AbortSignal>,
) -> Result<HostedClient, JsValue> {
    let canvas = canvas_by_id(&canvas_id)?;
    let client = with_abort(
        client::run_hosted(&ws_url, canvas, cols, rows, callback),
        signal,
    )
    .await?;
    Ok(HostedClient { client })
}

struct AbortBinding {
    signal: web_sys::AbortSignal,
    callback: Closure<dyn FnMut()>,
}

impl Drop for AbortBinding {
    fn drop(&mut self) {
        let _ = self
            .signal
            .remove_event_listener_with_callback("abort", self.callback.as_ref().unchecked_ref());
    }
}

async fn with_abort<T>(
    future: impl Future<Output = Result<T, JsValue>>,
    signal: Option<web_sys::AbortSignal>,
) -> Result<T, JsValue> {
    let Some(signal) = signal else {
        return future.await;
    };
    if signal.aborted() {
        return Err(JsValue::from_str("hosted connection aborted"));
    }
    let (sender, receiver) = oneshot::channel();
    let sender = RefCell::new(Some(sender));
    let callback = Closure::<dyn FnMut()>::new(move || {
        if let Some(sender) = sender.borrow_mut().take() {
            let _ = sender.send(());
        }
    });
    signal.add_event_listener_with_callback("abort", callback.as_ref().unchecked_ref())?;
    let _binding = AbortBinding { signal, callback };
    // Dropping the losing establishment future runs AppEstablishment's disposal
    // guard, closing its transport and removing the browser handlers.
    match select(Box::pin(future), receiver).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => Err(JsValue::from_str("hosted connection aborted")),
    }
}

/// JS entry point for the WebTransport-first path: try HTTP/3-over-QUIC at
/// `wt_url` (an `https://` session URL; append `?token=<hex>` for a
/// token-authenticated listener) and fall back to the WebSocket at `ws_url`
/// when the API or the endpoint is unavailable. After initial readiness the
/// entry point supervises transport loss and repeats the bounded fallback.
///
/// # Errors
/// Fails if the canvas element is missing or both transports fail to
/// connect.
#[wasm_bindgen]
pub async fn start_webtransport(
    wt_url: String,
    ws_url: String,
    canvas_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), JsValue> {
    let canvas = canvas_by_id(&canvas_id)?;
    let client = client::run_with_fallback(&wt_url, &ws_url, canvas, cols, rows).await?;
    client.enable_auto_reconnect(Some(&wt_url), &ws_url);
    Ok(())
}

/// Resolve the `<canvas>` element the terminal renders into.
fn canvas_by_id(canvas_id: &str) -> Result<web_sys::HtmlCanvasElement, JsValue> {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(canvas_id))
        .ok_or_else(|| JsValue::from_str("canvas element not found"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(Into::into)
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
    render_selected(ctx, grid, m, cursor_on, &(0..0));
}

/// [`render`], drawing the row-major cell indices in `selected` inverted.
pub fn render_selected(
    ctx: &CanvasRenderingContext2d,
    grid: &Grid,
    m: &Metrics,
    cursor_on: bool,
    selected: &Range<usize>,
) {
    ctx.set_font(&m.font);
    ctx.set_text_baseline("top");
    for row in 0..grid.rows {
        for col in 0..grid.cols {
            draw_cell(ctx, grid, m, col, row, selected);
        }
    }
    if cursor_on {
        draw_cursor(ctx, grid, m);
    }
}

/// Redraw only the cursor's cell of an already painted `grid`: the cell as
/// is, then the cursor over it when `cursor_on`. A blink costs one cell
/// instead of the whole grid.
pub fn render_cursor_cell(
    ctx: &CanvasRenderingContext2d,
    grid: &Grid,
    m: &Metrics,
    cursor_on: bool,
    selected: &Range<usize>,
) {
    ctx.set_font(&m.font);
    ctx.set_text_baseline("top");
    draw_cell(ctx, grid, m, grid.cursor_col, grid.cursor_row, selected);
    if cursor_on {
        draw_cursor(ctx, grid, m);
    }
}

fn draw_cell(
    ctx: &CanvasRenderingContext2d,
    grid: &Grid,
    m: &Metrics,
    col: u16,
    row: u16,
    selected: &Range<usize>,
) {
    let index = usize::from(row) * usize::from(grid.cols) + usize::from(col);
    let Some(cell) = grid.cells.get(index).filter(|_| col < grid.cols) else {
        return;
    };
    let x = f64::from(col) * m.cell_w;
    let y = f64::from(row) * m.cell_h;
    let (mut fg, mut bg) = (
        cell.fg.unwrap_or(grid.default_fg),
        cell.bg.unwrap_or(grid.default_bg),
    );
    if selected.contains(&index) {
        std::mem::swap(&mut fg, &mut bg);
    }
    ctx.set_fill_style_str(&css(bg));
    ctx.fill_rect(x, y, m.cell_w, m.cell_h);
    if cell.ch != ' ' && cell.ch != '\0' {
        ctx.set_fill_style_str(&css(fg));
        let mut buf = [0u8; 4];
        let _ = ctx.fill_text(cell.ch.encode_utf8(&mut buf), x, y);
    }
}

/// Inverted block cursor: fill the cell with the foreground color, then
/// redraw its glyph in the background color on top.
fn draw_cursor(ctx: &CanvasRenderingContext2d, grid: &Grid, m: &Metrics) {
    let (col, row) = (grid.cursor_col, grid.cursor_row);
    if !grid.cursor_visible || col >= grid.cols || row >= grid.rows {
        return;
    }
    let x = f64::from(col) * m.cell_w;
    let y = f64::from(row) * m.cell_h;
    let cell = grid
        .cells
        .get(usize::from(row) * usize::from(grid.cols) + usize::from(col));
    let fg = cell.and_then(|c| c.fg).unwrap_or(grid.default_fg);
    ctx.set_fill_style_str(&css(fg));
    ctx.fill_rect(x, y, m.cell_w, m.cell_h);
    if let Some(c) = cell
        && c.ch != ' '
        && c.ch != '\0'
    {
        ctx.set_fill_style_str(&css(c.bg.unwrap_or(grid.default_bg)));
        let mut buf = [0u8; 4];
        let _ = ctx.fill_text(c.ch.encode_utf8(&mut buf), x, y);
    }
}

fn css(c: Rgb) -> String {
    format!("rgb({} {} {})", c.r, c.g, c.b)
}
