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
//! * **WebTransport** ([`run_with_fallback`]) — HTTP/3 over QUIC. One
//!   bidirectional stream carries length-prefixed frames (reassembled by
//!   [`FrameBuffer`](crate::framing::FrameBuffer)); falls back to WebSocket
//!   when the API or the endpoint is unavailable.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
#[cfg(test)]
use std::rc::Weak;

use futures_channel::oneshot;
use futures_util::future::{Either, select};
use gloo_timers::future::TimeoutFuture;
use phux_protocol::BootstrapProfile;
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::ModSet;
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::wire::frame::FrameKind;
use phux_vt_web::{Grid, Vt};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    BinaryType, CanvasRenderingContext2d, ClipboardEvent, CompositionEvent, HtmlCanvasElement,
    HtmlTextAreaElement, KeyboardEvent, MessageEvent, ReadableStreamDefaultReader, WebSocket,
    WebTransport, WritableStreamDefaultWriter,
};

use crate::framing::FrameBuffer;
use crate::input::WheelAction;
use crate::search::{Search, find_matches, reveal_row};
use crate::selection::{Selection, cell_at};
use crate::{Mark, Metrics, Overlay, render_cursor_row, render_selected};

mod find;
mod path_picker;

const CONNECT_DEADLINE_MS: u32 = 10_000;
const MAX_OUTBOUND_BYTES: usize = 1024 * 1024;
const PHUX_WS_PROTOCOL: &str = "phux.v1";
const PHUX_WS_BEARER_PREFIX: &str = "phux.bearer.";
const BOOTSTRAP_EXPIRY_POLL_MS: i32 = 1_000;

#[cfg(test)]
thread_local! {
    static RETAINED_APP_FOR_TEST: RefCell<Option<Weak<RefCell<App>>>> = const {
        RefCell::new(None)
    };
}

struct AttemptDeadline {
    end_ms: f64,
}

struct WebTransportAttempt {
    session: Option<WebTransport>,
}

struct WebSocketAttempt {
    socket: Option<WebSocket>,
}

impl WebSocketAttempt {
    fn new(socket: WebSocket) -> Self {
        Self {
            socket: Some(socket),
        }
    }

    fn socket(&self) -> &WebSocket {
        self.socket
            .as_ref()
            .expect("WebSocket attempt is still armed")
    }

    fn transfer_to_app(&mut self) {
        self.socket.take();
    }
}

impl Drop for WebSocketAttempt {
    fn drop(&mut self) {
        if let Some(socket) = self.socket.take() {
            let _ = socket.close();
        }
    }
}

impl WebTransportAttempt {
    fn new(session: WebTransport) -> Self {
        Self {
            session: Some(session),
        }
    }

    fn session(&self) -> &WebTransport {
        self.session
            .as_ref()
            .expect("WebTransport attempt is still armed")
    }

    fn transfer_to_app(&mut self) {
        self.session.take();
    }
}

impl Drop for WebTransportAttempt {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            session.close();
        }
    }
}

struct AppEstablishment {
    app: Option<Rc<RefCell<App>>>,
}

impl AppEstablishment {
    fn new(app: Rc<RefCell<App>>) -> Self {
        Self { app: Some(app) }
    }

    fn complete(mut self) -> Client {
        Client {
            app: self.app.take().expect("App establishment is still armed"),
        }
    }
}

impl Drop for AppEstablishment {
    fn drop(&mut self) {
        if let Some(app) = self.app.take() {
            app.borrow_mut().dispose();
        }
    }
}

impl AttemptDeadline {
    fn new() -> Result<Self, JsValue> {
        let now = monotonic_now_ms()?;
        Ok(Self {
            end_ms: now + f64::from(CONNECT_DEADLINE_MS),
        })
    }

    fn remaining_ms(&self) -> u32 {
        let remaining = monotonic_now_ms()
            .map(|now| (self.end_ms - now).ceil())
            .unwrap_or(0.0);
        remaining.clamp(0.0, f64::from(CONNECT_DEADLINE_MS)) as u32
    }
}

fn monotonic_now_ms() -> Result<f64, JsValue> {
    web_sys::window()
        .and_then(|window| window.performance())
        .map(|performance| performance.now())
        .ok_or_else(|| JsValue::from_str("monotonic browser clock unavailable"))
}

async fn load_vt() -> Result<Rc<Vt>, JsValue> {
    match select(
        Box::pin(Vt::load()),
        Box::pin(TimeoutFuture::new(CONNECT_DEADLINE_MS)),
    )
    .await
    {
        Either::Left((result, _)) => result,
        Either::Right(((), _)) => Err(JsValue::from_str("terminal engine load timed out")),
    }
}

/// Canvas attribute naming the connection state: `connected` once attached,
/// `disconnected` after the transport or protocol fails.
pub const CONNECTION_ATTRIBUTE: &str = "data-phux-connection";

/// Canvas attribute holding the title the program set (OSC 0/2).
pub const TITLE_ATTRIBUTE: &str = "data-phux-title";

/// Bubbling `CustomEvent` dispatched on the canvas when the program's title
/// changes; `detail` is the new title (empty when cleared).
pub const TITLE_EVENT: &str = "phux-title";

/// Bubbling `CustomEvent` dispatched on the canvas when the program rings
/// the bell (BEL). The canvas also flashes briefly, unless the page prefers
/// reduced motion.
pub const BELL_EVENT: &str = "phux-bell";

/// How long the visual bell shows, and the least time between two flashes.
const BELL_FLASH_MS: u32 = 150;

/// The visual bell: a translucent wash over the whole canvas.
const BELL_FLASH_FILL: &str = "rgba(255, 255, 255, 0.18)";

/// DOM id of the element that holds the focused pane's agent badges.
pub const BADGE_CONTAINER_ID: &str = "phux-agent-badges";

/// Connect to a phux server over WebSocket and render the attached terminal
/// into the given canvas, routing keyboard input back. Resolves only after the
/// aggregate attach reaches READY; handlers then run for the connection's lifetime.
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
    let vt = load_vt().await?;
    run_websocket(ws_url, None, canvas, cols, rows, false, &vt).await
}

/// Connect over WebSocket for the hosted live-demo Worker.
///
/// The first message must be the text `phux.session.v1` envelope. HELLO waits
/// until that preamble arrives; later binary messages are the phux wire.
///
/// # Errors
/// Fails if the engine, canvas, or WebSocket cannot be initialized, or if the
/// hosted preamble is missing or invalid.
pub async fn run_hosted(
    ws_url: &str,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
    callback: js_sys::Function,
) -> Result<Client, JsValue> {
    let vt = load_vt().await?;
    let deadline = AttemptDeadline::new()?;
    let ws = websocket(ws_url, None)?;
    let mut attempt = WebSocketAttempt::new(ws);
    attempt.socket().set_binary_type(BinaryType::Arraybuffer);

    let app_socket = attempt.socket().clone();
    let tx = WireTx::Ws(WsTx::new(app_socket.clone()));
    let (app, ready) = build_app(&vt, tx, canvas, cols, rows, false)?;
    let app_attempt = AppEstablishment::new(Rc::clone(&app));
    attempt.transfer_to_app();
    install_transport_failure_hook(&app);

    install_hosted_websocket_handlers(&app, &app_socket, callback);
    await_protocol_ready(&app, ready, deadline.remaining_ms()).await?;
    ensure_app_live(&app)?;

    install_input(&app)?;
    path_picker::install(&app)?;
    install_cursor_blink(&app)?;

    Ok(app_attempt.complete())
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
    let vt = load_vt().await?;
    run_websocket(ws_url, None, canvas, cols, rows, true, &vt).await
}

async fn run_websocket(
    ws_url: &str,
    bearer_hex: Option<&str>,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
    synthesized_only: bool,
    vt: &Rc<Vt>,
) -> Result<Client, JsValue> {
    let deadline = AttemptDeadline::new()?;
    let ws = websocket(ws_url, bearer_hex)?;
    let mut attempt = WebSocketAttempt::new(ws);
    attempt.socket().set_binary_type(BinaryType::Arraybuffer);

    let app_socket = attempt.socket().clone();
    let tx = WireTx::Ws(WsTx::new(app_socket.clone()));
    let (app, ready) = build_app(vt, tx, canvas, cols, rows, synthesized_only)?;
    let app_attempt = AppEstablishment::new(Rc::clone(&app));
    attempt.transfer_to_app();
    install_transport_failure_hook(&app);

    install_websocket_handlers(&app, &app_socket);
    await_protocol_ready(&app, ready, deadline.remaining_ms()).await?;
    ensure_app_live(&app)?;

    install_input(&app)?;
    path_picker::install(&app)?;
    install_cursor_blink(&app)?;

    Ok(app_attempt.complete())
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
    let fallback_token = fallback_token_from_webtransport_url(wt_url);
    if matches!(fallback_token, FallbackToken::Invalid) {
        return Err(JsValue::from_str(
            "WebTransport URL contains an invalid or duplicate token",
        ));
    }
    let vt = load_vt().await?;
    match run_webtransport_loaded(wt_url, canvas.clone(), cols, rows, &vt).await {
        Ok(client) => Ok(client),
        Err(_) => {
            web_sys::console::warn_1(&JsValue::from_str(
                "phux-web: WebTransport unavailable; falling back to WebSocket",
            ));
            let bearer = fallback_token.valid();
            run_websocket(ws_url, bearer, canvas, cols, rows, false, &vt).await
        }
    }
}

async fn run_webtransport_loaded(
    wt_url: &str,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
    vt: &Rc<Vt>,
) -> Result<Client, JsValue> {
    let deadline = AttemptDeadline::new()?;
    // `WebTransport::new` throws (rather than returning Err) when the API is
    // absent from the global scope; the `catch` binding surfaces both cases
    // as Err so the caller's fallback fires either way.
    let wt = WebTransport::new(wt_url)
        .map_err(|_| JsValue::from_str("WebTransport initialization failed"))?;
    let mut attempt = WebTransportAttempt::new(wt);
    if await_js_promise_with_deadline(
        attempt.session().ready(),
        "WebTransport readiness",
        deadline.remaining_ms(),
    )
    .await
    .is_err()
    {
        return Err(JsValue::from_str("WebTransport readiness failed"));
    }

    // One bidirectional stream carries the whole wire, mirroring the QUIC
    // transport's one-stream-per-connection contract.
    let stream: web_sys::WebTransportBidirectionalStream = match await_js_promise_with_deadline(
        attempt.session().create_bidirectional_stream(),
        "WebTransport stream creation",
        deadline.remaining_ms(),
    )
    .await
    {
        Ok(stream) => stream,
        Err(_) => return Err(JsValue::from_str("WebTransport stream creation failed")),
    };
    let writer = WritableStreamDefaultWriter::new(&stream.writable())
        .map_err(|_| JsValue::from_str("WebTransport writer initialization failed"))?;
    let reader = ReadableStreamDefaultReader::new(&stream.readable())
        .map_err(|_| JsValue::from_str("WebTransport reader initialization failed"))?;

    let reader_session = attempt.session().clone();
    let tx = WireTx::Wt(Rc::new(WtTx::new(writer, attempt.session().clone())));
    let (app, ready) = build_app(vt, tx, canvas, cols, rows, false)?;
    let app_attempt = AppEstablishment::new(Rc::clone(&app));
    attempt.transfer_to_app();
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
            run_webtransport_reader(app, reader, reader_session, cancel_rx).await;
        });
    }

    await_protocol_ready(&app, ready, deadline.remaining_ms()).await?;
    ensure_app_live(&app)?;

    install_input(&app)?;
    path_picker::install(&app)?;
    install_cursor_blink(&app)?;

    Ok(app_attempt.complete())
}

/// A live connection handle. The event handlers run for the connection's
/// lifetime; this lets a caller (or test) inspect the rendered grid.
pub struct Client {
    app: Rc<RefCell<App>>,
}

impl Client {
    /// Keep this connection alive past transport loss: after a failure, a
    /// fresh client redials (WebTransport first when `wt_url` is given,
    /// else the WebSocket) until one attaches, then supervises itself the
    /// same way. The client is retained by its own handlers from here on.
    pub fn enable_auto_reconnect(&self, wt_url: Option<&str>, ws_url: &str) {
        let (canvas, cols, rows) = {
            let app = self.app.borrow();
            let grid = app.session.grid();
            (app.canvas.clone(), grid.cols, grid.rows)
        };
        let config = ReconnectConfig {
            wt_url: wt_url.map(str::to_owned),
            ws_url: ws_url.to_owned(),
            canvas,
            cols,
            rows,
        };
        let mut app = self.app.borrow_mut();
        if app.session.is_failed() {
            drop(app);
            spawn_reconnect(config);
            return;
        }
        app.reconnect.get_mut().replace(config);
        app.self_owner.get_mut().replace(Rc::clone(&self.app));
    }

    pub(crate) fn retain_until_failure(&self) {
        let mut app = self.app.borrow_mut();
        if !app.session.is_failed() {
            app.self_owner.get_mut().replace(Rc::clone(&self.app));
            #[cfg(test)]
            RETAINED_APP_FOR_TEST.with(|retained| {
                retained.replace(Some(Rc::downgrade(&self.app)));
            });
        }
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

    /// Close the transport and drop browser handlers and timers.
    pub fn close(&self) {
        self.app.borrow_mut().dispose();
    }

    /// Resize the terminal viewport to `cols`x`rows` cells. The server
    /// resizes the pane (subject to its multi-client size policy) and the
    /// canvas follows the replica's new geometry; a reconnect reattaches at
    /// this size.
    pub fn resize(&self, cols: u16, rows: u16) {
        self.app.borrow().clear_selection();
        let mut app = self.app.borrow_mut();
        if let Some(config) = app.reconnect.get_mut().as_mut() {
            config.cols = cols.max(1);
            config.rows = rows.max(1);
        }
        let Some(frame) = app.session.resize_frame(cols, rows) else {
            return;
        };
        if let Err(message) = app.tx.send(&frame) {
            app.fail(&message);
        }
    }

    /// Privacy-safe terminal failure reason, if this connection ended.
    #[must_use]
    pub fn failure_reason(&self) -> Option<String> {
        self.app.borrow().failure_reason.borrow().clone()
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
    input: Option<InputBinding>,
    find: Option<find::FindBinding>,
    path_picker: Option<path_picker::PickerBinding>,
    blink: Option<BlinkBinding>,
    frame: Option<FrameBinding>,
    bootstrap_expiry: Option<BlinkBinding>,
    wt_reader_cancel: Option<oneshot::Sender<()>>,
}

impl AppBindings {
    fn dispose(&mut self) {
        if let Some(websocket) = self.websocket.take() {
            websocket.dispose();
        }
        if let Some(input) = self.input.take() {
            input.dispose();
        }
        if let Some(find) = self.find.take() {
            find.dispose();
        }
        if let Some(picker) = self.path_picker.take() {
            picker.dispose();
        }
        if let Some(blink) = self.blink.take() {
            blink.dispose();
        }
        if let Some(frame) = self.frame.take() {
            frame.dispose();
        }
        if let Some(expiry) = self.bootstrap_expiry.take() {
            expiry.dispose();
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

/// Class of the hidden `<textarea>` that owns terminal input.
pub const INPUT_SURFACE_CLASS: &str = "phux-web-input";

/// Class of the find bar mounted after the canvas (hidden until opened);
/// its field is its `input`, its match count `.phux-find-count`.
pub const FIND_BAR_CLASS: &str = "phux-find";

/// The input surface and every listener it and the canvas carry.
struct InputBinding {
    surface: HtmlTextAreaElement,
    listeners: Listeners,
}

impl InputBinding {
    fn dispose(self) {
        self.listeners.dispose();
        self.surface.remove();
    }
}

/// DOM event listeners removed together.
#[derive(Default)]
struct Listeners(Vec<Listener>);

struct Listener {
    target: web_sys::EventTarget,
    kind: &'static str,
    callback: Closure<dyn FnMut(web_sys::Event)>,
}

impl Listeners {
    fn listen(
        &mut self,
        target: &web_sys::EventTarget,
        kind: &'static str,
        handler: impl FnMut(web_sys::Event) + 'static,
    ) -> Result<(), JsValue> {
        let callback = Closure::<dyn FnMut(web_sys::Event)>::new(handler);
        target.add_event_listener_with_callback(kind, callback.as_ref().unchecked_ref())?;
        self.0.push(Listener {
            target: target.clone(),
            kind,
            callback,
        });
        Ok(())
    }

    fn dispose(self) {
        for listener in self.0 {
            let _ = listener.target.remove_event_listener_with_callback(
                listener.kind,
                listener.callback.as_ref().unchecked_ref(),
            );
        }
    }
}

/// The one animation-frame callback that paints, and its pending request.
struct FrameBinding {
    window: web_sys::Window,
    callback: Closure<dyn FnMut()>,
    pending: Cell<Option<i32>>,
}

impl FrameBinding {
    fn request(&self) {
        if self.pending.get().is_some() {
            return;
        }
        if let Ok(id) = self
            .window
            .request_animation_frame(self.callback.as_ref().unchecked_ref())
        {
            self.pending.set(Some(id));
        }
    }

    fn dispose(self) {
        if let Some(id) = self.pending.take() {
            let _ = self.window.cancel_animation_frame(id);
        }
        drop(self.callback);
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
    /// Fractional wheel rows not yet scrolled (trackpads send small deltas).
    wheel_carry: Cell<f64>,
    /// The grid the canvas shows, so a cursor blink redraws one row
    /// without reading the whole grid back from the engine.
    painted: RefCell<Option<Grid>>,
    /// The program title last published to the page.
    title: RefCell<String>,
    /// The search matches the canvas shows, so a cursor blink keeps them.
    painted_marks: RefCell<Vec<Mark>>,
    /// The mouse selection over the viewport, and whether a drag is live.
    selection: Cell<Option<Selection>>,
    selecting: Cell<bool>,
    /// Find in the terminal, and whether new output has made its matches
    /// stale.
    search: RefCell<Search>,
    search_stale: Cell<bool>,
    /// The button a press forwarded to the program holds down, and the
    /// last cell a mouse report named (motion reports once per cell).
    forwarded_button: Cell<Option<MouseButton>>,
    mouse_cell: Cell<Option<(u16, u16)>>,
    /// Whether the pointer shows a link under a held Command/Ctrl.
    link_hover: Cell<bool>,
    /// When the visual bell last started, on the monotonic clock.
    bell_started_ms: Cell<f64>,
    bindings: RefCell<AppBindings>,
    ready: RefCell<Option<oneshot::Sender<Result<(), String>>>>,
    failure_reason: RefCell<Option<String>>,
    reconnect: RefCell<Option<ReconnectConfig>>,
    self_owner: RefCell<Option<Rc<RefCell<App>>>>,
}

type AppReady = (Rc<RefCell<App>>, oneshot::Receiver<Result<(), String>>);

impl App {
    fn send(&self, frames: Vec<Vec<u8>>) -> Result<(), String> {
        for f in frames {
            self.tx.send(&f)?;
        }
        Ok(())
    }

    fn signal_ready(&self) {
        if let Some(ready) = self.ready.borrow_mut().take() {
            self.mark_connection("connected");
            let _ = ready.send(Ok(()));
        }
    }

    /// Reflect the connection on the canvas as [`CONNECTION_ATTRIBUTE`], so
    /// a page can dim or badge a stale terminal while a reconnect is pending.
    fn mark_connection(&self, state: &str) {
        let _ = self.canvas.set_attribute(CONNECTION_ATTRIBUTE, state);
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
        self.mark_connection("disconnected");
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

    /// Move the input surface over the cursor cell, so an IME candidate
    /// window opens where the text will land.
    fn place_input_surface(&self) {
        let bindings = self.bindings.borrow();
        let Some(input) = bindings.input.as_ref() else {
            return;
        };
        let rect = self.canvas.get_bounding_client_rect();
        let grid = self.session.grid();
        let (css_w, css_h) = self.css_cell(&rect);
        let left = rect.left() + f64::from(grid.cursor_col) * css_w;
        let top = rect.top() + f64::from(grid.cursor_row) * css_h;
        let _ = input.surface.set_attribute(
            "style",
            &format!("{INPUT_SURFACE_STYLE}left:{left}px;top:{top}px;"),
        );
    }

    /// Paint on the next animation frame. Frames arriving in a burst (a
    /// flood of output, a WebTransport chunk of many frames) share one
    /// paint, and a hidden tab paints nothing until it is shown again.
    fn request_paint(&self) {
        if let Some(frame) = self.bindings.borrow().frame.as_ref() {
            frame.request();
        }
    }

    /// Toggle the cursor blink phase and redraw only the cursor's row.
    fn blink(&self) {
        self.cursor_on.set(!self.cursor_on.get());
        if self.session.viewport_scrolled() {
            return;
        }
        if self.bell_showing() {
            // The flash covers the cursor cell too; its end repaints it.
            return;
        }
        if let Some(grid) = self.painted.borrow().as_ref() {
            let marks = self.painted_marks.borrow();
            let overlay = Overlay {
                selected: self.selected_cells(grid.cols),
                marks: &marks,
            };
            render_cursor_row(
                &self.ctx,
                grid,
                &self.metrics,
                self.cursor_on.get(),
                &overlay,
            );
        }
    }

    fn paint(&self) {
        if !self.session.render_visible() {
            return;
        }
        let grid = self.session.grid();
        let marks = self.search_marks(grid.cols, grid.rows);
        // Keep the canvas sized to the grid (handles server-side resizes).
        let w = u32::from(grid.cols) * (self.metrics.cell_w as u32);
        let h = u32::from(grid.rows) * (self.metrics.cell_h as u32);
        if self.canvas.width() != w {
            self.canvas.set_width(w);
        }
        if self.canvas.height() != h {
            self.canvas.set_height(h);
        }
        // The cursor belongs to the live screen, not a scrolled-back view.
        let cursor = self.cursor_on.get() && !self.session.viewport_scrolled();
        let overlay = Overlay {
            selected: self.selected_cells(grid.cols),
            marks: &marks,
        };
        render_selected(&self.ctx, &grid, &self.metrics, cursor, &overlay);
        if self.bell_showing() {
            self.ctx.set_fill_style_str(BELL_FLASH_FILL);
            self.ctx.fill_rect(0.0, 0.0, f64::from(w), f64::from(h));
        }
        self.painted.replace(Some(grid));
        self.painted_marks.replace(marks);
        self.publish_title();
    }

    /// Whether the visual bell is on screen now.
    fn bell_showing(&self) -> bool {
        monotonic_now_ms()
            .is_ok_and(|now| now - self.bell_started_ms.get() < f64::from(BELL_FLASH_MS))
    }

    /// The highlights of the open search's matches on screen, re-running
    /// the search first when output has changed the screen since.
    fn search_marks(&self, cols: u16, rows: u16) -> Vec<Mark> {
        if self.search.borrow().query().is_empty() {
            return Vec::new();
        }
        if self.search_stale.get() {
            let query = self.search.borrow().query().to_owned();
            self.run_search(&query);
        }
        let Some(terminal) = self.session.terminal() else {
            return Vec::new();
        };
        let top = terminal.scrollbar().offset;
        self.search.borrow().marks(top, rows, cols)
    }

    /// Search the replica's whole screen for `query` and show the count.
    fn run_search(&self, query: &str) {
        self.search_stale.set(false);
        let (matches, truncated) = match self.session.terminal() {
            Some(terminal) if !query.is_empty() => {
                let vt = self.session.vt();
                find_matches(&terminal.screen_rows(), query, |ch| vt.codepoint_width(ch))
            }
            _ => (Vec::new(), false),
        };
        let label = {
            let mut search = self.search.borrow_mut();
            search.set_results(query, matches, truncated);
            search.label()
        };
        find::set_label(self, &label);
    }

    /// Scroll the current match into view, if it is off screen, and repaint.
    fn reveal_current_match(&self) {
        let current = self.search.borrow().current();
        if let (Some(found), Some(terminal)) = (current, self.session.terminal()) {
            let bar = terminal.scrollbar();
            if let Some(top) = reveal_row(found.row, bar.offset, bar.len, bar.total) {
                terminal.scroll_to_row(top);
                self.clear_selection();
            }
        }
        self.request_paint();
    }

    /// Row-major indices of the selected cells, empty without a selection.
    fn selected_cells(&self, cols: u16) -> std::ops::Range<usize> {
        self.selection
            .get()
            .filter(|selection| !selection.is_click())
            .map_or(0..0, |selection| selection.cells(cols))
    }

    /// The selected text, if anything is selected, as the engine copies it.
    /// Read from the replica's current viewport, which the canvas shows by
    /// the next frame.
    fn selected_text(&self) -> Option<String> {
        let selection = self.selection.get().filter(|s| !s.is_click())?;
        let (cols, _) = self.session.dims();
        let clamp = |(col, row): (u16, u16)| (col.min(cols.saturating_sub(1)), row);
        self.session
            .terminal()?
            .selection_text(clamp(selection.anchor), clamp(selection.head))
    }

    /// Drop the selection (it names viewport cells, which input, scrolling,
    /// and resizing move out from under it), repainting if one was shown.
    fn clear_selection(&self) {
        if self.selection.take().is_some() {
            self.request_paint();
        }
    }

    /// A cell's drawn size on the page, in CSS pixels: the canvas's cell
    /// size scaled by however CSS, page zoom, or the device pixel ratio
    /// shows the canvas.
    fn css_cell(&self, rect: &web_sys::DomRect) -> (f64, f64) {
        let scale_x = rect.width() / f64::from(self.canvas.width().max(1));
        let scale_y = rect.height() / f64::from(self.canvas.height().max(1));
        (self.metrics.cell_w * scale_x, self.metrics.cell_h * scale_y)
    }

    /// The viewport cell under a pointer event.
    fn cell_under(&self, event: &web_sys::MouseEvent) -> (u16, u16) {
        let rect = self.canvas.get_bounding_client_rect();
        let (css_w, css_h) = self.css_cell(&rect);
        let (cols, rows) = self.session.dims();
        cell_at(
            event.client_x() - rect.left(),
            event.client_y() - rect.top(),
            css_w,
            css_h,
            cols,
            rows,
        )
    }

    /// The pointer in pixels of the cell grid the viewport reports to the
    /// server ([`Metrics::cell_px`] per cell), clamped to the grid: what
    /// `INPUT_MOUSE` carries, and what the server's mouse encoder divides by
    /// the same cell size.
    fn surface_pixels(&self, event: &web_sys::MouseEvent) -> (f64, f64) {
        let rect = self.canvas.get_bounding_client_rect();
        let (css_w, css_h) = self.css_cell(&rect);
        let (cell_w, cell_h) = self.metrics.cell_px();
        let (cols, rows) = self.session.dims();
        (
            crate::input::surface_pixel(event.client_x() - rect.left(), css_w, cell_w, cols),
            crate::input::surface_pixel(event.client_y() - rect.top(), css_h, cell_h, rows),
        )
    }

    /// Mirror a changed program title onto the canvas and announce it, so a
    /// page can show it (the standalone page sets its document title).
    fn publish_title(&self) {
        let title = self.session.title();
        if *self.title.borrow() == title {
            return;
        }
        let _ = self.canvas.set_attribute(TITLE_ATTRIBUTE, &title);
        let init = web_sys::CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&JsValue::from_str(&title));
        if let Ok(event) = web_sys::CustomEvent::new_with_event_init_dict(TITLE_EVENT, &init) {
            let _ = self.canvas.dispatch_event(&event);
        }
        self.title.replace(title);
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.dispose();
    }
}

/// Grab the canvas context and assemble an [`App`] around a preloaded engine
/// and established transport send half.
fn build_app(
    vt: &Rc<Vt>,
    tx: WireTx,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
    synthesized_only: bool,
) -> Result<AppReady, JsValue> {
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("no 2D context"))?
        .dyn_into()?;

    let (ready_tx, ready_rx) = oneshot::channel();
    let metrics = Metrics::default();
    let mut session = if synthesized_only {
        crate::Session::new_synthesized_compat(vt, cols, rows)
    } else {
        crate::Session::new(vt, cols, rows)
    };
    // Mouse reports carry positions on the grid the canvas draws; the
    // viewport tells the server that grid's cell size.
    let (cell_w, cell_h) = metrics.cell_px();
    session.set_cell_size(cell_w, cell_h);
    let app = Rc::new(RefCell::new(App {
        session,
        tx,
        canvas,
        ctx,
        metrics,
        cursor_on: Cell::new(true),
        wheel_carry: Cell::new(0.0),
        painted: RefCell::new(None),
        title: RefCell::new(String::new()),
        painted_marks: RefCell::new(Vec::new()),
        selection: Cell::new(None),
        selecting: Cell::new(false),
        search: RefCell::new(Search::default()),
        search_stale: Cell::new(false),
        forwarded_button: Cell::new(None),
        mouse_cell: Cell::new(None),
        link_hover: Cell::new(false),
        bell_started_ms: Cell::new(f64::NEG_INFINITY),
        bindings: RefCell::new(AppBindings::default()),
        ready: RefCell::new(Some(ready_tx)),
        failure_reason: RefCell::new(None),
        reconnect: RefCell::new(None),
        self_owner: RefCell::new(None),
    }));
    install_bootstrap_expiry(&app)?;
    install_frame(&app)?;
    Ok((app, ready_rx))
}

struct ReconnectConfig {
    wt_url: Option<String>,
    ws_url: String,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
}

fn spawn_reconnect(config: ReconnectConfig) {
    wasm_bindgen_futures::spawn_local(async move {
        TimeoutFuture::new(250).await;
        loop {
            let canvas = config.canvas.clone();
            let attempt = match config.wt_url.as_deref() {
                Some(wt_url) => {
                    run_with_fallback(wt_url, &config.ws_url, canvas, config.cols, config.rows)
                        .await
                }
                None => run(&config.ws_url, canvas, config.cols, config.rows).await,
            };
            match attempt {
                Ok(client) => {
                    client.enable_auto_reconnect(config.wt_url.as_deref(), &config.ws_url);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FallbackToken<'a> {
    Missing,
    Valid(&'a str),
    Invalid,
}

impl<'a> FallbackToken<'a> {
    const fn valid(self) -> Option<&'a str> {
        match self {
            Self::Valid(token) => Some(token),
            Self::Missing | Self::Invalid => None,
        }
    }
}

fn fallback_token_from_webtransport_url(url: &str) -> FallbackToken<'_> {
    let Some(query) = url.split_once('?').map(|(_, rest)| rest) else {
        return FallbackToken::Missing;
    };
    let mut tokens = query
        .split('#')
        .next()
        .unwrap_or_default()
        .split('&')
        .filter(|part| *part == "token" || part.starts_with("token="));
    let Some(carrier) = tokens.next() else {
        return FallbackToken::Missing;
    };
    let Some(token) = carrier.strip_prefix("token=") else {
        return FallbackToken::Invalid;
    };
    if tokens.next().is_some()
        || token.is_empty()
        || token.len() % 2 != 0
        || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return FallbackToken::Invalid;
    }
    FallbackToken::Valid(token)
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

fn install_hosted_websocket_handlers(
    app: &Rc<RefCell<App>>,
    ws: &WebSocket,
    callback: js_sys::Function,
) {
    let callback = Rc::new(callback);
    let hello_sent = Rc::new(Cell::new(false));
    let preamble_done = Rc::new(Cell::new(false));
    let onopen = Closure::new(|| {});
    let onmessage = hosted_message_callback(
        app,
        Rc::clone(&callback),
        Rc::clone(&hello_sent),
        Rc::clone(&preamble_done),
    );
    let onerror = hosted_error_callback(app, Rc::clone(&callback));
    let onclose = hosted_close_callback(app, callback);

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

fn hosted_message_callback(
    app: &Rc<RefCell<App>>,
    callback: Rc<js_sys::Function>,
    hello_sent: Rc<Cell<bool>>,
    preamble_done: Rc<Cell<bool>>,
) -> Closure<dyn FnMut(MessageEvent)> {
    let weak = Rc::downgrade(app);
    Closure::new(move |event: MessageEvent| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.borrow().session.is_failed() {
            return;
        }
        if let Some(text) = event.data().as_string() {
            if preamble_done.replace(true) {
                emit_hosted_error(&callback, "protocol");
                close_with_protocol_error(&app, "unexpected text frame after hosted preamble");
                return;
            }
            match js_sys::JSON::parse(&text) {
                Ok(value) => {
                    emit_hosted(&callback, &value);
                    if !hello_sent.replace(true) {
                        send_handshake(&app);
                    }
                }
                Err(_) => {
                    emit_hosted_error(&callback, "client");
                    close_with_protocol_error(&app, "hosted session preamble was not JSON");
                }
            }
            return;
        }
        if !preamble_done.get() {
            emit_hosted_error(&callback, "protocol");
            close_with_protocol_error(&app, "hosted session preamble missing");
            return;
        }
        let framed = js_sys::Uint8Array::new(&event.data()).to_vec();
        match decode_server_frame(&app, &framed) {
            Ok(frame) => {
                let _ = handle_frame(&app, frame);
            }
            Err(message) => {
                emit_hosted_error(&callback, "protocol");
                close_with_protocol_error(&app, &message);
            }
        }
    })
}

fn hosted_error_callback(
    app: &Rc<RefCell<App>>,
    callback: Rc<js_sys::Function>,
) -> Closure<dyn FnMut(web_sys::Event)> {
    let weak = Rc::downgrade(app);
    Closure::new(move |_| {
        emit_hosted_error(&callback, "transport");
        if let Some(app) = weak.upgrade() {
            close_with_transport_error(&app, "WebSocket transport error");
        }
    })
}

fn hosted_close_callback(
    app: &Rc<RefCell<App>>,
    callback: Rc<js_sys::Function>,
) -> Closure<dyn FnMut(web_sys::Event)> {
    let weak = Rc::downgrade(app);
    Closure::new(move |event: web_sys::Event| {
        let close = event.dyn_into::<web_sys::CloseEvent>();
        let (code, was_clean) = close
            .ok()
            .map_or((1006, false), |event| (event.code(), event.was_clean()));
        emit_hosted_close(&callback, code, was_clean);
        if let Some(app) = weak.upgrade() {
            close_with_transport_error(&app, "WebSocket closed by peer");
        }
    })
}

fn emit_hosted(callback: &js_sys::Function, value: &JsValue) {
    let _ = callback.call1(&JsValue::NULL, value);
}

fn emit_hosted_error(callback: &js_sys::Function, category: &str) {
    let event = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &event,
        &JsValue::from_str("type"),
        &JsValue::from_str("error"),
    );
    let _ = js_sys::Reflect::set(
        &event,
        &JsValue::from_str("category"),
        &JsValue::from_str(category),
    );
    emit_hosted(callback, &event);
}

fn emit_hosted_close(callback: &js_sys::Function, code: u16, was_clean: bool) {
    let event = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &event,
        &JsValue::from_str("type"),
        &JsValue::from_str("close"),
    );
    let _ = js_sys::Reflect::set(&event, &JsValue::from_str("code"), &JsValue::from(code));
    let _ = js_sys::Reflect::set(
        &event,
        &JsValue::from_str("category"),
        &JsValue::from_str(hosted_close_category(code)),
    );
    let _ = js_sys::Reflect::set(
        &event,
        &JsValue::from_str("wasClean"),
        &JsValue::from(was_clean),
    );
    emit_hosted(callback, &event);
}

fn hosted_close_category(code: u16) -> &'static str {
    match code {
        1000 => "normal",
        1001 => "going-away",
        1002 | 1003 | 1007 => "protocol",
        1008 | 4003 => "bad-request",
        1011 | 4011 => "server",
        1013 => "unavailable",
        4001 => "capacity",
        4002 => "rate-limited",
        4004 => "idle",
        4005 => "expired",
        4007 => "unauthorized",
        _ => "network",
    }
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
    deadline_ms: u32,
) -> Result<(), JsValue> {
    match select(ready, TimeoutFuture::new(deadline_ms)).await {
        Either::Left((Ok(Ok(())), _)) => Ok(()),
        Either::Left((Ok(Err(message)), _)) => Err(JsValue::from_str(&message)),
        Either::Left((Err(_), _)) => {
            Err(JsValue::from_str("connection closed before ATTACH_READY"))
        }
        Either::Right(((), _)) => {
            close_with_transport_error(app, "protocol attach timed out");
            Err(JsValue::from_str("protocol attach timed out"))
        }
    }
}

fn ensure_app_live(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    if app.borrow().session.is_failed() {
        Err(JsValue::from_str(
            "connection failed while completing protocol readiness",
        ))
    } else {
        Ok(())
    }
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
    bell: bool,
}

impl Default for BatchEffects {
    fn default() -> Self {
        Self {
            flow: ReceiveFlow::Continue,
            render: false,
            badges: false,
            bell: false,
        }
    }
}

impl BatchEffects {
    fn merge(&mut self, other: Self) {
        self.render |= other.render;
        self.badges |= other.badges;
        self.bell |= other.bell;
        if matches!(other.flow, ReceiveFlow::Stop) {
            self.flow = ReceiveFlow::Stop;
        }
    }

    fn paint(self, app: &Rc<RefCell<App>>) {
        {
            let app = app.borrow();
            if self.render {
                app.search_stale.set(true);
                app.request_paint();
            }
            if self.badges {
                app.paint_badges();
            }
        }
        // One bell per batch: a flood of BELs flashes and announces once.
        if self.bell {
            ring_bell(app);
        }
    }
}

/// The program rang the bell: announce it with [`BELL_EVENT`], then flash
/// the canvas unless the page prefers reduced motion or a flash is showing.
fn ring_bell(app: &Rc<RefCell<App>>) {
    let canvas = app.borrow().canvas.clone();
    let init = web_sys::CustomEventInit::new();
    init.set_bubbles(true);
    if let Ok(event) = web_sys::CustomEvent::new_with_event_init_dict(BELL_EVENT, &init) {
        // Dispatched with no borrow held: a listener may call the client.
        let _ = canvas.dispatch_event(&event);
    }
    {
        let app = app.borrow();
        let Ok(now) = monotonic_now_ms() else {
            return;
        };
        if prefers_reduced_motion() || app.bell_showing() {
            return;
        }
        app.bell_started_ms.set(now);
        app.request_paint();
    }
    let weak = Rc::downgrade(app);
    wasm_bindgen_futures::spawn_local(async move {
        TimeoutFuture::new(BELL_FLASH_MS).await;
        if let Some(app) = weak.upgrade() {
            app.borrow().request_paint();
        }
    });
}

/// Whether the page asks for reduced motion (`prefers-reduced-motion`).
fn prefers_reduced_motion() -> bool {
    web_sys::window()
        .and_then(|window| {
            window
                .match_media("(prefers-reduced-motion: reduce)")
                .ok()
                .flatten()
        })
        .is_some_and(|query| query.matches())
}

fn apply_frame(app: &Rc<RefCell<App>>, frame: FrameKind) -> BatchEffects {
    let path_reply = matches!(
        frame,
        FrameKind::PathResults { .. }
            | FrameKind::Attached { .. }
            | FrameKind::ResourceClosed { .. }
    );
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
    if a.session.is_attach_ready() {
        a.signal_ready();
    }
    if path_reply {
        path_picker::paint(&a);
    }
    BatchEffects {
        flow: ReceiveFlow::Continue,
        render: outcome.render,
        badges: outcome.badges,
        bell: outcome.bell,
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

/// Inline style of the input surface: invisible and inert to the pointer,
/// but a real focusable text control so the browser runs IME composition,
/// dead keys, mobile keyboards, and clipboard paste against it.
const INPUT_SURFACE_STYLE: &str = "position:fixed;width:1px;height:1px;padding:0;border:0;\
margin:0;opacity:0;resize:none;overflow:hidden;white-space:pre;pointer-events:none;\
caret-color:transparent;";

/// Keyboard, IME, and paste: a hidden `<textarea>` beside the canvas owns
/// terminal input. Focusing or clicking the canvas focuses it, so only keys
/// typed at the terminal become `INPUT_KEY`; the embedding page's other
/// controls keep theirs. Keydowns the terminal encodes are sent and
/// cancelled; composed text arrives as `compositionend` or `input`, and a
/// clipboard paste as one `INPUT_PASTE`.
fn install_input(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let canvas = app.borrow().canvas.clone();
    let document = canvas
        .owner_document()
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let surface = create_input_surface(&document, &canvas)?;
    let mut binding = InputBinding {
        surface: surface.clone(),
        listeners: Listeners::default(),
    };
    let handlers: [(&str, InputHandler); 8] = [
        ("keydown", on_keydown),
        ("focus", on_focus_change),
        ("blur", on_focus_change),
        ("compositionstart", |app, _, _| {
            app.borrow().place_input_surface()
        }),
        ("compositionend", on_composition_end),
        ("input", on_text_input),
        ("paste", on_paste),
        ("copy", on_copy),
    ];
    let canvas_handlers: [(&str, InputHandler); 8] = [
        ("focus", focus_surface),
        ("mousedown", focus_surface),
        ("wheel", on_wheel),
        ("pointerdown", on_pointer),
        ("pointermove", on_pointer),
        ("pointerup", on_pointer),
        ("pointercancel", on_pointer),
        ("contextmenu", on_context_menu),
    ];
    let targets: [(&web_sys::EventTarget, &[(&str, InputHandler)]); 2] = [
        (surface.as_ref(), &handlers),
        (canvas.as_ref(), &canvas_handlers),
    ];
    for (target, handlers) in targets {
        for &(kind, handler) in handlers {
            let weak = Rc::downgrade(app);
            let surface = surface.clone();
            binding.listeners.listen(target, kind, move |event| {
                if let Some(app) = weak.upgrade() {
                    handler(&app, &event, &surface);
                }
            })?;
        }
    }

    let old = app.borrow().bindings.borrow_mut().input.replace(binding);
    if let Some(old) = old {
        old.dispose();
    }
    find::install(app, &document)?;
    if focus_is_idle(&document) {
        app.borrow().place_input_surface();
        let _ = surface.focus();
    }
    Ok(())
}

type InputHandler = fn(&Rc<RefCell<App>>, &web_sys::Event, &HtmlTextAreaElement);

/// The hidden, focusable text control, mounted beside the canvas.
fn create_input_surface(
    document: &web_sys::Document,
    canvas: &HtmlCanvasElement,
) -> Result<HtmlTextAreaElement, JsValue> {
    let surface: HtmlTextAreaElement = document.create_element("textarea")?.dyn_into()?;
    surface.set_class_name(INPUT_SURFACE_CLASS);
    for (name, value) in [
        ("aria-label", "Terminal input"),
        ("autocomplete", "off"),
        ("autocorrect", "off"),
        ("autocapitalize", "off"),
        ("spellcheck", "false"),
        ("tabindex", "-1"),
        ("style", INPUT_SURFACE_STYLE),
    ] {
        surface.set_attribute(name, value)?;
    }
    let parent = canvas
        .parent_element()
        .or_else(|| document.body().map(Into::into))
        .ok_or_else(|| JsValue::from_str("no element to host the input surface"))?;
    parent.append_child(&surface)?;
    Ok(surface)
}

/// Whether nothing holds focus: a fresh page, or a reconnect whose previous
/// surface held it. The terminal takes focus then, so typing works without
/// a click, but never from another control the user is in.
fn focus_is_idle(document: &web_sys::Document) -> bool {
    document
        .active_element()
        .is_none_or(|active| document.body().is_some_and(|body| active == *body.as_ref()))
}

/// Focusing or pressing on the canvas focuses the input surface.
fn focus_surface(_: &Rc<RefCell<App>>, event: &web_sys::Event, surface: &HtmlTextAreaElement) {
    if event.type_() == "mousedown" {
        // Keep the canvas from taking focus back from the surface.
        event.prevent_default();
    }
    let _ = surface.focus();
}

/// The pointer over the canvas, in precedence order: Command/Ctrl+click
/// opens a link; while the program tracks the mouse, presses, releases, and
/// the motion it asked for reach it as `INPUT_MOUSE`; otherwise (and always
/// with Shift held) a primary-button drag selects text locally.
fn on_pointer(app: &Rc<RefCell<App>>, event: &web_sys::Event, _: &HtmlTextAreaElement) {
    let Some(event) = event.dyn_ref::<web_sys::PointerEvent>() else {
        return;
    };
    if event.type_() == "pointermove" {
        hover_link(&app.borrow(), event);
    }
    if open_link(app, event) || forward_pointer(app, event) {
        return;
    }
    select_with_pointer(app, event);
}

/// Whether a pointer event carries the link modifier: Command on macOS,
/// Ctrl elsewhere (either is accepted), without Shift.
fn link_modifier(event: &web_sys::MouseEvent) -> bool {
    (event.meta_key() || event.ctrl_key()) && !event.shift_key()
}

/// The link at a viewport cell that a click may open: the program's OSC 8
/// hyperlink when it set one (even one this client refuses to open), else a
/// plain `http`, `https`, or `mailto` URL in the row's text.
fn link_at(app: &App, (col, row): (u16, u16)) -> Option<String> {
    let terminal = app.session.terminal()?;
    if let Some(uri) = terminal.hyperlink_at(col, row) {
        return crate::links::allowed_link(&uri).map(str::to_owned);
    }
    let painted = app.painted.borrow();
    let grid = painted.as_ref()?;
    let cols = usize::from(grid.cols);
    let start = usize::from(row) * cols;
    let cells = grid.cells.get(start..start + cols)?;
    let text: Vec<char> = cells.iter().map(|cell| cell.ch).collect();
    crate::links::url_at(&text, usize::from(col))
}

/// Command/Ctrl+click on a link opens it in a new tab, with no opener or
/// referrer. Returns whether the click was a link's.
fn open_link(app: &Rc<RefCell<App>>, event: &web_sys::PointerEvent) -> bool {
    if event.type_() != "pointerdown" || event.button() != 0 || !link_modifier(event) {
        return false;
    }
    let url = {
        let app = app.borrow();
        link_at(&app, app.cell_under(event))
    };
    let Some(url) = url else {
        return false;
    };
    event.prevent_default();
    if let Some(window) = web_sys::window() {
        let _ = window.open_with_url_and_target_and_features(&url, "_blank", "noopener,noreferrer");
    }
    true
}

/// Show a pointer over a link while the link modifier is held.
fn hover_link(app: &App, event: &web_sys::PointerEvent) {
    let over_link = link_modifier(event) && link_at(app, app.cell_under(event)).is_some();
    if app.link_hover.replace(over_link) == over_link {
        return;
    }
    let style = app.canvas.style();
    let _ = if over_link {
        style.set_property("cursor", "pointer")
    } else {
        style.remove_property("cursor").map(|_| ())
    };
}

/// Whether pointer input goes to the program: it tracks the mouse, the
/// viewport shows the live screen its coordinates name, and Shift (the
/// local-selection override) is not held.
fn program_takes_mouse(app: &App, shift: bool) -> bool {
    !shift
        && !app.session.viewport_scrolled()
        && app
            .session
            .terminal()
            .is_some_and(phux_vt_web::Terminal::mouse_tracking)
}

/// Where one pointer event goes.
enum PointerRoute {
    /// Local selection handles it.
    Local,
    /// The program owns the pointer but asked for no report of this event.
    Swallow,
    /// Report it to the program.
    Report(MouseAction, MouseButton),
}

/// Forward a press, release, or reported motion to a mouse-tracking
/// program. Returns whether the event was the program's.
fn forward_pointer(app: &Rc<RefCell<App>>, event: &web_sys::PointerEvent) -> bool {
    let route = route_pointer(&app.borrow(), event);
    match route {
        PointerRoute::Local => false,
        PointerRoute::Swallow => true,
        PointerRoute::Report(action, button) => {
            send_mouse(app, event, action, button);
            true
        }
    }
}

fn route_pointer(app: &App, event: &web_sys::PointerEvent) -> PointerRoute {
    if app.selecting.get() {
        return PointerRoute::Local;
    }
    match event.type_().as_str() {
        "pointerdown" => route_press(app, event),
        "pointerup" | "pointercancel" => route_release(app, event),
        "pointermove" => route_motion(app, event),
        _ => PointerRoute::Local,
    }
}

/// A press goes to a tracking program, which then owns the drag.
fn route_press(app: &App, event: &web_sys::PointerEvent) -> PointerRoute {
    let Some(button) = crate::input::mouse_button(event.button()) else {
        return PointerRoute::Local;
    };
    if !program_takes_mouse(app, event.shift_key()) {
        return PointerRoute::Local;
    }
    app.forwarded_button.set(Some(button));
    // Keep receiving moves and the release outside the canvas.
    let _ = app.canvas.set_pointer_capture(event.pointer_id());
    app.clear_selection();
    PointerRoute::Report(MouseAction::Press, button)
}

/// A release (or cancel) ends a forwarded press, even if tracking stopped.
fn route_release(app: &App, event: &web_sys::PointerEvent) -> PointerRoute {
    let Some(pressed) = app.forwarded_button.take() else {
        return PointerRoute::Local;
    };
    let released = crate::input::mouse_button(event.button()).unwrap_or(pressed);
    PointerRoute::Report(MouseAction::Release, released)
}

/// Motion reaches the program once per cell, when its mode reports it.
fn route_motion(app: &App, event: &web_sys::PointerEvent) -> PointerRoute {
    let forwarding = app.forwarded_button.get().is_some();
    if !forwarding && !program_takes_mouse(app, event.shift_key()) {
        return PointerRoute::Local;
    }
    let Some(terminal) = app.session.terminal() else {
        return PointerRoute::Local;
    };
    let dragging = crate::input::held_button(event.buttons());
    let wanted = crate::input::reports_motion(
        terminal.dec_mode(1003),
        terminal.dec_mode(1002),
        dragging.is_some(),
    );
    if !wanted || app.mouse_cell.get() == Some(app.cell_under(event)) {
        return PointerRoute::Swallow;
    }
    PointerRoute::Report(
        MouseAction::Motion,
        dragging.unwrap_or(MouseButton::Unknown),
    )
}

/// Send one mouse report at the pointer's cell-grid pixel position.
fn send_mouse(
    app: &Rc<RefCell<App>>,
    event: &web_sys::MouseEvent,
    action: MouseAction,
    button: MouseButton,
) {
    let (x, y) = {
        let app = app.borrow();
        app.mouse_cell.set(Some(app.cell_under(event)));
        app.surface_pixels(event)
    };
    let mut mods = ModSet::empty();
    if event.ctrl_key() {
        mods |= ModSet::CTRL;
    }
    if event.alt_key() {
        mods |= ModSet::ALT;
    }
    if event.shift_key() {
        mods |= ModSet::SHIFT;
    }
    send_input(
        app,
        [InputEvent::Mouse(MouseEvent {
            action,
            button,
            mods,
            x,
            y,
        })],
    );
}

/// The context menu is the program's while it tracks the mouse, so a
/// right-click reaches it; Shift+right-click still opens the browser's.
fn on_context_menu(app: &Rc<RefCell<App>>, event: &web_sys::Event, _: &HtmlTextAreaElement) {
    let shift = event
        .dyn_ref::<web_sys::MouseEvent>()
        .is_some_and(web_sys::MouseEvent::shift_key);
    if program_takes_mouse(&app.borrow(), shift) {
        event.prevent_default();
    }
}

/// A primary-button drag over the canvas selects cells; a click clears.
fn select_with_pointer(app: &Rc<RefCell<App>>, event: &web_sys::PointerEvent) {
    let app = app.borrow();
    let cell = app.cell_under(event);
    match event.type_().as_str() {
        "pointerdown" if event.button() == 0 => {
            app.selection.set(Some(Selection::at(cell)));
            app.selecting.set(true);
            // Keep receiving moves when the drag leaves the canvas.
            let _ = app.canvas.set_pointer_capture(event.pointer_id());
            app.request_paint();
        }
        "pointermove" if app.selecting.get() => {
            if let Some(mut selection) = app.selection.get()
                && selection.head != cell
            {
                selection.head = cell;
                app.selection.set(Some(selection));
                app.request_paint();
            }
        }
        // A cancelled pointer (a touch taken for panning) ends the drag too.
        "pointerup" | "pointercancel" => {
            let dragging = app.selecting.replace(false);
            if dragging && app.selection.get().is_some_and(|s| s.is_click()) {
                app.clear_selection();
            }
        }
        _ => {}
    }
}

/// Copying (the browser's own Command+C, or the chord handler) takes the
/// selected text when there is a selection.
fn on_copy(app: &Rc<RefCell<App>>, event: &web_sys::Event, _: &HtmlTextAreaElement) {
    let Some(text) = app.borrow().selected_text() else {
        return;
    };
    if let Some(data) = event
        .dyn_ref::<ClipboardEvent>()
        .and_then(ClipboardEvent::clipboard_data)
        && data.set_data("text/plain", &text).is_ok()
    {
        event.prevent_default();
    }
}

/// The wheel pages the local scrollback, scrolls a mouse-tracking program,
/// or reaches a program on the alternate screen as arrow keys
/// ([`crate::input::route_wheel`]); the page itself does not scroll.
fn on_wheel(app: &Rc<RefCell<App>>, event: &web_sys::Event, _: &HtmlTextAreaElement) {
    let Some(event) = event.dyn_ref::<web_sys::WheelEvent>() else {
        return;
    };
    event.prevent_default();
    let action = {
        let app = app.borrow();
        crate::input::route_wheel(
            wheel_modes(&app),
            event.shift_key(),
            app.session.viewport_scrolled(),
        )
    };
    match action {
        WheelAction::Report => forward_wheel(app, event),
        WheelAction::Arrows => {
            let rows = wheel_travel(&app.borrow(), event, 1.0);
            let arrows =
                crate::input::wheel_arrows(rows.clamp(-MAX_WHEEL_ARROWS, MAX_WHEEL_ARROWS));
            send_input(app, arrows.into_iter().map(InputEvent::Key));
        }
        WheelAction::Scrollback => {
            let app = app.borrow();
            let rows = wheel_travel(&app, event, 1.0);
            if rows != 0 && app.session.scroll_viewport(rows) {
                app.clear_selection();
                app.request_paint();
            }
        }
    }
}

/// The replica's modes a wheel event is routed by.
fn wheel_modes(app: &App) -> crate::input::WheelModes {
    app.session
        .terminal()
        .map_or_else(Default::default, |terminal| crate::input::WheelModes {
            tracking: terminal.mouse_tracking(),
            alt_screen: [1049, 1047, 47]
                .into_iter()
                .any(|mode| terminal.dec_mode(mode)),
            alt_scroll: terminal.dec_mode(1007),
        })
}

/// Whole units of wheel travel, `rows_per_unit` rows each; the remainder
/// carries to the next wheel event. A row is a cell's drawn height.
fn wheel_travel(app: &App, event: &web_sys::WheelEvent, rows_per_unit: f64) -> i32 {
    let rect = app.canvas.get_bounding_client_rect();
    let (_, css_h) = app.css_cell(&rect);
    let (_, page_rows) = app.session.dims();
    let mut carry = app.wheel_carry.get();
    let units = crate::input::wheel_rows(
        event.delta_y(),
        event.delta_mode(),
        css_h * rows_per_unit,
        page_rows,
        &mut carry,
    );
    app.wheel_carry.set(carry);
    units
}

/// Most wheel clicks one wheel event reports, so a flung trackpad cannot
/// flood the program.
const MAX_WHEEL_CLICKS: i32 = 10;

/// Most arrow keys one wheel event sends: the rows of
/// [`MAX_WHEEL_CLICKS`] clicks.
const MAX_WHEEL_ARROWS: i32 = 30;

/// Report wheel travel to the program as xterm wheel presses (buttons 4
/// and 5), one per [`crate::input::WHEEL_ROWS_PER_CLICK`] rows.
fn forward_wheel(app: &Rc<RefCell<App>>, event: &web_sys::WheelEvent) {
    let clicks = wheel_travel(&app.borrow(), event, crate::input::WHEEL_ROWS_PER_CLICK)
        .clamp(-MAX_WHEEL_CLICKS, MAX_WHEEL_CLICKS);
    let button = crate::input::wheel_button(clicks);
    for _ in 0..clicks.unsigned_abs() {
        send_mouse(app, event, MouseAction::Press, button);
    }
}

/// Send one routed keydown; cancel the browser default only when sent.
fn on_keydown(app: &Rc<RefCell<App>>, event: &web_sys::Event, _: &HtmlTextAreaElement) {
    let Some(event) = event.dyn_ref::<KeyboardEvent>() else {
        return;
    };
    let key = event.key();
    let code = event.code();
    let browser_key = crate::input::BrowserKey {
        key: &key,
        code: &code,
        ctrl: event.ctrl_key(),
        shift: event.shift_key(),
        alt: event.alt_key(),
        meta: event.meta_key(),
        alt_graph: event.get_modifier_state("AltGraph"),
        repeat: event.repeat(),
        // Safari reports the keydown that starts a composition as 229
        // before `isComposing` turns true.
        composing: event.is_composing() || event.key_code() == 229,
    };
    if crate::input::is_find_chord(&browser_key) {
        event.prevent_default();
        find::open(app);
        return;
    }
    if crate::input::is_copy_chord(&browser_key) && copy_selection(app) {
        event.prevent_default();
        return;
    }
    if let Some(direction) = crate::input::scrollback_page(&browser_key) {
        event.prevent_default();
        let app = app.borrow();
        let (_, rows) = app.session.dims();
        let page = i32::from(rows.saturating_sub(1).max(1));
        if app.session.scroll_viewport(direction * page) {
            app.clear_selection();
            app.request_paint();
        }
        return;
    }
    let routed = crate::input::route_key(&browser_key);
    if let Some(key) = routed
        && send_input(app, [InputEvent::Key(key)])
    {
        event.prevent_default();
    }
}

/// An IME or dead-key composition committed its text.
fn on_composition_end(
    app: &Rc<RefCell<App>>,
    event: &web_sys::Event,
    surface: &HtmlTextAreaElement,
) {
    if let Some(text) = event
        .dyn_ref::<CompositionEvent>()
        .and_then(CompositionEvent::data)
    {
        send_input(app, text_input_events(&text));
    }
    surface.set_value("");
}

/// Text inserted without a composition (mobile keyboards, dictation, an
/// emoji picker). Composition commits arrive as `compositionend` instead,
/// whichever order the browser fires the two in.
fn on_text_input(app: &Rc<RefCell<App>>, event: &web_sys::Event, surface: &HtmlTextAreaElement) {
    let Some(event) = event.dyn_ref::<web_sys::InputEvent>() else {
        return;
    };
    if event.is_composing() {
        return;
    }
    let plain = matches!(
        event.input_type().as_str(),
        "insertText" | "insertReplacementText"
    );
    if plain && let Some(text) = event.data() {
        send_input(app, text_input_events(&text));
    }
    surface.set_value("");
}

/// A clipboard paste becomes one `INPUT_PASTE`; nothing lands in the surface.
fn on_paste(app: &Rc<RefCell<App>>, event: &web_sys::Event, _: &HtmlTextAreaElement) {
    event.prevent_default();
    let text = event
        .dyn_ref::<ClipboardEvent>()
        .and_then(ClipboardEvent::clipboard_data)
        .and_then(|data| data.get_data("text/plain").ok())
        .unwrap_or_default();
    match crate::input::paste_event(&text) {
        Some(paste) => {
            send_input(app, [InputEvent::Paste(paste)]);
        }
        None if !text.is_empty() => web_sys::console::warn_1(&JsValue::from_str(
            "phux-web: paste larger than the 512 KiB limit was not sent",
        )),
        None => {}
    }
}

/// Copy the selection through the browser's copy command, which raises the
/// `copy` event [`on_copy`] fills. Returns whether there was one to copy.
fn copy_selection(app: &Rc<RefCell<App>>) -> bool {
    let document = {
        let app = app.borrow();
        if app.selected_text().is_none() {
            return false;
        }
        app.canvas.owner_document()
    };
    document
        .and_then(|document| document.dyn_into::<web_sys::HtmlDocument>().ok())
        .is_some_and(|document| document.exec_command("copy").unwrap_or(false))
}

fn text_input_events(text: &str) -> Vec<InputEvent> {
    crate::input::key_events_for_text(text)
        .into_iter()
        .map(InputEvent::Key)
        .collect()
}

/// Send input the user typed or pointed: as [`send_events`], and when any
/// was sent, return a scrolled-back view to the live screen and end the
/// selection, as in a local terminal.
fn send_input(app: &Rc<RefCell<App>>, events: impl IntoIterator<Item = InputEvent>) -> bool {
    let sent = send_events(app, events);
    let app = app.borrow();
    if sent {
        app.clear_selection();
        if app.session.scroll_to_bottom() {
            app.request_paint();
        }
    }
    sent
}

/// Encode and send input atoms for the focused terminal. Returns whether any
/// was sent; a transport failure closes the connection.
fn send_events(app: &Rc<RefCell<App>>, events: impl IntoIterator<Item = InputEvent>) -> bool {
    let mut sent = false;
    for event in events {
        let mut a = app.borrow_mut();
        let Some(frame) = a.session.input_frame(event) else {
            return sent;
        };
        if let Err(message) = a.tx.send(&frame) {
            drop(a);
            close_with_transport_error(app, &message);
            return false;
        }
        sent = true;
    }
    sent
}

/// Focus reporting (DECSET 1004): while the program asks for it, the input
/// surface gaining or losing focus (a click on the terminal, the find bar
/// or another control taking the keys, the window going to the background)
/// reaches it as `INPUT_FOCUS`, which the server writes as `CSI I` or
/// `CSI O`. It is not typing: the view and the selection stay.
fn on_focus_change(app: &Rc<RefCell<App>>, event: &web_sys::Event, _: &HtmlTextAreaElement) {
    let reporting = app
        .borrow()
        .session
        .terminal()
        .is_some_and(|terminal| terminal.dec_mode(1004));
    if !reporting {
        return;
    }
    let focus = if event.type_() == "focus" {
        FocusEvent::Gained
    } else {
        FocusEvent::Lost
    };
    send_events(app, [InputEvent::Focus(focus)]);
}

/// Cursor blink: toggle the phase and repaint on a fixed cadence.
fn install_cursor_blink(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let weak = Rc::downgrade(app);
    let blink = Closure::<dyn FnMut()>::new(move || {
        if let Some(app) = weak.upgrade() {
            app.borrow().blink();
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

/// The animation-frame callback every paint request shares.
fn install_frame(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let weak = Rc::downgrade(app);
    let callback = Closure::<dyn FnMut()>::new(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let app = app.borrow();
        if let Some(frame) = app.bindings.borrow().frame.as_ref() {
            frame.pending.set(None);
        }
        app.paint();
    });
    app.borrow()
        .bindings
        .borrow_mut()
        .frame
        .replace(FrameBinding {
            window,
            callback,
            pending: Cell::new(None),
        });
    Ok(())
}

fn install_bootstrap_expiry(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let weak = Rc::downgrade(app);
    let callback = Closure::<dyn FnMut()>::new(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let expired = app.borrow_mut().session.expire_bootstrap_staging();
        if expired {
            close_with_protocol_error(&app, "terminal bootstrap staging timed out");
        }
    });
    let interval = window.set_interval_with_callback_and_timeout_and_arguments_0(
        callback.as_ref().unchecked_ref(),
        BOOTSTRAP_EXPIRY_POLL_MS,
    )?;
    app.borrow()
        .bindings
        .borrow_mut()
        .bootstrap_expiry
        .replace(BlinkBinding {
            window,
            interval,
            callback,
        });
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::cell::Cell;
    use std::future::Future as _;
    use std::rc::{Rc, Weak};
    use std::task::Poll;

    use super::{
        BatchEffects, FallbackToken, FrameBuffer, MAX_OUTBOUND_BYTES, OutboundQueue, ReceiveFlow,
        WebTransportExit, authenticated_websocket_protocols, await_js_promise_with_deadline,
        fallback_token_from_webtransport_url, poisoned_framing_flow, webtransport_eof_exit,
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
    use wasm_bindgen::{JsValue, closure::Closure};
    use wasm_bindgen_futures::JsFuture;
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
    use web_sys::{HtmlCanvasElement, WebSocket};

    wasm_bindgen_test_configure!(run_in_browser);

    /// The live `ws_demo_server` the browser lane starts (a fixed default
    /// for running these by hand).
    const TEST_WS_URL: &str = match option_env!("PHUX_TEST_WS_URL") {
        Some(url) => url,
        None => "ws://127.0.0.1:47654/",
    };

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
            fallback_token_from_webtransport_url("https://host/session?x=1&token=00aF#ignored"),
            FallbackToken::Valid("00aF")
        );
        assert_eq!(
            fallback_token_from_webtransport_url("https://host/session?token=secret"),
            FallbackToken::Invalid
        );
        assert_eq!(
            fallback_token_from_webtransport_url("https://host/session#token=00"),
            FallbackToken::Missing
        );
        assert_eq!(
            fallback_token_from_webtransport_url("https://host/session?token=00&token=00"),
            FallbackToken::Invalid
        );
        assert_eq!(
            fallback_token_from_webtransport_url("https://host/session?token=00&token"),
            FallbackToken::Invalid
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

    async fn open_websocket() -> WebSocket {
        let ws = WebSocket::new(TEST_WS_URL).expect("create test WebSocket");
        for _ in 0..200 {
            if ws.ready_state() == WebSocket::OPEN {
                return ws;
            }
            TimeoutFuture::new(10).await;
        }
        panic!("test WebSocket did not open");
    }

    async fn closed_websocket() -> WebSocket {
        let ws = open_websocket().await;
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
    async fn malformed_token_fails_closed_before_an_available_ws_fallback() {
        let result = super::run_with_fallback(
            "https://127.0.0.1:9/session?token=not-hex",
            TEST_WS_URL,
            test_canvas(),
            80,
            24,
        )
        .await;
        assert!(result.is_err());
    }

    #[wasm_bindgen_test]
    async fn dropping_polled_establishment_disposes_app_reader_and_transport() {
        let ws = open_websocket().await;
        let vt = Vt::load().await.unwrap();
        let tx = super::WireTx::Ws(super::WsTx::new(ws.clone()));
        let (app, _ready) = super::build_app(&vt, tx, test_canvas(), 80, 24, false).unwrap();
        let weak = Rc::downgrade(&app);
        let (cancel_tx, cancel_rx) = oneshot::channel();
        app.borrow()
            .bindings
            .borrow_mut()
            .wt_reader_cancel
            .replace(cancel_tx);

        let reader_finished = Rc::new(Cell::new(false));
        let finished = Rc::clone(&reader_finished);
        let reader_app = Rc::clone(&app);
        wasm_bindgen_futures::spawn_local(async move {
            let _ = cancel_rx.await;
            drop(reader_app);
            finished.set(true);
        });

        let mut establishment = Box::pin(async move {
            let _guard = super::AppEstablishment::new(app);
            futures_util::future::pending::<()>().await;
        });
        futures_util::future::poll_fn(|cx| {
            assert!(establishment.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(establishment);

        for _ in 0..20 {
            if reader_finished.get() && ws.ready_state() != WebSocket::OPEN {
                break;
            }
            TimeoutFuture::new(10).await;
        }
        assert!(reader_finished.get(), "reader task observed cancellation");
        assert_ne!(ws.ready_state(), WebSocket::OPEN, "transport was closed");
        assert!(weak.upgrade().is_none(), "establishment App was released");
    }

    #[wasm_bindgen_test]
    async fn dropping_polled_webtransport_ready_closes_raw_session() {
        let object = js_sys::Object::new();
        let pending = js_sys::Promise::new(&mut |_, _| {});
        js_sys::Reflect::set(&object, &JsValue::from_str("ready"), pending.as_ref()).unwrap();
        let closed = Rc::new(Cell::new(false));
        let close_latch = Rc::clone(&closed);
        let close = Closure::<dyn FnMut()>::new(move || close_latch.set(true));
        js_sys::Reflect::set(&object, &JsValue::from_str("close"), close.as_ref()).unwrap();
        let transport: web_sys::WebTransport = object.unchecked_into();

        let mut establishment = Box::pin(async move {
            let guard = super::WebTransportAttempt::new(transport);
            let _ = JsFuture::from(guard.session().ready()).await;
        });
        futures_util::future::poll_fn(|cx| {
            assert!(establishment.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(establishment);

        assert!(closed.get(), "raw WebTransport session was closed");
        drop(close);
    }

    #[wasm_bindgen_test]
    async fn exported_start_retains_live_client_until_transport_failure() {
        let document = web_sys::window().unwrap().document().unwrap();
        let canvas = test_canvas();
        canvas.set_id("phux-start-retention-test");
        document.body().unwrap().append_child(&canvas).unwrap();

        crate::start(
            TEST_WS_URL.to_owned(),
            "phux-start-retention-test".to_owned(),
            80,
            24,
        )
        .await
        .unwrap();
        let app = super::RETAINED_APP_FOR_TEST
            .with(|retained| retained.borrow().as_ref().and_then(Weak::upgrade))
            .expect("exported start retained its live App");
        assert!(!app.borrow().session.is_failed());

        super::close_with_transport_error(&app, "test cleanup");
        drop(app);
        assert!(
            super::RETAINED_APP_FOR_TEST
                .with(|retained| retained.borrow().as_ref().and_then(Weak::upgrade))
                .is_none()
        );
        canvas.remove();
    }

    /// One transport chunk that takes a fresh session through HELLO_OK to
    /// ATTACH_READY with one raw-profile terminal whose bootstrap is
    /// `payload`.
    fn attached_chunk(payload: &'static [u8]) -> Vec<u8> {
        let terminal_id = ResourceId::new(1);
        let stream_id = StreamId::new(1).unwrap();
        let bootstrap_id = BootstrapId::new(1).unwrap();
        let frames = [
            FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new(),
                server_id: Vec::new(),
                selected_profile: BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::default(),
            },
            FrameKind::Attached {
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
            },
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 80,
                rows: 24,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(payload),
            },
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
            FrameKind::AttachReady { attach_id: 1 },
        ];
        let mut chunk = Vec::new();
        for frame in frames {
            let mut encoded = bytes::BytesMut::new();
            frame.encode(&mut encoded);
            chunk.extend_from_slice(&encoded);
        }
        chunk
    }

    #[wasm_bindgen_test]
    async fn program_title_reaches_the_canvas_and_a_dom_event() {
        let ws = open_websocket().await;
        let vt = Vt::load().await.unwrap();
        let tx = super::WireTx::Ws(super::WsTx::new(ws));
        let canvas = test_canvas();
        let (app, _ready) = super::build_app(&vt, tx, canvas.clone(), 80, 24, false).unwrap();
        let seen = Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let record = Rc::clone(&seen);
        let listener =
            Closure::<dyn FnMut(web_sys::CustomEvent)>::new(move |event: web_sys::CustomEvent| {
                record
                    .borrow_mut()
                    .push(event.detail().as_string().unwrap_or_default());
            });
        canvas
            .add_event_listener_with_callback(super::TITLE_EVENT, listener.as_ref().unchecked_ref())
            .unwrap();

        let chunk = attached_chunk(b"\x1b]2;vim README.md\x07prompt$ ");
        assert_eq!(
            super::process_webtransport_chunk(&app, &mut FrameBuffer::new(), &chunk),
            ReceiveFlow::Continue
        );
        app.borrow().paint();
        app.borrow().paint();
        assert_eq!(
            canvas.get_attribute(super::TITLE_ATTRIBUTE).as_deref(),
            Some("vim README.md")
        );
        assert_eq!(
            *seen.borrow(),
            ["vim README.md"],
            "one event per change, not per paint"
        );
        app.borrow_mut().dispose();
    }

    #[wasm_bindgen_test]
    async fn ready_then_malformed_same_chunk_remains_failed() {
        let ws = open_websocket().await;
        let vt = Vt::load().await.unwrap();
        let tx = super::WireTx::Ws(super::WsTx::new(ws));
        let (app, ready) = super::build_app(&vt, tx, test_canvas(), 80, 24, false).unwrap();
        let mut chunk = attached_chunk(b"ready");
        chunk.extend_from_slice(&[0, 0, 0, 1, 0xff]);

        assert_eq!(
            super::process_webtransport_chunk(&app, &mut FrameBuffer::new(), &chunk),
            ReceiveFlow::Stop
        );
        super::await_protocol_ready(&app, ready, 100)
            .await
            .expect("ATTACH_READY signalled first");
        assert!(super::ensure_app_live(&app).is_err());
        assert!(app.borrow().session.is_failed());
        assert!(app.borrow().self_owner.borrow().is_none());
    }

    #[wasm_bindgen_test]
    async fn closed_ws_send_and_repeated_reconnect_teardown_release_every_app() {
        let ws = closed_websocket().await;
        let vt = Vt::load().await.unwrap();
        let document = web_sys::window().unwrap().document().unwrap();
        for _ in 0..8 {
            let tx = super::WireTx::Ws(super::WsTx::new(ws.clone()));
            let (app, _ready) = super::build_app(&vt, tx, test_canvas(), 80, 24, false).unwrap();
            super::install_transport_failure_hook(&app);
            super::install_websocket_handlers(&app, &ws);
            super::install_input(&app).unwrap();
            super::install_cursor_blink(&app).unwrap();
            app.borrow().self_owner.replace(Some(Rc::clone(&app)));
            let weak = Rc::downgrade(&app);

            super::send_handshake(&app);
            assert!(app.borrow().session.is_failed());
            assert!(app.borrow().self_owner.borrow().is_none());
            assert_eq!(
                app.borrow()
                    .canvas
                    .get_attribute(super::CONNECTION_ATTRIBUTE)
                    .as_deref(),
                Some("disconnected"),
                "a failed connection is marked on its canvas"
            );
            {
                let app = app.borrow();
                let bindings = app.bindings.borrow();
                assert!(bindings.websocket.is_none());
                assert!(bindings.input.is_none());
                assert!(bindings.blink.is_none());
                assert!(bindings.bootstrap_expiry.is_none());
            }
            document
                .dispatch_event(&web_sys::KeyboardEvent::new("keydown").unwrap())
                .unwrap();
            drop(app);
            assert!(weak.upgrade().is_none(), "failed App must be released");
        }
    }

    #[wasm_bindgen_test]
    async fn paint_requests_share_one_animation_frame_and_teardown_cancels_it() {
        let ws = open_websocket().await;
        let vt = Vt::load().await.unwrap();
        let tx = super::WireTx::Ws(super::WsTx::new(ws));
        let (app, _ready) = super::build_app(&vt, tx, test_canvas(), 80, 24, false).unwrap();
        let pending = |app: &Rc<std::cell::RefCell<super::App>>| {
            app.borrow()
                .bindings
                .borrow()
                .frame
                .as_ref()
                .and_then(|frame| frame.pending.get())
        };

        app.borrow().request_paint();
        let first = pending(&app).expect("a paint is scheduled");
        for _ in 0..100 {
            app.borrow().request_paint();
        }
        assert_eq!(pending(&app), Some(first), "a burst shares one frame");
        for _ in 0..50 {
            if pending(&app).is_none() {
                break;
            }
            TimeoutFuture::new(10).await;
        }
        assert_eq!(pending(&app), None, "the frame ran and cleared its request");

        app.borrow().request_paint();
        assert!(pending(&app).is_some());
        app.borrow_mut().dispose();
        assert!(
            app.borrow().bindings.borrow().frame.is_none(),
            "teardown cancels the pending frame"
        );
    }

    #[wasm_bindgen_test]
    fn drained_frame_batch_coalesces_paint_requests() {
        let mut batch = BatchEffects::default();
        for _ in 0..2_048 {
            batch.merge(BatchEffects {
                flow: ReceiveFlow::Continue,
                render: true,
                badges: false,
                bell: true,
            });
        }
        assert!(batch.render);
        assert_eq!(usize::from(batch.render), 1, "one paint follows the drain");
        assert!(batch.bell, "a flood of bells rings once per drained batch");
    }
}
