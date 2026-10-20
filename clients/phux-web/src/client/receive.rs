//! Receiving: protocol readiness, the WebTransport reader, and applying frames.

use super::*;

pub(super) async fn await_protocol_ready(
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

pub(super) fn ensure_app_live(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    if app.borrow().session.is_failed() {
        Err(JsValue::from_str(
            "connection failed while completing protocol readiness",
        ))
    } else {
        Ok(())
    }
}

pub(super) async fn await_js_promise_with_deadline<
    T: wasm_bindgen::convert::FromWasmAbi + 'static,
>(
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

pub(super) async fn run_webtransport_reader(
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

pub(super) fn process_webtransport_chunk(
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

pub(super) fn webtransport_eof_exit(frames: &FrameBuffer) -> WebTransportExit<'static> {
    if frames.pending_bytes() == 0 {
        WebTransportExit::CleanEof
    } else {
        WebTransportExit::PartialEof
    }
}

pub(super) fn webtransport_exit_flow(
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

pub(super) fn poisoned_framing_flow(frames: &FrameBuffer, close: impl FnOnce(&str)) -> ReceiveFlow {
    if frames.poisoned() {
        close("WebTransport stream used a zero or oversized frame length");
        ReceiveFlow::Stop
    } else {
        ReceiveFlow::Continue
    }
}

/// Drive the session with one decoded server frame: ack and repaint as the
/// session asks. Shared by both transports' receive paths.
pub(super) fn handle_frame(app: &Rc<RefCell<App>>, frame: FrameKind) -> ReceiveFlow {
    let effects = apply_frame(app, frame);
    effects.paint(app);
    effects.flow
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
    pub(super) fn merge(&mut self, other: Self) {
        self.render |= other.render;
        self.badges |= other.badges;
        self.bell |= other.bell;
        if matches!(other.flow, ReceiveFlow::Stop) {
            self.flow = ReceiveFlow::Stop;
        }
    }

    pub(super) fn paint(self, app: &Rc<RefCell<App>>) {
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
    let old_focus = a.session.focused_pane();
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
    if old_focus != a.session.focused_pane() {
        a.focus_changed(old_focus);
    }
    if outcome.panes {
        a.publish_panes();
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

pub(super) fn decode_server_frame(
    app: &Rc<RefCell<App>>,
    framed: &[u8],
) -> Result<FrameKind, String> {
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

pub(super) fn close_with_protocol_error(app: &Rc<RefCell<App>>, message: &str) {
    web_sys::console::error_1(&JsValue::from_str(&format!(
        "phux-web protocol error: {message}",
    )));
    let mut app = app.borrow_mut();
    app.fail(message);
}

pub(super) fn close_with_transport_error(app: &Rc<RefCell<App>>, message: &str) {
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
