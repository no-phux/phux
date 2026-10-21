//! WebSocket and hosted-backend event handlers.

use super::*;

pub(super) fn install_transport_failure_hook(app: &Rc<RefCell<App>>) {
    let weak = Rc::downgrade(app);
    let hook = Rc::new(move |message: String| {
        if let Some(app) = weak.upgrade() {
            close_with_transport_error(&app, &message);
        }
    });
    app.borrow().tx.install_failure_hook(hook);
}

pub(super) fn install_websocket_handlers(app: &Rc<RefCell<App>>, ws: &WebSocket) {
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

pub(super) fn install_hosted_websocket_handlers(
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
            Ok(Some(frame)) => {
                let _ = handle_frame(&app, frame);
            }
            Ok(None) => {}
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
                    app.borrow_mut().configure_hosted_backend(&value);
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
            Ok(Some(frame)) => {
                let _ = handle_frame(&app, frame);
            }
            Ok(None) => {}
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

pub(super) fn send_handshake(app: &Rc<RefCell<App>>) {
    let frames = app.borrow().session.handshake();
    let result = app.borrow().send(frames);
    if let Err(message) = result {
        close_with_transport_error(app, &message);
    }
}
