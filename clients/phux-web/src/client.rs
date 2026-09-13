//! Browser glue: a transport to the phux server (WebTransport or WebSocket),
//! a `<canvas>`, and the keyboard, all driving a [`Session`](crate::Session).
//! This is the only part that touches the DOM/network; the protocol logic
//! lives in [`crate::session`].
//!
//! Two connect paths speak the identical wire (ADR-0025: the transport is a
//! byte-stream detail below the frame codec):
//!
//! * **WebSocket** ([`run`]) — one binary message per encoded frame. The
//!   historical path; works everywhere.
//! * **WebTransport** ([`run_webtransport`]) — HTTP/3 over QUIC, the
//!   browser's door to QUIC-class transport. One bidirectional stream
//!   carries length-prefixed frames (reassembled by
//!   [`FrameBuffer`](crate::framing::FrameBuffer), since stream chunks
//!   arrive at arbitrary boundaries). [`run_with_fallback`] tries this
//!   first and falls back to WebSocket when the API or the endpoint is
//!   unavailable.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use futures_channel::oneshot;
use futures_util::future::{Either, select};
use gloo_timers::future::TimeoutFuture;
use phux_protocol::BootstrapProfile;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::FrameKind;
use phux_vt_web::Vt;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    BinaryType, CanvasRenderingContext2d, HtmlCanvasElement, KeyboardEvent, MessageEvent,
    ReadableStreamDefaultReader, WebSocket, WebTransport, WritableStreamDefaultWriter,
};

use crate::framing::FrameBuffer;
use crate::{Metrics, render};

const CONNECT_DEADLINE_MS: u32 = 10_000;
const MAX_OUTBOUND_BYTES: usize = 1024 * 1024;
const PHUX_WS_PROTOCOL: &str = "phux.v1";
const PHUX_WS_BEARER_PREFIX: &str = "phux.bearer.";

/// DOM id of the element that holds the focused pane's agent badges.
pub const BADGE_CONTAINER_ID: &str = "phux-agent-badges";

/// Connect to a phux server over WebSocket and render the attached terminal
/// into the given canvas, routing keyboard input back. Resolves only after the
/// server validates HELLO; the handlers then run for the connection's lifetime.
///
/// # Errors
/// Fails if the engine can't load, the canvas has no 2D context, or the
/// WebSocket can't be opened.
pub async fn run(
    ws_url: &str,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
) -> Result<Client, JsValue> {
    run_websocket(ws_url, None, canvas, cols, rows, false).await
}

/// Connect over WebSocket while explicitly advertising synthesized
/// compatibility profiles only.
///
/// # Errors
/// Fails if the engine, canvas, or WebSocket cannot be initialized.
pub async fn run_synthesized_compat(
    ws_url: &str,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
) -> Result<Client, JsValue> {
    run_websocket(ws_url, None, canvas, cols, rows, true).await
}

async fn run_websocket(
    ws_url: &str,
    bearer_hex: Option<&str>,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
    synthesized_only: bool,
) -> Result<Client, JsValue> {
    let ws = websocket(ws_url, bearer_hex)?;
    ws.set_binary_type(BinaryType::Arraybuffer);

    let tx = WireTx::Ws(WsTx::new(ws.clone()));
    let (app, ready) = build_app(tx, canvas, cols, rows, synthesized_only).await?;
    install_transport_failure_hook(&app);

    install_websocket_handlers(&app, &ws);
    await_protocol_ready(&app, ready).await?;

    install_keyboard(&app)?;
    install_cursor_blink(&app)?;

    Ok(Client { app })
}

/// Connect over WebTransport (HTTP/3 over QUIC), falling back to the
/// WebSocket path when WebTransport is unavailable — an older browser
/// without the API, or a server not listening on the WebTransport endpoint.
///
/// `wt_url` is an `https://` session URL (`phux server --webtransport`; on a
/// token-authenticated listener append `?token=<hex>`, since the JS
/// `WebTransport` API cannot set request headers). `ws_url` is the WebSocket
/// fallback URL. The token is offered there through `Sec-WebSocket-Protocol`
/// rather than copied into the WSS URL; the direct server must support that
/// authenticated upgrade seam.
///
/// # Errors
/// Fails only if *both* paths fail to come up.
pub async fn run_with_fallback(
    wt_url: &str,
    ws_url: &str,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
) -> Result<Client, JsValue> {
    match run_webtransport(wt_url, canvas.clone(), cols, rows).await {
        Ok(client) => Ok(client),
        Err(_) => {
            web_sys::console::warn_1(&JsValue::from_str(
                "phux-web: WebTransport unavailable; falling back to WebSocket",
            ));
            let bearer = token_from_webtransport_url(wt_url);
            run_websocket(ws_url, bearer, canvas, cols, rows, false).await
        }
    }
}

/// Connect over WebTransport only (no fallback): establish the session, open
/// the single bidirectional wire stream, send the handshake, and start the
/// read pump.
///
/// # Errors
/// Fails if the engine can't load, the canvas has no 2D context, the
/// `WebTransport` API is missing, or the session/stream can't be established.
pub async fn run_webtransport(
    wt_url: &str,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
) -> Result<Client, JsValue> {
    // `WebTransport::new` throws (rather than returning Err) when the API is
    // absent from the global scope; the `catch` binding surfaces both cases
    // as Err so the caller's fallback fires either way.
    let wt = WebTransport::new(wt_url)
        .map_err(|_| JsValue::from_str("WebTransport initialization failed"))?;
    if await_js_promise(wt.ready(), "WebTransport readiness")
        .await
        .is_err()
    {
        wt.close();
        return Err(JsValue::from_str("WebTransport readiness failed"));
    }

    // One bidirectional stream carries the whole wire, mirroring the QUIC
    // transport's one-stream-per-connection contract.
    let stream: web_sys::WebTransportBidirectionalStream = match await_js_promise(
        wt.create_bidirectional_stream(),
        "WebTransport stream creation",
    )
    .await
    {
        Ok(stream) => stream,
        Err(_) => {
            wt.close();
            return Err(JsValue::from_str("WebTransport stream creation failed"));
        }
    };
    let writer = WritableStreamDefaultWriter::new(&stream.writable())
        .map_err(|_| JsValue::from_str("WebTransport writer initialization failed"))?;
    let reader = ReadableStreamDefaultReader::new(&stream.readable())
        .map_err(|_| JsValue::from_str("WebTransport reader initialization failed"))?;

    let tx = WireTx::Wt(Rc::new(WtTx::new(writer, wt.clone())));
    let (app, ready) = build_app(tx, canvas, cols, rows, false).await?;
    install_transport_failure_hook(&app);

    // The session is already established (unlike the WebSocket path there is
    // no onopen moment): send HELLO now; ATTACH follows HELLO_OK.
    {
        send_handshake(&app);
    }

    // Read pump: stream chunks land at arbitrary boundaries, so reassemble
    // complete frames before decoding. `wt` is moved in to keep the session
    // handle alive for the pump's lifetime.
    {
        let (cancel_tx, cancel_rx) = oneshot::channel();
        app.borrow()
            .bindings
            .borrow_mut()
            .wt_reader_cancel
            .replace(cancel_tx);
        let app = Rc::clone(&app);
        wasm_bindgen_futures::spawn_local(async move {
            run_webtransport_reader(app, reader, wt, cancel_rx).await;
        });
    }

    await_protocol_ready(&app, ready).await?;

    install_keyboard(&app)?;
    install_cursor_blink(&app)?;

    Ok(Client { app })
}

/// A live connection handle. The event handlers run for the connection's
/// lifetime; this lets a caller (or test) inspect the rendered grid.
pub struct Client {
    app: Rc<RefCell<App>>,
}

impl Client {
    pub(crate) fn enable_auto_reconnect(&self, wt_url: &str, ws_url: &str) {
        let (canvas, cols, rows) = {
            let app = self.app.borrow();
            let grid = app.session.grid();
            (app.canvas.clone(), grid.cols, grid.rows)
        };
        self.app.borrow().reconnect.replace(Some(ReconnectConfig {
            wt_url: wt_url.to_owned(),
            ws_url: ws_url.to_owned(),
            canvas,
            cols,
            rows,
        }));
        self.app
            .borrow()
            .self_owner
            .replace(Some(Rc::clone(&self.app)));
    }

    /// The current styled grid as one `String` per row (for inspection/tests).
    #[must_use]
    pub fn rows_text(&self) -> Vec<String> {
        let grid = self.app.borrow().session.grid();
        let cols = usize::from(grid.cols);
        grid.cells
            .chunks(cols.max(1))
            .map(|row| row.iter().map(|c| c.ch).collect::<String>())
            .collect()
    }

    /// Exact profile selected by the validated server handshake.
    #[must_use]
    pub fn selected_profile(&self) -> Option<BootstrapProfile> {
        self.app.borrow().session.selected_profile()
    }

    /// Agent badges the DOM currently shows for the focused pane.
    #[must_use]
    pub fn agent_badges(&self) -> Vec<crate::AgentBadge> {
        self.app.borrow().session.agent_badges()
    }

    /// Whether the transport or protocol has failed since readiness. Callers
    /// can create a fresh client with the same URLs to reconnect.
    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.app.borrow().session.is_failed()
    }

    /// Privacy-safe terminal failure reason, if this connection ended.
    #[must_use]
    pub fn failure_reason(&self) -> Option<String> {
        self.app.borrow().failure_reason.borrow().clone()
    }

    /// Establish a fresh transport and protocol session after this client has
    /// failed, preserving the canvas and last negotiated viewport size.
    ///
    /// # Errors
    /// Returns an error while the current client is still live, or when both
    /// replacement transports fail to reach `HELLO_OK` within the deadline.
    pub async fn reconnect_with_fallback(
        &self,
        wt_url: &str,
        ws_url: &str,
    ) -> Result<Self, JsValue> {
        let (canvas, cols, rows) = {
            let app = self.app.borrow();
            if !app.session.is_failed() {
                return Err(JsValue::from_str("connection is still active"));
            }
            let grid = app.session.grid();
            (app.canvas.clone(), grid.cols, grid.rows)
        };
        run_with_fallback(wt_url, ws_url, canvas, cols, rows).await
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let supervised = self.app.borrow().self_owner.borrow().is_some();
        if !supervised {
            self.app.borrow_mut().dispose();
        }
    }
}

/// The send half of whichever transport carried the connection. Both carry
/// identical encoded frames; only the byte-stream mechanics differ.
enum WireTx {
    /// One binary message per frame.
    Ws(WsTx),
    /// Length-prefixed frames over the session's single bidirectional stream.
    Wt(Rc<WtTx>),
}

impl WireTx {
    fn send(&self, frame: &[u8]) -> Result<(), String> {
        match self {
            Self::Ws(tx) => tx.send(frame),
            Self::Wt(tx) => tx.enqueue(frame),
        }
    }

    fn install_failure_hook(&self, hook: Rc<dyn Fn(String)>) {
        match self {
            Self::Ws(tx) => tx.failure.install(hook),
            Self::Wt(tx) => tx.failure.install(hook),
        }
    }

    fn close(&self) {
        match self {
            Self::Ws(tx) => {
                let _ = tx.socket.close();
            }
            Self::Wt(tx) => {
                tx.shutdown();
            }
        }
    }
}

type FailureCallback = Rc<dyn Fn(String)>;

#[derive(Clone, Default)]
struct FailureHook(Rc<RefCell<Option<FailureCallback>>>);

impl FailureHook {
    fn install(&self, hook: Rc<dyn Fn(String)>) {
        self.0.replace(Some(hook));
    }

    fn notify(&self, message: &str) {
        let hook = self.0.borrow().clone();
        if let Some(hook) = hook {
            hook(message.to_owned());
        }
    }
}

struct WsTx {
    socket: WebSocket,
    failure: FailureHook,
}

impl WsTx {
    fn new(socket: WebSocket) -> Self {
        Self {
            socket,
            failure: FailureHook::default(),
        }
    }

    fn send(&self, frame: &[u8]) -> Result<(), String> {
        if self.socket.ready_state() != WebSocket::OPEN {
            return Err("WebSocket is not open".to_owned());
        }
        let queued = self.socket.buffered_amount() as usize;
        if queued.saturating_add(frame.len()) > MAX_OUTBOUND_BYTES {
            return Err("WebSocket outbound byte budget exceeded".to_owned());
        }
        self.socket
            .send_with_u8_array(frame)
            .map_err(|_| "WebSocket send failed".to_owned())
    }
}

struct WtTx {
    writer: WritableStreamDefaultWriter,
    session: WebTransport,
    queue: RefCell<OutboundQueue>,
    writing: Cell<bool>,
    failed: Cell<bool>,
    cancel: RefCell<Option<oneshot::Sender<()>>>,
    failure: FailureHook,
}

impl WtTx {
    fn new(writer: WritableStreamDefaultWriter, session: WebTransport) -> Self {
        Self {
            writer,
            session,
            queue: RefCell::new(OutboundQueue::default()),
            writing: Cell::new(false),
            failed: Cell::new(false),
            cancel: RefCell::new(None),
            failure: FailureHook::default(),
        }
    }

    fn enqueue(self: &Rc<Self>, frame: &[u8]) -> Result<(), String> {
        if self.failed.get() {
            return Err("WebTransport writer is closed".to_owned());
        }
        self.queue.borrow_mut().push(frame)?;
        if !self.writing.replace(true) {
            Rc::clone(self).spawn_writer();
        }
        Ok(())
    }

    fn spawn_writer(self: Rc<Self>) {
        let (cancel_tx, cancel_rx) = oneshot::channel();
        self.cancel.replace(Some(cancel_tx));
        wasm_bindgen_futures::spawn_local(async move {
            self.write_queued(cancel_rx).await;
        });
    }

    async fn write_queued(&self, mut cancel: oneshot::Receiver<()>) {
        loop {
            let Some(frame) = self.queue.borrow_mut().pop() else {
                self.finish_writer();
                return;
            };
            let PromiseOutcome::Completed(ready, next_cancel) =
                await_or_cancel(self.writer.ready(), cancel).await
            else {
                self.finish_writer();
                return;
            };
            cancel = next_cancel;
            if ready.is_err() {
                self.fail("WebTransport write failed");
                return;
            }
            let chunk = js_sys::Uint8Array::from(frame.as_slice());
            let PromiseOutcome::Completed(written, next_cancel) =
                await_or_cancel(self.writer.write_with_chunk(&chunk), cancel).await
            else {
                self.finish_writer();
                return;
            };
            cancel = next_cancel;
            if written.is_err() {
                self.fail("WebTransport write failed");
                return;
            }
            self.queue.borrow_mut().complete(frame.len());
        }
    }

    fn fail(&self, message: &str) {
        if self.failed.get() {
            return;
        }
        self.shutdown();
        self.failure.notify(message);
    }

    fn shutdown(&self) {
        if self.failed.replace(true) {
            return;
        }
        self.queue.borrow_mut().clear();
        if let Some(cancel) = self.cancel.borrow_mut().take() {
            let _ = cancel.send(());
        }
        self.session.close();
        let pending = self.writer.abort();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = JsFuture::from(pending).await;
        });
    }

    fn finish_writer(&self) {
        self.cancel.borrow_mut().take();
        self.writing.set(false);
    }
}

enum PromiseOutcome<T> {
    Completed(Result<T, JsValue>, oneshot::Receiver<()>),
    Cancelled,
}

async fn await_or_cancel<T: wasm_bindgen::convert::FromWasmAbi + 'static>(
    promise: js_sys::Promise<T>,
    cancel: oneshot::Receiver<()>,
) -> PromiseOutcome<T> {
    match select(cancel, Box::pin(JsFuture::from(promise))).await {
        Either::Left(_) => PromiseOutcome::Cancelled,
        Either::Right((result, cancel)) => PromiseOutcome::Completed(result, cancel),
    }
}

#[derive(Default)]
struct OutboundQueue {
    frames: VecDeque<Vec<u8>>,
    bytes: usize,
}

impl OutboundQueue {
    fn push(&mut self, frame: &[u8]) -> Result<(), String> {
        let next_bytes = self.bytes.saturating_add(frame.len());
        if next_bytes > MAX_OUTBOUND_BYTES {
            return Err("WebTransport outbound byte budget exceeded".to_owned());
        }
        self.frames.push_back(frame.to_vec());
        self.bytes = next_bytes;
        Ok(())
    }

    fn pop(&mut self) -> Option<Vec<u8>> {
        self.frames.pop_front()
    }

    fn complete(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_sub(bytes);
    }

    fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }
}

#[derive(Default)]
struct AppBindings {
    websocket: Option<WebSocketBindings>,
    keyboard: Option<KeyboardBinding>,
    blink: Option<BlinkBinding>,
    wt_reader_cancel: Option<oneshot::Sender<()>>,
}

impl AppBindings {
    fn dispose(&mut self) {
        if let Some(websocket) = self.websocket.take() {
            websocket.dispose();
        }
        if let Some(keyboard) = self.keyboard.take() {
            keyboard.dispose();
        }
        if let Some(blink) = self.blink.take() {
            blink.dispose();
        }
        if let Some(cancel) = self.wt_reader_cancel.take() {
            let _ = cancel.send(());
        }
    }
}

struct WebSocketBindings {
    socket: WebSocket,
    onopen: Closure<dyn FnMut()>,
    onmessage: Closure<dyn FnMut(MessageEvent)>,
    onerror: Closure<dyn FnMut(web_sys::Event)>,
    onclose: Closure<dyn FnMut(web_sys::Event)>,
}

impl WebSocketBindings {
    fn dispose(self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onerror(None);
        self.socket.set_onclose(None);
        drop((self.onopen, self.onmessage, self.onerror, self.onclose));
    }
}

struct KeyboardBinding {
    document: web_sys::Document,
    callback: Closure<dyn FnMut(KeyboardEvent)>,
}

impl KeyboardBinding {
    fn dispose(self) {
        let _ = self
            .document
            .remove_event_listener_with_callback("keydown", self.callback.as_ref().unchecked_ref());
    }
}

struct BlinkBinding {
    window: web_sys::Window,
    interval: i32,
    callback: Closure<dyn FnMut()>,
}

impl BlinkBinding {
    fn dispose(self) {
        self.window.clear_interval_with_handle(self.interval);
        drop(self.callback);
    }
}

struct App {
    session: crate::Session,
    tx: WireTx,
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    metrics: Metrics,
    /// Cursor blink phase; toggled by an interval in `run`.
    cursor_on: Cell<bool>,
    bindings: RefCell<AppBindings>,
    ready: RefCell<Option<oneshot::Sender<Result<(), String>>>>,
    failure_reason: RefCell<Option<String>>,
    reconnect: RefCell<Option<ReconnectConfig>>,
    self_owner: RefCell<Option<Rc<RefCell<App>>>>,
}

impl App {
    fn send(&self, frames: Vec<Vec<u8>>) -> Result<(), String> {
        for f in frames {
            self.tx.send(&f)?;
        }
        Ok(())
    }

    fn signal_ready(&self) {
        if let Some(ready) = self.ready.borrow_mut().take() {
            let _ = ready.send(Ok(()));
        }
    }

    fn signal_failed(&self, message: &str) {
        if let Some(ready) = self.ready.borrow_mut().take() {
            let _ = ready.send(Err(message.to_owned()));
        }
    }

    fn fail(&mut self, message: &str) {
        if self.failure_reason.borrow().is_some() {
            return;
        }
        self.failure_reason.replace(Some(message.to_owned()));
        self.session.fail_protocol(message);
        self.signal_failed(message);
        self.bindings.get_mut().dispose();
        self.tx.close();
        let reconnect = self.reconnect.get_mut().take();
        self.self_owner.get_mut().take();
        if let Some(config) = reconnect {
            spawn_reconnect(config);
        }
    }

    fn dispose(&mut self) {
        self.bindings.get_mut().dispose();
        self.tx.close();
        self.reconnect.get_mut().take();
        self.self_owner.get_mut().take();
    }

    /// Project the focused pane's agent badges into the DOM: one
    /// `<span class="phux-agent-badge">` per `AgentSession` under a
    /// `#phux-agent-badges` container beside the canvas, hidden when empty.
    fn paint_badges(&self) {
        let Some(container) = self.badge_container() else {
            return;
        };
        let badges = self.session.agent_badges();
        container.set_text_content(None);
        let Some(document) = container.owner_document() else {
            return;
        };
        for badge in &badges {
            let Ok(span) = document.create_element("span") else {
                continue;
            };
            span.set_class_name("phux-agent-badge");
            let _ = span.set_attribute("data-provider", &badge.provider);
            let _ = span.set_attribute("data-state", &badge.state);
            let provider = if badge.provider.is_empty() {
                "agent"
            } else {
                badge.provider.as_str()
            };
            let text = if badge.state.is_empty() || badge.state == "unknown" {
                provider.to_owned()
            } else {
                format!("{provider}: {}", badge.state)
            };
            span.set_text_content(Some(&text));
            let _ = container.append_child(&span);
        }
        let _ = container.set_attribute("hidden", "");
        if !badges.is_empty() {
            let _ = container.remove_attribute("hidden");
        }
    }

    /// The badge container, created next to the canvas on first use.
    fn badge_container(&self) -> Option<web_sys::Element> {
        let document = self.canvas.owner_document()?;
        if let Some(existing) = document.get_element_by_id(BADGE_CONTAINER_ID) {
            return Some(existing);
        }
        let container = document.create_element("div").ok()?;
        container.set_id(BADGE_CONTAINER_ID);
        let _ = container.set_attribute("hidden", "");
        let parent = self
            .canvas
            .parent_element()
            .or_else(|| document.body().map(Into::into))?;
        parent.append_child(&container).ok()?;
        Some(container)
    }

    fn paint(&self) {
        if !self.session.render_visible() {
            return;
        }
        let grid = self.session.grid();
        // Keep the canvas sized to the grid (handles server-side resizes).
        let w = u32::from(grid.cols) * (self.metrics.cell_w as u32);
        let h = u32::from(grid.rows) * (self.metrics.cell_h as u32);
        if self.canvas.width() != w {
            self.canvas.set_width(w);
        }
        if self.canvas.height() != h {
            self.canvas.set_height(h);
        }
        render(&self.ctx, &grid, &self.metrics, self.cursor_on.get());
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.dispose();
    }
}

/// Load the engine, grab the canvas 2D context, and assemble the shared
/// [`App`] around an established transport send half.
async fn build_app(
    tx: WireTx,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
    synthesized_only: bool,
) -> Result<(Rc<RefCell<App>>, oneshot::Receiver<Result<(), String>>), JsValue> {
    let vt = Vt::load().await?;
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("no 2D context"))?
        .dyn_into()?;

    let (ready_tx, ready_rx) = oneshot::channel();
    let app = Rc::new(RefCell::new(App {
        session: if synthesized_only {
            crate::Session::new_synthesized_compat(&vt, cols, rows)
        } else {
            crate::Session::new(&vt, cols, rows)
        },
        tx,
        canvas,
        ctx,
        metrics: Metrics::default(),
        cursor_on: Cell::new(true),
        bindings: RefCell::new(AppBindings::default()),
        ready: RefCell::new(Some(ready_tx)),
        failure_reason: RefCell::new(None),
        reconnect: RefCell::new(None),
        self_owner: RefCell::new(None),
    }));
    Ok((app, ready_rx))
}

struct ReconnectConfig {
    wt_url: String,
    ws_url: String,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
}

fn spawn_reconnect(config: ReconnectConfig) {
    wasm_bindgen_futures::spawn_local(async move {
        TimeoutFuture::new(250).await;
        loop {
            match run_with_fallback(
                &config.wt_url,
                &config.ws_url,
                config.canvas.clone(),
                config.cols,
                config.rows,
            )
            .await
            {
                Ok(client) => {
                    client.enable_auto_reconnect(&config.wt_url, &config.ws_url);
                    return;
                }
                Err(_) => TimeoutFuture::new(1_000).await,
            }
        }
    });
}

fn websocket(ws_url: &str, bearer_hex: Option<&str>) -> Result<WebSocket, JsValue> {
    let Some(token) = bearer_hex else {
        return WebSocket::new(ws_url);
    };
    let protocols = js_sys::Array::new();
    for protocol in authenticated_websocket_protocols(token) {
        protocols.push(&JsValue::from_str(&protocol));
    }
    WebSocket::new_with_str_sequence(ws_url, protocols.as_ref())
        .map_err(|_| JsValue::from_str("authenticated WebSocket initialization failed"))
}

fn authenticated_websocket_protocols(token: &str) -> [String; 2] {
    [
        PHUX_WS_PROTOCOL.to_owned(),
        format!("{PHUX_WS_BEARER_PREFIX}{token}"),
    ]
}

fn token_from_webtransport_url(url: &str) -> Option<&str> {
    let query = url.split_once('?')?.1.split('#').next()?;
    let token = query
        .split('&')
        .find_map(|part| part.strip_prefix("token="))?;
    (!token.is_empty()
        && token.len() % 2 == 0
        && token.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then_some(token)
}

fn install_transport_failure_hook(app: &Rc<RefCell<App>>) {
    let weak = Rc::downgrade(app);
    let hook = Rc::new(move |message: String| {
        if let Some(app) = weak.upgrade() {
            close_with_transport_error(&app, &message);
        }
    });
    app.borrow().tx.install_failure_hook(hook);
}

fn install_websocket_handlers(app: &Rc<RefCell<App>>, ws: &WebSocket) {
    // Engine loading is asynchronous, so a local socket may already be open.
    // The latch also handles a queued open event arriving after the state check.
    let hello_sent = Rc::new(Cell::new(false));
    let onopen = websocket_open_callback(app, Rc::clone(&hello_sent));
    let onmessage = websocket_message_callback(app);
    let onerror = websocket_failure_callback(app, "WebSocket transport error");
    let onclose = websocket_failure_callback(app, "WebSocket closed by peer");

    ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
    ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));
    ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
    let old = app
        .borrow()
        .bindings
        .borrow_mut()
        .websocket
        .replace(WebSocketBindings {
            socket: ws.clone(),
            onopen,
            onmessage,
            onerror,
            onclose,
        });
    if let Some(old) = old {
        old.dispose();
    }

    if ws.ready_state() == WebSocket::OPEN && !hello_sent.replace(true) {
        send_handshake(app);
    }
}

fn websocket_open_callback(
    app: &Rc<RefCell<App>>,
    hello_sent: Rc<Cell<bool>>,
) -> Closure<dyn FnMut()> {
    let weak = Rc::downgrade(app);
    Closure::new(move || {
        if !hello_sent.replace(true)
            && let Some(app) = weak.upgrade()
        {
            send_handshake(&app);
        }
    })
}

fn websocket_message_callback(app: &Rc<RefCell<App>>) -> Closure<dyn FnMut(MessageEvent)> {
    let weak = Rc::downgrade(app);
    Closure::new(move |event: MessageEvent| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.borrow().session.is_failed() {
            return;
        }
        let framed = js_sys::Uint8Array::new(&event.data()).to_vec();
        match decode_server_frame(&app, &framed) {
            Ok(frame) => {
                let _ = handle_frame(&app, frame);
            }
            Err(message) => close_with_protocol_error(&app, &message),
        }
    })
}

fn websocket_failure_callback(
    app: &Rc<RefCell<App>>,
    message: &'static str,
) -> Closure<dyn FnMut(web_sys::Event)> {
    let weak = Rc::downgrade(app);
    Closure::new(move |_| {
        if let Some(app) = weak.upgrade() {
            close_with_transport_error(&app, message);
        }
    })
}

fn send_handshake(app: &Rc<RefCell<App>>) {
    let frames = app.borrow().session.handshake();
    let result = app.borrow().send(frames);
    if let Err(message) = result {
        close_with_transport_error(app, &message);
    }
}

async fn await_protocol_ready(
    app: &Rc<RefCell<App>>,
    ready: oneshot::Receiver<Result<(), String>>,
) -> Result<(), JsValue> {
    match select(ready, TimeoutFuture::new(CONNECT_DEADLINE_MS)).await {
        Either::Left((Ok(Ok(())), _)) => Ok(()),
        Either::Left((Ok(Err(message)), _)) => Err(JsValue::from_str(&message)),
        Either::Left((Err(_), _)) => Err(JsValue::from_str("connection closed before HELLO_OK")),
        Either::Right(((), _)) => {
            close_with_transport_error(app, "protocol HELLO timed out");
            Err(JsValue::from_str("protocol HELLO timed out"))
        }
    }
}

async fn await_js_promise<T: wasm_bindgen::convert::FromWasmAbi + 'static>(
    promise: js_sys::Promise<T>,
    stage: &'static str,
) -> Result<T, JsValue> {
    await_js_promise_with_deadline(promise, stage, CONNECT_DEADLINE_MS).await
}

async fn await_js_promise_with_deadline<T: wasm_bindgen::convert::FromWasmAbi + 'static>(
    promise: js_sys::Promise<T>,
    stage: &'static str,
    deadline_ms: u32,
) -> Result<T, JsValue> {
    match select(
        Box::pin(JsFuture::from(promise)),
        Box::pin(TimeoutFuture::new(deadline_ms)),
    )
    .await
    {
        Either::Left((result, _)) => result.map_err(|_| JsValue::from_str(stage)),
        Either::Right(((), _)) => Err(JsValue::from_str(&format!("{stage} timed out"))),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiveFlow {
    Continue,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebTransportExit<'a> {
    CleanEof,
    PartialEof,
    ReadError(&'a str),
    MissingValue,
}

async fn run_webtransport_reader(
    app: Rc<RefCell<App>>,
    reader: ReadableStreamDefaultReader,
    session: WebTransport,
    mut cancel: oneshot::Receiver<()>,
) {
    let _session = session;
    let mut frames = FrameBuffer::new();
    loop {
        let read = Box::pin(JsFuture::from(reader.read()));
        let result = match select(cancel, read).await {
            Either::Left(_) => return,
            Either::Right((result, next_cancel)) => {
                cancel = next_cancel;
                result
            }
        };
        let result = match result {
            Ok(result) => result,
            Err(_) => {
                let _ = webtransport_exit_flow(
                    WebTransportExit::ReadError("WebTransport read failed"),
                    |message, protocol| close_webtransport_exit(&app, message, protocol),
                );
                return;
            }
        };
        let Some(chunk) = webtransport_chunk(&app, &frames, &result) else {
            return;
        };
        if matches!(
            process_webtransport_chunk(&app, &mut frames, &chunk),
            ReceiveFlow::Stop
        ) {
            return;
        }
    }
}

fn webtransport_chunk(
    app: &Rc<RefCell<App>>,
    frames: &FrameBuffer,
    result: &JsValue,
) -> Option<Vec<u8>> {
    let done = js_sys::Reflect::get(result, &JsValue::from_str("done"))
        .ok()
        .and_then(|done| done.as_bool())
        .unwrap_or(true);
    if done {
        let exit = webtransport_eof_exit(frames);
        let _ = webtransport_exit_flow(exit, |message, protocol| {
            close_webtransport_exit(app, message, protocol);
        });
        return None;
    }
    let Ok(value) = js_sys::Reflect::get(result, &JsValue::from_str("value")) else {
        let _ = webtransport_exit_flow(WebTransportExit::MissingValue, |message, protocol| {
            close_webtransport_exit(app, message, protocol)
        });
        return None;
    };
    Some(js_sys::Uint8Array::new(&value).to_vec())
}

fn process_webtransport_chunk(
    app: &Rc<RefCell<App>>,
    frames: &mut FrameBuffer,
    chunk: &[u8],
) -> ReceiveFlow {
    frames.push(chunk);
    let mut batch = BatchEffects::default();
    while let Some(framed) = frames.next_frame() {
        let frame = match decode_server_frame(app, &framed) {
            Ok(frame) => frame,
            Err(message) => {
                close_with_protocol_error(app, &message);
                return ReceiveFlow::Stop;
            }
        };
        let effects = apply_frame(app, frame);
        batch.merge(effects);
        if matches!(effects.flow, ReceiveFlow::Stop) {
            return ReceiveFlow::Stop;
        }
    }
    batch.paint(app);
    poisoned_framing_flow(frames, |message| close_with_protocol_error(app, message))
}

fn webtransport_eof_exit(frames: &FrameBuffer) -> WebTransportExit<'static> {
    if frames.pending_bytes() == 0 {
        WebTransportExit::CleanEof
    } else {
        WebTransportExit::PartialEof
    }
}

fn webtransport_exit_flow(
    exit: WebTransportExit<'_>,
    close: impl FnOnce(&str, bool),
) -> ReceiveFlow {
    let (message, protocol) = match exit {
        WebTransportExit::CleanEof => ("WebTransport stream closed by peer", false),
        WebTransportExit::PartialEof => {
            ("WebTransport stream ended in the middle of a frame", true)
        }
        WebTransportExit::ReadError(message) => (message, false),
        WebTransportExit::MissingValue => ("WebTransport read result omitted a chunk value", true),
    };
    close(message, protocol);
    ReceiveFlow::Stop
}

fn poisoned_framing_flow(frames: &FrameBuffer, close: impl FnOnce(&str)) -> ReceiveFlow {
    if frames.poisoned() {
        close("WebTransport stream used a zero or oversized frame length");
        ReceiveFlow::Stop
    } else {
        ReceiveFlow::Continue
    }
}

/// Drive the session with one decoded server frame: ack and repaint as the
/// session asks. Shared by both transports' receive paths.
fn handle_frame(app: &Rc<RefCell<App>>, frame: FrameKind) -> ReceiveFlow {
    let effects = apply_frame(app, frame);
    effects.paint(app);
    effects.flow
}

#[derive(Clone, Copy)]
struct BatchEffects {
    flow: ReceiveFlow,
    render: bool,
    badges: bool,
}

impl Default for BatchEffects {
    fn default() -> Self {
        Self {
            flow: ReceiveFlow::Continue,
            render: false,
            badges: false,
        }
    }
}

impl BatchEffects {
    fn merge(&mut self, other: Self) {
        self.render |= other.render;
        self.badges |= other.badges;
        if matches!(other.flow, ReceiveFlow::Stop) {
            self.flow = ReceiveFlow::Stop;
        }
    }

    fn paint(self, app: &Rc<RefCell<App>>) {
        let app = app.borrow();
        if self.render {
            app.paint();
        }
        if self.badges {
            app.paint_badges();
        }
    }
}

fn apply_frame(app: &Rc<RefCell<App>>, frame: FrameKind) -> BatchEffects {
    let mut a = app.borrow_mut();
    let outcome = a.session.on_frame(frame);
    if let Some(message) = outcome.fatal {
        web_sys::console::error_1(&JsValue::from_str(&format!(
            "phux-web protocol error: {message}",
        )));
        a.fail(&message);
        return BatchEffects {
            flow: ReceiveFlow::Stop,
            ..BatchEffects::default()
        };
    }
    if !outcome.send.is_empty()
        && let Err(message) = a.send(outcome.send)
    {
        a.fail(&message);
        return BatchEffects {
            flow: ReceiveFlow::Stop,
            ..BatchEffects::default()
        };
    }
    if a.session.selected_profile().is_some() {
        a.signal_ready();
    }
    BatchEffects {
        flow: ReceiveFlow::Continue,
        render: outcome.render,
        badges: outcome.badges,
    }
}

fn decode_server_frame(app: &Rc<RefCell<App>>, framed: &[u8]) -> Result<FrameKind, String> {
    if app.borrow().session.is_failed() {
        return Err("web session already failed".to_owned());
    }
    let limits = app.borrow().session.bootstrap_limits();
    let decoded = match limits {
        Some(limits) => FrameKind::decode_with_limits(framed, limits),
        None => FrameKind::decode(framed),
    }
    .map_err(|error| format!("server sent undecodable frame: {error:?}"))?;
    if !decoded.1.is_empty() {
        return Err("server frame contained trailing bytes".to_owned());
    }
    Ok(decoded.0)
}

fn close_with_protocol_error(app: &Rc<RefCell<App>>, message: &str) {
    web_sys::console::error_1(&JsValue::from_str(&format!(
        "phux-web protocol error: {message}",
    )));
    let mut app = app.borrow_mut();
    app.fail(message);
}

fn close_with_transport_error(app: &Rc<RefCell<App>>, message: &str) {
    if app.borrow().session.is_failed() {
        return;
    }
    web_sys::console::error_1(&JsValue::from_str(&format!(
        "phux-web transport error: {message}"
    )));
    let mut app = app.borrow_mut();
    app.fail(message);
}

fn close_webtransport_exit(app: &Rc<RefCell<App>>, message: &str, protocol: bool) {
    let kind = if protocol {
        "protocol error"
    } else {
        "transport closed"
    };
    web_sys::console::error_1(&JsValue::from_str(&format!("phux-web {kind}: {message}",)));
    let mut app = app.borrow_mut();
    app.fail(message);
}

/// Keyboard: each keydown becomes an `INPUT_KEY` for the attached terminal.
fn install_keyboard(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let weak = Rc::downgrade(app);
    let onkey = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let Some(event) = key_event_from_browser(&e) else {
            return;
        };
        let mut a = app.borrow_mut();
        if let Some(frame) = a.session.key_frame(event) {
            if let Err(message) = a.tx.send(&frame) {
                drop(a);
                close_with_transport_error(&app, &message);
                return;
            }
            e.prevent_default();
        }
    });
    document.add_event_listener_with_callback("keydown", onkey.as_ref().unchecked_ref())?;
    let old = app
        .borrow()
        .bindings
        .borrow_mut()
        .keyboard
        .replace(KeyboardBinding {
            document,
            callback: onkey,
        });
    if let Some(old) = old {
        old.dispose();
    }
    Ok(())
}

/// Cursor blink: toggle the phase and repaint on a fixed cadence.
fn install_cursor_blink(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let weak = Rc::downgrade(app);
    let blink = Closure::<dyn FnMut()>::new(move || {
        if let Some(app) = weak.upgrade() {
            let app = app.borrow();
            app.cursor_on.set(!app.cursor_on.get());
            app.paint();
        }
    });
    let interval = window.set_interval_with_callback_and_timeout_and_arguments_0(
        blink.as_ref().unchecked_ref(),
        530,
    )?;
    let old = app
        .borrow()
        .bindings
        .borrow_mut()
        .blink
        .replace(BlinkBinding {
            window,
            interval,
            callback: blink,
        });
    if let Some(old) = old {
        old.dispose();
    }
    Ok(())
}

/// Map a browser `KeyboardEvent` to a wire `KeyEvent`. Returns `None` for
/// modifier-only keydowns (which carry no terminal input on their own).
fn key_event_from_browser(e: &KeyboardEvent) -> Option<KeyEvent> {
    let key = code_to_physical_key(&e.code());

    let mut mods = ModSet::empty();
    if e.ctrl_key() {
        mods |= ModSet::CTRL;
    }
    if e.shift_key() {
        mods |= ModSet::SHIFT;
    }
    if e.alt_key() {
        mods |= ModSet::ALT;
    }
    if e.meta_key() {
        mods |= ModSet::SUPER;
    }

    // `key()` is the produced character; carry it as text for printable keys
    // (single char, no Ctrl/Meta). Named keys ("Enter", "Shift", …) are >1 char.
    let produced = e.key();
    if produced == "Shift" || produced == "Control" || produced == "Alt" || produced == "Meta" {
        return None;
    }
    let text =
        (produced.chars().count() == 1 && !e.ctrl_key() && !e.meta_key()).then_some(produced);

    Some(KeyEvent {
        action: KeyAction::Press,
        key,
        mods,
        consumed_mods: ModSet::empty(),
        composing: false,
        text,
        unshifted_codepoint: None,
    })
}

/// Map a W3C `KeyboardEvent.code` to libghostty's physical-key discriminant.
/// `KeyA`–`KeyZ` and `Digit0`–`Digit9` map arithmetically; the rest by name.
fn code_to_physical_key(code: &str) -> PhysicalKey {
    use PhysicalKey as K;

    if let Some(c) = code.strip_prefix("Key").and_then(|s| s.chars().next())
        && c.is_ascii_uppercase()
    {
        return PhysicalKey::try_from(20 + (c as u32 - u32::from(b'A'))).unwrap_or(K::Unidentified);
    }
    if let Some(d) = code.strip_prefix("Digit").and_then(|s| s.chars().next())
        && d.is_ascii_digit()
    {
        return PhysicalKey::try_from(6 + (d as u32 - u32::from(b'0'))).unwrap_or(K::Unidentified);
    }

    match code {
        "Enter" | "NumpadEnter" => K::Enter,
        "Backspace" => K::Backspace,
        "Tab" => K::Tab,
        "Space" => K::Space,
        "Escape" => K::Escape,
        "ArrowUp" => K::ArrowUp,
        "ArrowDown" => K::ArrowDown,
        "ArrowLeft" => K::ArrowLeft,
        "ArrowRight" => K::ArrowRight,
        "Home" => K::Home,
        "End" => K::End,
        "PageUp" => K::PageUp,
        "PageDown" => K::PageDown,
        "Delete" => K::Delete,
        "Insert" => K::Insert,
        "Minus" => K::Minus,
        "Equal" => K::Equal,
        "Period" => K::Period,
        "Comma" => K::Comma,
        "Slash" => K::Slash,
        "Semicolon" => K::Semicolon,
        "Quote" => K::Quote,
        "Backslash" => K::Backslash,
        "BracketLeft" => K::BracketLeft,
        "BracketRight" => K::BracketRight,
        "Backquote" => K::Backquote,
        _ => K::Unidentified,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::rc::Rc;

    use super::{
        BatchEffects, FrameBuffer, MAX_OUTBOUND_BYTES, OutboundQueue, ReceiveFlow,
        WebTransportExit, authenticated_websocket_protocols, await_js_promise_with_deadline,
        poisoned_framing_flow, token_from_webtransport_url, webtransport_eof_exit,
        webtransport_exit_flow,
    };
    use futures_channel::oneshot;
    use gloo_timers::future::TimeoutFuture;
    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ServerCapabilities};
    use phux_protocol::ids::{BootstrapId, ClientId, ResourceId, SessionId, StreamId, WindowId};
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
    use phux_protocol::wire::frame::{FrameKind, MAX_FRAME_LEN};
    use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
    use phux_vt_web::Vt;
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
    use web_sys::{HtmlCanvasElement, WebSocket};

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    fn poisoned_framing_stops_pump_and_invokes_close() {
        for length in [0, MAX_FRAME_LEN + 1] {
            let mut frames = FrameBuffer::new();
            frames.push(&length.to_be_bytes());
            assert!(frames.next_frame().is_none());

            let mut closed = false;
            let flow = poisoned_framing_flow(&frames, |_| closed = true);
            assert_eq!(flow, ReceiveFlow::Stop);
            assert!(closed, "poisoned framing must close its retained transport");
        }
    }

    #[wasm_bindgen_test]
    async fn every_webtransport_pump_exit_closes_and_disables_the_session() {
        let vt = Vt::load().await.expect("load engine");
        let mut partial = FrameBuffer::new();
        partial.push(&[0, 0, 0, 3, 0xaa]);
        let empty = FrameBuffer::new();
        let exits = [
            webtransport_eof_exit(&empty),
            webtransport_eof_exit(&partial),
            WebTransportExit::ReadError("read rejected"),
            WebTransportExit::MissingValue,
        ];
        assert_eq!(exits[0], WebTransportExit::CleanEof);
        assert_eq!(exits[1], WebTransportExit::PartialEof);

        for exit in exits {
            let terminal_id = ResourceId::local(1);
            let mut session = crate::Session::new(&vt, 80, 24);
            let hello = session.on_frame(FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new(),
                server_id: Vec::new(),
                selected_profile: BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::default(),
            });
            assert_eq!(hello.send.len(), 1);
            assert!(
                session
                    .on_frame(FrameKind::Attached {
                        attach_id: 1,
                        snapshot: SessionSnapshot::new(
                            SessionId::new(1),
                            WindowId::new(1),
                            terminal_id.clone(),
                        )
                        .with_resources(vec![ResourceInfo::new(
                            terminal_id.clone(),
                            WindowId::new(1),
                            80,
                            24,
                        )]),
                        initial_client_id: ClientId::new(1),
                    })
                    .fatal
                    .is_none()
            );
            let stream_id = StreamId::new(1).unwrap();
            let bootstrap_id = BootstrapId::new(1).unwrap();
            assert!(
                session
                    .on_frame(FrameKind::BootstrapBegin {
                        terminal_id: terminal_id.clone(),
                        stream_id,
                        bootstrap_id,
                        profile: phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
                        cols: 80,
                        rows: 24,
                        base_seq: 0,
                    })
                    .fatal
                    .is_none()
            );
            assert!(
                session
                    .on_frame(FrameKind::BootstrapChunk {
                        terminal_id: terminal_id.clone(),
                        stream_id,
                        bootstrap_id,
                        chunk_seq: 0,
                        payload: bytes::Bytes::from_static(b"ready"),
                    })
                    .fatal
                    .is_none()
            );
            assert!(
                session
                    .on_frame(FrameKind::BootstrapReady {
                        terminal_id: terminal_id.clone(),
                        stream_id,
                        bootstrap_id,
                        history_cursor: None,
                    })
                    .fatal
                    .is_none()
            );
            assert!(
                session
                    .on_frame(FrameKind::AttachReady { attach_id: 1 })
                    .fatal
                    .is_none()
            );
            let key = KeyEvent {
                action: KeyAction::Press,
                key: PhysicalKey::A,
                mods: ModSet::empty(),
                consumed_mods: ModSet::empty(),
                composing: false,
                text: Some("a".to_owned()),
                unshifted_codepoint: Some(u32::from(b'a')),
            };
            assert!(session.key_frame(key.clone()).is_some());

            let mut retained_transport_closed = false;
            let flow = webtransport_exit_flow(exit, |message, _protocol| {
                retained_transport_closed = true;
                session.fail_protocol(message);
            });
            assert_eq!(flow, ReceiveFlow::Stop);
            assert!(retained_transport_closed);
            assert!(session.is_failed());
            assert!(session.key_frame(key).is_none());

            let after_exit = session.on_frame(FrameKind::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq: 1,
                bytes: bytes::Bytes::from_static(b"ignored"),
            });
            assert!(after_exit.send.is_empty());
            assert!(!after_exit.render);
        }
    }

    #[wasm_bindgen_test]
    fn outbound_queue_is_ordered_and_byte_bounded() {
        let mut queue = OutboundQueue::default();
        queue.push(b"first").unwrap();
        queue.push(b"second").unwrap();
        assert_eq!(queue.bytes, 11);
        assert_eq!(queue.pop().as_deref(), Some(b"first".as_slice()));
        assert_eq!(queue.bytes, 11, "in-flight bytes remain budgeted");
        queue.complete(5);
        assert_eq!(queue.pop().as_deref(), Some(b"second".as_slice()));
        queue.complete(6);
        assert_eq!(queue.bytes, 0);

        queue.push(&vec![0; MAX_OUTBOUND_BYTES]).unwrap();
        assert!(queue.push(&[1]).is_err());
        queue.clear();
        assert_eq!(queue.bytes, 0);
        assert!(queue.frames.is_empty());
    }

    #[wasm_bindgen_test]
    fn fallback_token_carrier_is_strict_and_fragment_free() {
        assert_eq!(
            token_from_webtransport_url("https://host/session?x=1&token=00aF#ignored"),
            Some("00aF")
        );
        assert_eq!(
            token_from_webtransport_url("https://host/session?token=secret"),
            None
        );
        assert_eq!(
            token_from_webtransport_url("https://host/session#token=00"),
            None
        );
        assert_eq!(
            authenticated_websocket_protocols("00aF"),
            ["phux.v1", "phux.bearer.00aF"]
        );
    }

    #[wasm_bindgen_test]
    async fn unavailable_transport_promise_has_a_bounded_outcome() {
        let pending = js_sys::Promise::new(&mut |_, _| {});
        let result = await_js_promise_with_deadline(pending, "blackholed transport", 1).await;
        assert!(result.is_err());
    }

    #[wasm_bindgen_test]
    async fn cancellation_drops_a_pending_writer_promise() {
        let pending = js_sys::Promise::new(&mut |_, _| {});
        let (cancel_tx, cancel_rx) = oneshot::channel();
        cancel_tx.send(()).unwrap();
        let outcome = super::await_or_cancel(pending, cancel_rx).await;
        assert!(matches!(outcome, super::PromiseOutcome::Cancelled));
    }

    async fn closed_websocket() -> WebSocket {
        let ws = WebSocket::new("ws://127.0.0.1:47654/").expect("create test WebSocket");
        for _ in 0..200 {
            if ws.ready_state() == WebSocket::OPEN {
                break;
            }
            TimeoutFuture::new(10).await;
        }
        assert_eq!(ws.ready_state(), WebSocket::OPEN, "test WebSocket opened");
        ws.close().expect("close test WebSocket");
        for _ in 0..200 {
            if ws.ready_state() == WebSocket::CLOSED {
                return ws;
            }
            TimeoutFuture::new(10).await;
        }
        panic!("test WebSocket did not close");
    }

    fn test_canvas() -> HtmlCanvasElement {
        web_sys::window()
            .unwrap()
            .document()
            .unwrap()
            .create_element("canvas")
            .unwrap()
            .dyn_into()
            .unwrap()
    }

    #[wasm_bindgen_test]
    async fn closed_ws_send_and_repeated_reconnect_teardown_release_every_app() {
        let ws = closed_websocket().await;
        let document = web_sys::window().unwrap().document().unwrap();
        for _ in 0..8 {
            let tx = super::WireTx::Ws(super::WsTx::new(ws.clone()));
            let (app, _ready) = super::build_app(tx, test_canvas(), 80, 24, false)
                .await
                .unwrap();
            super::install_transport_failure_hook(&app);
            super::install_websocket_handlers(&app, &ws);
            super::install_keyboard(&app).unwrap();
            super::install_cursor_blink(&app).unwrap();
            app.borrow().self_owner.replace(Some(Rc::clone(&app)));
            let weak = Rc::downgrade(&app);

            super::send_handshake(&app);
            assert!(app.borrow().session.is_failed());
            assert!(app.borrow().self_owner.borrow().is_none());
            {
                let app = app.borrow();
                let bindings = app.bindings.borrow();
                assert!(bindings.websocket.is_none());
                assert!(bindings.keyboard.is_none());
                assert!(bindings.blink.is_none());
            }
            document
                .dispatch_event(&web_sys::KeyboardEvent::new("keydown").unwrap())
                .unwrap();
            drop(app);
            assert!(weak.upgrade().is_none(), "failed App must be released");
        }
    }

    #[wasm_bindgen_test]
    fn drained_frame_batch_coalesces_paint_requests() {
        let mut batch = BatchEffects::default();
        for _ in 0..2_048 {
            batch.merge(BatchEffects {
                flow: ReceiveFlow::Continue,
                render: true,
                badges: false,
            });
        }
        assert!(batch.render);
        assert_eq!(usize::from(batch.render), 1, "one paint follows the drain");
    }
}
