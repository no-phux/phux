//! Browser glue: a WebSocket to the phux server, a `<canvas>`, and the keyboard,
//! all driving a [`Session`]. This is the only part that touches the DOM/WS; the
//! protocol logic lives in [`crate::session`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::FrameKind;
use phux_vt_web::Vt;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    BinaryType, CanvasRenderingContext2d, CloseEvent, HtmlCanvasElement, KeyboardEvent,
    MessageEvent, WebSocket,
};

use crate::{Metrics, render};

const MAX_PENDING_FRAMES: usize = 64;
const MAX_PENDING_BYTES: usize = 1024 * 1024;

/// Connect to a phux server over WebSocket and render the attached terminal
/// into the given canvas, routing keyboard input back. Resolves once wired up;
/// the handlers then run for the connection's lifetime.
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
    let vt = Vt::load().await?;
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("no 2D context"))?
        .dyn_into()?;

    let ws = WebSocket::new(ws_url)?;
    ws.set_binary_type(BinaryType::Arraybuffer);

    let app = Rc::new(RefCell::new(App {
        session: crate::Session::new(&vt, cols, rows),
        ws: ws.clone(),
        canvas,
        ctx,
        metrics: Metrics::default(),
        cursor_on: Cell::new(true),
    }));

    let disposed = Rc::new(Cell::new(false));
    let ui = Rc::new(RefCell::new(UiResources::new(app.borrow().canvas.clone())));

    // On open: send HELLO + ATTACH.
    let onopen = {
        let app = Rc::clone(&app);
        let disposed = Rc::clone(&disposed);
        let onopen = Closure::<dyn FnMut()>::new(move || {
            if disposed.get() {
                return;
            }
            let a = app.borrow();
            a.send(a.session.handshake());
        });
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen
    };

    // On message: decode one frame, drive the session, ack, repaint.
    let onmessage = {
        let app = Rc::clone(&app);
        let disposed = Rc::clone(&disposed);
        let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if disposed.get() {
                return;
            }
            let buf = js_sys::Uint8Array::new(&e.data()).to_vec();
            let Ok((frame, _rest)) = FrameKind::decode(&buf) else {
                return;
            };
            let mut a = app.borrow_mut();
            let outcome = a.session.on_frame(frame);
            if !outcome.send.is_empty() {
                a.send(outcome.send);
            }
            if outcome.render {
                a.paint();
            }
        });
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage
    };

    if let Err(error) = install_ui(&app, &ui, &disposed) {
        disposed.set(true);
        let _ = ws.close_with_code(1011);
        detach_socket(&ws);
        return Err(error);
    }

    let onclose = {
        let disposed = Rc::clone(&disposed);
        let ui = Rc::clone(&ui);
        let onclose = Closure::<dyn FnMut(CloseEvent)>::new(move |_event: CloseEvent| {
            disposed.set(true);
            ui.borrow_mut().dispose();
        });
        ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        onclose
    };

    Ok(Client {
        app,
        ws,
        disposed,
        ui,
        _onopen: Some(onopen),
        _onmessage: Some(onmessage),
        _onerror: None,
        _onclose: Some(onclose),
    })
}

/// Connect to the hosted WebSocket endpoint. The endpoint must send exactly
/// one accepted `phux.session.v1` text control message before any binary phux
/// wire frame. Lifecycle callbacks contain only validated control fields and
/// normalized close/error categories.
///
/// The socket and all of its handlers are installed before the terminal engine
/// is loaded. Accepted wire frames arriving while the engine loads are queued,
/// which avoids losing the first frame or an immediate close.
///
/// # Errors
/// Fails if the WebSocket cannot be created, the engine cannot load, or the
/// canvas has no 2D context.
pub async fn run_hosted(
    ws_url: &str,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
    callback: js_sys::Function,
) -> Result<Client, JsValue> {
    let ws = WebSocket::new(ws_url)?;
    ws.set_binary_type(BinaryType::Arraybuffer);
    let disposed = Rc::new(Cell::new(false));
    let ui = Rc::new(RefCell::new(UiResources::new(canvas.clone())));
    let state = Rc::new(RefCell::new(HostedState {
        app: None,
        accepted: false,
        opened: false,
        handshake_sent: false,
        failed: false,
        error_emitted: false,
        close_emitted: false,
        pending: Vec::new(),
        pending_bytes: 0,
        callback,
        disposed: Rc::clone(&disposed),
    }));

    let onopen = {
        let state = Rc::clone(&state);
        let onopen = Closure::<dyn FnMut()>::new(move || {
            let mut state = state.borrow_mut();
            if state.disposed.get() {
                return;
            }
            state.opened = true;
            state.maybe_send_handshake();
        });
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen
    };

    let onmessage = {
        let state = Rc::clone(&state);
        let message_ws = ws.clone();
        let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let mut state = state.borrow_mut();
            if state.failed || state.disposed.get() {
                return;
            }
            if let Some(text) = event.data().as_string() {
                if state.accepted {
                    state.fail_protocol(&message_ws);
                    return;
                }
                let Some(control) = parse_hosted_control(&text) else {
                    state.fail_protocol(&message_ws);
                    return;
                };
                state.accepted = true;
                state.emit(&control);
                state.maybe_send_handshake();
                return;
            }
            if !state.accepted || !event.data().is_instance_of::<js_sys::ArrayBuffer>() {
                state.fail_protocol(&message_ws);
                return;
            }
            let bytes = js_sys::Uint8Array::new(&event.data()).to_vec();
            if let Some(app) = &state.app {
                handle_wire_message(app, &bytes);
            } else {
                if pending_limit_exceeded(state.pending.len(), state.pending_bytes, bytes.len()) {
                    state.fail_protocol(&message_ws);
                    return;
                }
                state.pending_bytes += bytes.len();
                state.pending.push(bytes);
            }
        });
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage
    };

    let onerror = {
        let state = Rc::clone(&state);
        let onerror = Closure::<dyn FnMut()>::new(move || {
            let mut state = state.borrow_mut();
            if state.disposed.get() {
                return;
            }
            if !state.error_emitted {
                state.error_emitted = true;
                state.emit(&callback_event(
                    "error",
                    &[("category", JsValue::from_str("transport"))],
                ));
            }
        });
        ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror
    };

    let onclose = {
        let state = Rc::clone(&state);
        let ui = Rc::clone(&ui);
        let onclose = Closure::<dyn FnMut(CloseEvent)>::new(move |event: CloseEvent| {
            let mut state = state.borrow_mut();
            if state.close_emitted || state.disposed.get() {
                return;
            }
            state.close_emitted = true;
            state.pending.clear();
            state.pending_bytes = 0;
            ui.borrow_mut().dispose();
            let (code, category) = normalize_close(event.code());
            state.emit(&callback_event(
                "close",
                &[
                    ("code", JsValue::from_f64(f64::from(code))),
                    ("category", JsValue::from_str(category)),
                    ("wasClean", JsValue::from_bool(event.was_clean())),
                ],
            ));
            state.disposed.set(true);
        });
        ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        onclose
    };

    let vt = match Vt::load().await {
        Ok(vt) => vt,
        Err(error) => {
            state.borrow_mut().fail_client(&ws);
            detach_socket(&ws);
            return Err(error);
        }
    };
    let context = match canvas.get_context("2d") {
        Ok(Some(context)) => context,
        Ok(None) => {
            state.borrow_mut().fail_client(&ws);
            detach_socket(&ws);
            return Err(JsValue::from_str("no 2D context"));
        }
        Err(error) => {
            state.borrow_mut().fail_client(&ws);
            detach_socket(&ws);
            return Err(error);
        }
    };
    let ctx: CanvasRenderingContext2d = match context.dyn_into() {
        Ok(ctx) => ctx,
        Err(error) => {
            state.borrow_mut().fail_client(&ws);
            detach_socket(&ws);
            return Err(error.into());
        }
    };
    let app = Rc::new(RefCell::new(App {
        session: crate::Session::new(&vt, cols, rows),
        ws: ws.clone(),
        canvas,
        ctx,
        metrics: Metrics::default(),
        cursor_on: Cell::new(true),
    }));

    {
        let mut state = state.borrow_mut();
        for bytes in state.pending.drain(..) {
            handle_wire_message(&app, &bytes);
        }
        state.pending_bytes = 0;
        state.app = Some(Rc::clone(&app));
        state.maybe_send_handshake();
    }
    if !disposed.get() {
        if let Err(error) = install_ui(&app, &ui, &disposed) {
            state.borrow_mut().fail_client(&ws);
            disposed.set(true);
            detach_socket(&ws);
            return Err(error);
        }
    }

    Ok(Client {
        app,
        ws,
        disposed,
        ui,
        _onopen: Some(onopen),
        _onmessage: Some(onmessage),
        _onerror: Some(onerror),
        _onclose: Some(onclose),
    })
}

struct HostedState {
    app: Option<Rc<RefCell<App>>>,
    accepted: bool,
    opened: bool,
    handshake_sent: bool,
    failed: bool,
    error_emitted: bool,
    close_emitted: bool,
    pending: Vec<Vec<u8>>,
    pending_bytes: usize,
    callback: js_sys::Function,
    disposed: Rc<Cell<bool>>,
}

impl HostedState {
    fn emit(&self, value: &JsValue) {
        if self.disposed.get() {
            return;
        }
        if let Err(error) = self.callback.call1(&JsValue::UNDEFINED, value) {
            web_sys::console::error_2(
                &JsValue::from_str("phux-web hosted callback failed"),
                &error,
            );
        }
    }

    fn maybe_send_handshake(&mut self) {
        if self.accepted
            && self.opened
            && !self.failed
            && !self.disposed.get()
            && !self.handshake_sent
        {
            if let Some(app) = &self.app {
                let app = app.borrow();
                app.send(app.session.handshake());
                self.handshake_sent = true;
            }
        }
    }

    fn fail_protocol(&mut self, ws: &WebSocket) {
        self.failed = true;
        self.pending.clear();
        self.pending_bytes = 0;
        if !self.error_emitted {
            self.error_emitted = true;
            self.emit(&callback_event(
                "error",
                &[("category", JsValue::from_str("protocol"))],
            ));
        }
        let _ = ws.close_with_code(1002);
    }

    fn fail_client(&mut self, ws: &WebSocket) {
        self.failed = true;
        self.pending.clear();
        self.pending_bytes = 0;
        if !self.error_emitted {
            self.error_emitted = true;
            self.emit(&callback_event(
                "error",
                &[("category", JsValue::from_str("client"))],
            ));
        }
        let _ = ws.close_with_code(1011);
    }
}

fn parse_hosted_control(text: &str) -> Option<JsValue> {
    let value = js_sys::JSON::parse(text).ok()?;
    if !value.is_object() || js_sys::Array::is_array(&value) {
        return None;
    }
    let object = js_sys::Object::from(value.clone());
    let keys = js_sys::Object::keys(&object);
    for key in keys.iter().filter_map(|key| key.as_string()) {
        if !matches!(
            key.as_str(),
            "type" | "outcome" | "backend" | "expiresAt" | "fallbackReason"
        ) {
            return None;
        }
    }
    if string_field(&value, "type").as_deref() != Some("phux.session.v1")
        || string_field(&value, "outcome").as_deref() != Some("accepted")
    {
        return None;
    }
    let backend = string_field(&value, "backend")?;
    if backend != "native" && backend != "edge" {
        return None;
    }
    let expires_at = js_sys::Reflect::get(&value, &JsValue::from_str("expiresAt"))
        .ok()?
        .as_f64()?;
    if !expires_at.is_finite()
        || expires_at <= 0.0
        || expires_at > 9_007_199_254_740_991.0
        || expires_at.fract() != 0.0
    {
        return None;
    }
    let fallback = js_sys::Reflect::get(&value, &JsValue::from_str("fallbackReason")).ok()?;
    let fallback = if fallback.is_undefined() {
        None
    } else {
        let fallback = fallback.as_string()?;
        if backend != "edge" || !safe_fallback_reason(&fallback) {
            return None;
        }
        Some(fallback)
    };

    let fields = callback_event(
        "phux.session.v1",
        &[
            ("outcome", JsValue::from_str("accepted")),
            ("backend", JsValue::from_str(&backend)),
            ("expiresAt", JsValue::from_f64(expires_at)),
        ],
    );
    if let Some(fallback) = fallback {
        js_sys::Reflect::set(
            &fields,
            &JsValue::from_str("fallbackReason"),
            &JsValue::from_str(&fallback),
        )
        .ok()?;
    }
    Some(fields)
}

fn string_field(value: &JsValue, name: &str) -> Option<String> {
    js_sys::Reflect::get(value, &JsValue::from_str(name))
        .ok()?
        .as_string()
}

fn safe_fallback_reason(reason: &str) -> bool {
    matches!(
        reason,
        "auth-required"
            | "account-concurrency"
            | "hourly-quota"
            | "daily-quota"
            | "native-capacity"
            | "ip-capacity"
            | "native-disabled"
            | "native-unhealthy"
            | "startup-timeout"
            | "startup-failed"
    )
}

fn callback_event(kind: &str, fields: &[(&str, JsValue)]) -> JsValue {
    let value = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&value, &JsValue::from_str("type"), &JsValue::from_str(kind));
    for (name, field) in fields {
        let _ = js_sys::Reflect::set(&value, &JsValue::from_str(name), field);
    }
    value.into()
}

fn normalize_close(code: u16) -> (u16, &'static str) {
    match code {
        1000 => (1000, "normal"),
        1001 => (1001, "going-away"),
        1002 | 1003 | 1007 | 1008 | 1009 => (code, "protocol"),
        1011 => (1011, "server"),
        1012 | 1013 => (code, "unavailable"),
        4001 => (4001, "capacity"),
        4002 => (4002, "rate-limited"),
        4003 => (4003, "bad-request"),
        4004 => (4004, "idle"),
        4005 => (4005, "expired"),
        4007 => (4007, "unauthorized"),
        4011 => (4011, "server"),
        _ => (1006, "network"),
    }
}

fn handle_wire_message(app: &Rc<RefCell<App>>, bytes: &[u8]) {
    let Ok((frame, _rest)) = FrameKind::decode(bytes) else {
        return;
    };
    let mut app = app.borrow_mut();
    let outcome = app.session.on_frame(frame);
    if !outcome.send.is_empty() {
        app.send(outcome.send);
    }
    if outcome.render {
        app.paint();
    }
}

fn pending_limit_exceeded(count: usize, bytes: usize, incoming: usize) -> bool {
    count >= MAX_PENDING_FRAMES
        || bytes
            .checked_add(incoming)
            .is_none_or(|total| total > MAX_PENDING_BYTES)
}

fn detach_socket(ws: &WebSocket) {
    ws.set_onopen(None);
    ws.set_onmessage(None);
    ws.set_onerror(None);
    ws.set_onclose(None);
}

fn install_ui(
    app: &Rc<RefCell<App>>,
    ui: &Rc<RefCell<UiResources>>,
    disposed: &Rc<Cell<bool>>,
) -> Result<(), JsValue> {
    let key_app = Rc::clone(app);
    let disposed_for_key = Rc::clone(disposed);
    let onkey = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
        if disposed_for_key.get() {
            return;
        }
        let Some(event) = key_event_from_browser(&e) else {
            return;
        };
        let app = key_app.borrow();
        if let Some(frame) = app.session.key_frame(event) {
            let _ = app.ws.send_with_u8_array(&frame);
            e.prevent_default();
        }
    });
    {
        let mut ui = ui.borrow_mut();
        ui.original_tabindex = Some(ui.canvas.get_attribute("tabindex"));
        if let Err(error) = ui.canvas.set_attribute("tabindex", "0") {
            ui.dispose();
            return Err(error);
        }
        if let Err(error) = ui
            .canvas
            .add_event_listener_with_callback("keydown", onkey.as_ref().unchecked_ref())
        {
            ui.dispose();
            return Err(error);
        }
        ui.onkey = Some(onkey);
    }

    let app = Rc::clone(app);
    let disposed_for_blink = Rc::clone(disposed);
    let Some(window) = web_sys::window() else {
        ui.borrow_mut().dispose();
        return Err(JsValue::from_str("no window"));
    };
    let blink = Closure::<dyn FnMut()>::new(move || {
        if disposed_for_blink.get() {
            return;
        }
        let app = app.borrow();
        app.cursor_on.set(!app.cursor_on.get());
        app.paint();
    });
    let blink_id = match window
        .set_interval_with_callback_and_timeout_and_arguments_0(blink.as_ref().unchecked_ref(), 530)
    {
        Ok(id) => id,
        Err(error) => {
            ui.borrow_mut().dispose();
            return Err(error);
        }
    };
    let mut ui = ui.borrow_mut();
    ui.blink_id = Some(blink_id);
    ui.blink = Some(blink);
    Ok(())
}

struct UiResources {
    canvas: HtmlCanvasElement,
    original_tabindex: Option<Option<String>>,
    onkey: Option<Closure<dyn FnMut(KeyboardEvent)>>,
    blink: Option<Closure<dyn FnMut()>>,
    blink_id: Option<i32>,
}

impl UiResources {
    fn new(canvas: HtmlCanvasElement) -> Self {
        Self {
            canvas,
            original_tabindex: None,
            onkey: None,
            blink: None,
            blink_id: None,
        }
    }

    fn dispose(&mut self) {
        if let Some(onkey) = self.onkey.take() {
            let _ = self
                .canvas
                .remove_event_listener_with_callback("keydown", onkey.as_ref().unchecked_ref());
        }
        if let Some(id) = self.blink_id.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_interval_with_handle(id);
        }
        self.blink.take();
        if let Some(tabindex) = self.original_tabindex.take() {
            match tabindex {
                Some(value) => {
                    let _ = self.canvas.set_attribute("tabindex", &value);
                }
                None => {
                    let _ = self.canvas.remove_attribute("tabindex");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{MAX_PENDING_BYTES, normalize_close, parse_hosted_control, pending_limit_exceeded};

    #[wasm_bindgen_test]
    fn hosted_control_accepts_only_the_deployed_shape() {
        assert!(
            parse_hosted_control(
                r#"{"type":"phux.session.v1","outcome":"accepted","backend":"native","expiresAt":1786800000000}"#,
            )
            .is_some()
        );
        assert!(
            parse_hosted_control(
                r#"{"type":"phux.session.v1","outcome":"accepted","backend":"edge","expiresAt":1786800000000,"fallbackReason":"native-capacity"}"#,
            )
            .is_some()
        );
        for malformed in [
            r#"{"type":"phux.session.v1","outcome":"accepted","backend":"edge"}"#,
            r#"{"type":"phux.session.v1","outcome":"accepted","backend":"other","expiresAt":1}"#,
            r#"{"type":"phux.session.v1","outcome":"accepted","backend":"native","expiresAt":1,"fallbackReason":"startup-error"}"#,
            r#"{"type":"phux.session.v1","outcome":"accepted","backend":"edge","expiresAt":1,"fallbackReason":"arbitrary server text"}"#,
            r#"{"type":"phux.session.v1","outcome":"accepted","backend":"edge","expiresAt":1,"fallbackReason":"pre-upgrade-503"}"#,
            r#"{"type":"phux.session.v1","outcome":"accepted","backend":"edge","expiresAt":0}"#,
            r#"{"type":"phux.session.v1","outcome":"accepted","backend":"edge","expiresAt":1,"extra":true}"#,
        ] {
            assert!(parse_hosted_control(malformed).is_none(), "{malformed}");
        }
    }

    #[wasm_bindgen_test]
    fn close_codes_are_normalized_to_closed_categories() {
        assert_eq!(normalize_close(4001), (4001, "capacity"));
        assert_eq!(normalize_close(4004), (4004, "idle"));
        assert_eq!(normalize_close(4011), (4011, "server"));
        assert_eq!(normalize_close(4999), (1006, "network"));
    }

    #[wasm_bindgen_test]
    fn pending_startup_frames_are_bounded_by_count_and_bytes() {
        assert!(!pending_limit_exceeded(63, MAX_PENDING_BYTES - 1, 1));
        assert!(pending_limit_exceeded(64, 0, 1));
        assert!(pending_limit_exceeded(0, MAX_PENDING_BYTES, 1));
        assert!(pending_limit_exceeded(0, usize::MAX, 1));
    }
}

/// A live connection handle. The event handlers run for the connection's
/// lifetime; this lets a caller (or test) inspect the rendered grid.
pub struct Client {
    app: Rc<RefCell<App>>,
    ws: WebSocket,
    disposed: Rc<Cell<bool>>,
    ui: Rc<RefCell<UiResources>>,
    _onopen: Option<Closure<dyn FnMut()>>,
    _onmessage: Option<Closure<dyn FnMut(MessageEvent)>>,
    _onerror: Option<Closure<dyn FnMut()>>,
    _onclose: Option<Closure<dyn FnMut(CloseEvent)>>,
}

impl Client {
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

    /// Close the socket and detach all browser callbacks and timers. Idempotent.
    pub fn close(&mut self) {
        let was_disposed = self.disposed.replace(true);
        self.ui.borrow_mut().dispose();
        detach_socket(&self.ws);
        if !was_disposed {
            let _ = self.ws.close_with_code(1000);
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}

struct App {
    session: crate::Session,
    ws: WebSocket,
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    metrics: Metrics,
    /// Cursor blink phase; toggled by an interval in `run`.
    cursor_on: Cell<bool>,
}

impl App {
    fn send(&self, frames: Vec<Vec<u8>>) {
        for f in frames {
            let _ = self.ws.send_with_u8_array(&f);
        }
    }

    fn paint(&self) {
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
