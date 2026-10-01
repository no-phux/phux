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
pub mod links;
mod panes;
pub mod search;
pub mod selection;
pub mod session;

pub use panes::PaneRect;

pub use session::{AgentBadge, Outcome, Session};

use std::cell::RefCell;
use std::ops::Range;

use futures_channel::oneshot;
use futures_util::future::{Either, select};
use phux_vt_web::{Grid, GridCell, Rgb};
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

    /// Split the focused terminal using one new resource on this connection.
    ///
    /// # Errors
    /// Refuses invalid axes, a fifth pane, or a split while another is pending.
    pub fn split_pane(&self, axis: &str) -> Result<(), JsValue> {
        self.client.split_pane(axis)
    }

    /// Move keyboard focus to the next published pane.
    ///
    /// # Errors
    /// Fails when the connection has no usable terminal.
    pub fn focus_next_pane(&self) -> Result<(), JsValue> {
        self.client.focus_next_pane()
    }

    /// Close the focused terminal without closing its siblings.
    ///
    /// # Errors
    /// Refuses the last pane; use the host's Release Session control instead.
    pub fn close_pane(&self) -> Result<(), JsValue> {
        self.client.close_pane()
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

impl Metrics {
    /// The cell size in whole pixels (at least 1x1), as the viewport
    /// reports it to the server.
    #[must_use]
    pub fn cell_px(&self) -> (u16, u16) {
        let whole = |px: f64| px.round().clamp(1.0, f64::from(u16::MAX)) as u16;
        (whole(self.cell_w), whole(self.cell_h))
    }
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

/// Paint a [`Grid`] onto a 2D canvas context: row by row, a background rect
/// per cell, then each glyph in its resolved foreground. Cells fall back to
/// the grid defaults.
/// When `cursor_on` is true and the grid's cursor is visible, an inverted block
/// cursor is drawn over the cursor cell (the caller toggles `cursor_on` to blink).
pub fn render(ctx: &CanvasRenderingContext2d, grid: &Grid, m: &Metrics, cursor_on: bool) {
    render_selected(ctx, grid, m, cursor_on, &Overlay::default());
}

/// A highlighted run of row-major cells: a search match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    /// Row-major cell indices.
    pub cells: Range<usize>,
    /// Whether this is the current match.
    pub current: bool,
}

/// What is drawn over the grid's own colors: the selection (inverted) and
/// search matches.
#[derive(Clone, Debug, Default)]
pub struct Overlay<'a> {
    /// Row-major indices of the selected cells.
    pub selected: Range<usize>,
    /// Search matches on screen.
    pub marks: &'a [Mark],
}

/// Background of a search match, and of the current one; both draw their
/// text in [`MARK_TEXT`].
const MARK_BG: Rgb = Rgb {
    r: 0x8a,
    g: 0x72,
    b: 0x1c,
};
const CURRENT_MARK_BG: Rgb = Rgb {
    r: 0xf2,
    g: 0xb1,
    b: 0x3a,
};
const MARK_TEXT: Rgb = Rgb { r: 0, g: 0, b: 0 };

/// [`render`] with an [`Overlay`]: selected cells inverted, search matches
/// highlighted.
pub fn render_selected(
    ctx: &CanvasRenderingContext2d,
    grid: &Grid,
    m: &Metrics,
    cursor_on: bool,
    overlay: &Overlay<'_>,
) {
    ctx.set_font(&m.font);
    ctx.set_text_baseline("top");
    for row in 0..grid.rows {
        draw_row(ctx, grid, m, row, overlay);
    }
    if cursor_on {
        draw_cursor(ctx, grid, m);
    }
}

/// Redraw only the cursor's row of an already painted `grid`, then the
/// cursor over it when `cursor_on`. A blink costs one row instead of the
/// whole grid, and repaints whatever part of a neighbor's glyph (a wide
/// character's right half) reaches into the cursor's cell.
pub fn render_cursor_row(
    ctx: &CanvasRenderingContext2d,
    grid: &Grid,
    m: &Metrics,
    cursor_on: bool,
    overlay: &Overlay<'_>,
) {
    ctx.set_font(&m.font);
    ctx.set_text_baseline("top");
    draw_row(ctx, grid, m, grid.cursor_row, overlay);
    if cursor_on {
        draw_cursor(ctx, grid, m);
    }
}

/// Paint one row: every cell's background first, then every glyph. A wide
/// (CJK) glyph is drawn from its own cell across the spacer cell after it,
/// so painting cell by cell would cover its right half with the spacer's
/// background.
fn draw_row(
    ctx: &CanvasRenderingContext2d,
    grid: &Grid,
    m: &Metrics,
    row: u16,
    overlay: &Overlay<'_>,
) {
    let start = usize::from(row) * usize::from(grid.cols);
    let Some(cells) = grid.cells.get(start..start + usize::from(grid.cols)) else {
        return;
    };
    let y = f64::from(row) * m.cell_h;
    for (col, cell) in (0..grid.cols).zip(cells) {
        let (_, bg) = cell_colors(grid, cell, start + usize::from(col), overlay);
        ctx.set_fill_style_str(&css(bg));
        ctx.fill_rect(f64::from(col) * m.cell_w, y, m.cell_w, m.cell_h);
    }
    for (col, cell) in (0..grid.cols).zip(cells) {
        if has_glyph(cell.ch) {
            let (fg, _) = cell_colors(grid, cell, start + usize::from(col), overlay);
            ctx.set_fill_style_str(&css(fg));
            draw_glyph(ctx, cell.ch, f64::from(col) * m.cell_w, y);
        }
    }
}

/// A cell's foreground and background under the overlay: the selection
/// inverts them, a search match takes the match colors.
fn cell_colors(grid: &Grid, cell: &GridCell, index: usize, overlay: &Overlay<'_>) -> (Rgb, Rgb) {
    let fg = cell.fg.unwrap_or(grid.default_fg);
    let bg = cell.bg.unwrap_or(grid.default_bg);
    if overlay.selected.contains(&index) {
        return (bg, fg);
    }
    match overlay
        .marks
        .iter()
        .find(|mark| mark.cells.contains(&index))
    {
        Some(mark) if mark.current => (MARK_TEXT, CURRENT_MARK_BG),
        Some(_) => (MARK_TEXT, MARK_BG),
        None => (fg, bg),
    }
}

/// Whether a cell draws a glyph: blanks and wide characters' spacer cells
/// do not.
const fn has_glyph(ch: char) -> bool {
    ch != ' ' && ch != '\0'
}

fn draw_glyph(ctx: &CanvasRenderingContext2d, ch: char, x: f64, y: f64) {
    let mut buf = [0u8; 4];
    let _ = ctx.fill_text(ch.encode_utf8(&mut buf), x, y);
}

/// Inverted block cursor: fill the cell with the foreground color, then
/// redraw its glyph in the background color on top, clipped to the cell so
/// a wide glyph's right half keeps its own colors.
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
    if let Some(c) = cell.filter(|c| has_glyph(c.ch)) {
        ctx.save();
        ctx.begin_path();
        ctx.rect(x, y, m.cell_w, m.cell_h);
        ctx.clip();
        ctx.set_fill_style_str(&css(c.bg.unwrap_or(grid.default_bg)));
        draw_glyph(ctx, c.ch, x, y);
        ctx.restore();
    }
}

fn css(c: Rgb) -> String {
    format!("rgb({} {} {})", c.r, c.g, c.b)
}
