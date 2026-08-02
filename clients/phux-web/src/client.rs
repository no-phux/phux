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
use std::rc::Rc;

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
    let ws = WebSocket::new(ws_url)?;
    ws.set_binary_type(BinaryType::Arraybuffer);

    let app = build_app(WireTx::Ws(ws.clone()), canvas, cols, rows).await?;

    // On open: send HELLO. Session dispatch sends ATTACH after HELLO_OK.
    {
        let app = Rc::clone(&app);
        let onopen = Closure::<dyn FnMut()>::new(move || {
            let a = app.borrow();
            a.send(a.session.handshake());
        });
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen.forget();
    }

    // On message: decode one frame, drive the session, ack, repaint. Each
    // binary message is one complete encoded frame — no reassembly.
    {
        let app = Rc::clone(&app);
        let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let buf = js_sys::Uint8Array::new(&e.data()).to_vec();
            if app.borrow().session.is_failed() {
                return;
            }
            match decode_server_frame(&app, &buf) {
                Ok(frame) => {
                    let _ = handle_frame(&app, frame);
                }
                Err(message) => close_with_protocol_error(&app, &message),
            }
        });
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();
    }

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
/// `WebTransport` API cannot set request headers). `ws_url` is the
/// WebSocket URL used as the fallback.
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
        Err(err) => {
            web_sys::console::warn_2(
                &JsValue::from_str("phux-web: WebTransport unavailable; falling back to WebSocket"),
                &err,
            );
            run(ws_url, canvas, cols, rows).await
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
    let wt = WebTransport::new(wt_url)?;
    if let Err(err) = JsFuture::from(wt.ready()).await {
        wt.close();
        return Err(err);
    }

    // One bidirectional stream carries the whole wire, mirroring the QUIC
    // transport's one-stream-per-connection contract.
    let stream = match JsFuture::from(wt.create_bidirectional_stream()).await {
        Ok(stream) => stream,
        Err(err) => {
            wt.close();
            return Err(err);
        }
    };
    let writer = WritableStreamDefaultWriter::new(&stream.writable())?;
    let reader = ReadableStreamDefaultReader::new(&stream.readable())?;

    let app = build_app(
        WireTx::Wt {
            writer,
            session: wt.clone(),
        },
        canvas,
        cols,
        rows,
    )
    .await?;

    // The session is already established (unlike the WebSocket path there is
    // no onopen moment): send HELLO now; ATTACH follows HELLO_OK.
    {
        let a = app.borrow();
        a.send(a.session.handshake());
    }

    // Read pump: stream chunks land at arbitrary boundaries, so reassemble
    // complete frames before decoding. `wt` is moved in to keep the session
    // handle alive for the pump's lifetime.
    {
        let app = Rc::clone(&app);
        wasm_bindgen_futures::spawn_local(async move {
            let _session = wt;
            let mut frames = FrameBuffer::new();
            loop {
                let result = match JsFuture::from(reader.read()).await {
                    Ok(result) => result,
                    Err(error) => {
                        let message = format!("WebTransport read failed: {error:?}");
                        let _ = webtransport_exit_flow(
                            WebTransportExit::ReadError(&message),
                            |message, protocol| {
                                close_webtransport_exit(&app, message, protocol);
                            },
                        );
                        return;
                    }
                };
                let done = js_sys::Reflect::get(&result, &JsValue::from_str("done"))
                    .ok()
                    .and_then(|d| d.as_bool())
                    .unwrap_or(true);
                if done {
                    let exit = webtransport_eof_exit(&frames);
                    let _ = webtransport_exit_flow(exit, |message, protocol| {
                        close_webtransport_exit(&app, message, protocol);
                    });
                    return;
                }
                let Ok(value) = js_sys::Reflect::get(&result, &JsValue::from_str("value")) else {
                    let _ = webtransport_exit_flow(
                        WebTransportExit::MissingValue,
                        |message, protocol| {
                            close_webtransport_exit(&app, message, protocol);
                        },
                    );
                    return;
                };
                frames.push(&js_sys::Uint8Array::new(&value).to_vec());
                while let Some(framed) = frames.next_frame() {
                    match decode_server_frame(&app, &framed) {
                        Ok(frame) => {
                            if matches!(handle_frame(&app, frame), ReceiveFlow::Stop) {
                                return;
                            }
                        }
                        Err(message) => {
                            close_with_protocol_error(&app, &message);
                            return;
                        }
                    }
                }
                if matches!(
                    poisoned_framing_flow(&frames, |message| {
                        close_with_protocol_error(&app, message);
                    }),
                    ReceiveFlow::Stop
                ) {
                    return;
                }
            }
        });
    }

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
}

/// The send half of whichever transport carried the connection. Both carry
/// identical encoded frames; only the byte-stream mechanics differ.
enum WireTx {
    /// One binary message per frame.
    Ws(WebSocket),
    /// Length-prefixed frames over the session's single bidirectional stream.
    Wt {
        writer: WritableStreamDefaultWriter,
        session: WebTransport,
    },
}

impl WireTx {
    fn send(&self, frame: &[u8]) {
        match self {
            Self::Ws(ws) => {
                let _ = ws.send_with_u8_array(frame);
            }
            Self::Wt { writer, .. } => {
                let chunk = js_sys::Uint8Array::from(frame);
                // Writer chunks queue in call order; await the promise off
                // the hot path only to observe (and drop) failures, so a
                // rejected write never surfaces as an unhandled rejection.
                let pending = writer.write_with_chunk(&chunk);
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = JsFuture::from(pending).await;
                });
            }
        }
    }

    fn close(&self) {
        match self {
            Self::Ws(ws) => {
                let _ = ws.close();
            }
            Self::Wt { writer, session } => {
                session.close();
                let pending = writer.close();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = JsFuture::from(pending).await;
                });
            }
        }
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
}

impl App {
    fn send(&self, frames: Vec<Vec<u8>>) {
        for f in frames {
            self.tx.send(&f);
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

/// Load the engine, grab the canvas 2D context, and assemble the shared
/// [`App`] around an established transport send half.
async fn build_app(
    tx: WireTx,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
) -> Result<Rc<RefCell<App>>, JsValue> {
    let vt = Vt::load().await?;
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("no 2D context"))?
        .dyn_into()?;

    Ok(Rc::new(RefCell::new(App {
        session: crate::Session::new(&vt, cols, rows),
        tx,
        canvas,
        ctx,
        metrics: Metrics::default(),
        cursor_on: Cell::new(true),
    })))
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
        WebTransportExit::PartialEof => (
            "WebTransport stream ended in the middle of a frame",
            true,
        ),
        WebTransportExit::ReadError(message) => (message, false),
        WebTransportExit::MissingValue => (
            "WebTransport read result omitted a chunk value",
            true,
        ),
    };
    close(message, protocol);
    ReceiveFlow::Stop
}

fn poisoned_framing_flow(
    frames: &FrameBuffer,
    close: impl FnOnce(&str),
) -> ReceiveFlow {
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
    let mut a = app.borrow_mut();
    let outcome = a.session.on_frame(frame);
    if let Some(message) = outcome.fatal {
        web_sys::console::error_1(&JsValue::from_str(&format!(
            "phux-web protocol error: {message}",
        )));
        a.tx.close();
        return ReceiveFlow::Stop;
    }
    if !outcome.send.is_empty() {
        a.send(outcome.send);
    }
    if outcome.render {
        a.paint();
    }
    ReceiveFlow::Continue
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
    app.session.fail_protocol(message);
    app.tx.close();
}

fn close_webtransport_exit(app: &Rc<RefCell<App>>, message: &str, protocol: bool) {
    let kind = if protocol {
        "protocol error"
    } else {
        "transport closed"
    };
    web_sys::console::error_1(&JsValue::from_str(&format!(
        "phux-web {kind}: {message}",
    )));
    let mut app = app.borrow_mut();
    app.session.fail_protocol(message);
    app.tx.close();
}

/// Keyboard: each keydown becomes an `INPUT_KEY` for the attached terminal.
fn install_keyboard(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let app = Rc::clone(app);
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let onkey = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
        let Some(event) = key_event_from_browser(&e) else {
            return;
        };
        let mut a = app.borrow_mut();
        if let Some(frame) = a.session.key_frame(event) {
            a.tx.send(&frame);
            e.prevent_default();
        }
    });
    document.add_event_listener_with_callback("keydown", onkey.as_ref().unchecked_ref())?;
    onkey.forget();
    Ok(())
}

/// Cursor blink: toggle the phase and repaint on a fixed cadence.
fn install_cursor_blink(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let app = Rc::clone(app);
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let blink = Closure::<dyn FnMut()>::new(move || {
        let a = app.borrow();
        a.cursor_on.set(!a.cursor_on.get());
        a.paint();
    });
    window.set_interval_with_callback_and_timeout_and_arguments_0(
        blink.as_ref().unchecked_ref(),
        530,
    )?;
    blink.forget();
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
    use super::{
        FrameBuffer, ReceiveFlow, WebTransportExit, poisoned_framing_flow,
        webtransport_eof_exit, webtransport_exit_flow,
    };
    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ServerCapabilities};
    use phux_protocol::ids::{
        BootstrapId, ClientId, SessionId, StreamId, TerminalId, WindowId,
    };
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
    use phux_protocol::wire::frame::{FrameKind, MAX_FRAME_LEN};
    use phux_protocol::wire::info::{SessionSnapshot, TerminalInfo};
    use phux_vt_web::Vt;
    use wasm_bindgen_test::wasm_bindgen_test;

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
            let terminal_id = TerminalId::local(1);
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
                        .with_panes(vec![TerminalInfo::new(
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

            let after_exit = session.on_frame(FrameKind::TerminalOutput {
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
}
