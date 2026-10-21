mod lifetime;

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
async fn renderer_host_picker_is_absent_for_demo_but_preserved_for_native_sessions() {
    let vt = Vt::load().await.unwrap();
    let document = web_sys::window().unwrap().document().unwrap();
    for (synthesized_only, backend) in [(true, "native"), (false, "native"), (false, "edge")] {
        let parent = document.create_element("div").unwrap();
        let canvas = test_canvas();
        parent.append_child(&canvas).unwrap();
        // No live server: the picker must not perform discovery during install.
        let ws = WebSocket::new("ws://127.0.0.1:1/").unwrap();
        let tx = super::WireTx::Ws(super::WsTx::new(ws));
        let (app, _ready) =
            super::build_app(&vt, tx, canvas.clone(), 80, 24, synthesized_only).unwrap();
        let preamble = js_sys::JSON::parse(&format!("{{\"backend\":\"{backend}\"}}")).unwrap();
        app.borrow_mut().configure_hosted_backend(&preamble);
        super::path_picker::install(&app).unwrap();
        assert_eq!(
            parent
                .query_selector("#phux-path-picker")
                .unwrap()
                .is_none(),
            synthesized_only || backend == "edge"
        );
        assert!(!app.borrow().session.path_pending());
        assert!(canvas.has_attribute("data-phux-cell-width"));
        assert!(canvas.has_attribute("data-phux-cell-height"));
        app.borrow_mut().dispose();
        assert!(
            parent
                .query_selector("#phux-path-picker")
                .unwrap()
                .is_none()
        );
    }
}

fn startup_frames(app: &mut super::App) -> Vec<Vec<u8>> {
    let mut sent = app.session.handshake();
    let mut inbound = FrameBuffer::new();
    inbound.push(&attached_chunk(b"ready"));
    while let Some(bytes) = inbound.next_frame() {
        let (frame, _) = FrameKind::decode(&bytes).unwrap();
        let outcome = app.session.on_frame(frame);
        assert!(outcome.fatal.is_none());
        sent.extend(outcome.send);
    }
    sent
}

#[wasm_bindgen_test]
async fn hosted_edge_policy_omits_only_event_registrations_without_changing_hello() {
    let vt = Vt::load().await.unwrap();
    let subscribe_type = phux_protocol::wire::frame::TYPE_SUBSCRIBE_EVENTS;
    for backend in ["edge", "native", "unknown"] {
        let ws = WebSocket::new("ws://127.0.0.1:1/").unwrap();
        let tx = super::WireTx::Ws(super::WsTx::new(ws));
        let (app, _ready) = super::build_app(&vt, tx, test_canvas(), 80, 24, false).unwrap();
        let mut app = app.borrow_mut();
        let hello = app.session.handshake();
        let preamble = js_sys::JSON::parse(&format!("{{\"backend\":\"{backend}\"}}")).unwrap();
        app.configure_hosted_backend(&preamble);
        assert_eq!(
            app.session.handshake(),
            hello,
            "native codec capabilities are unchanged"
        );
        let all = startup_frames(&mut app);
        let registrations: Vec<_> = all
            .iter()
            .filter(|frame| frame[4] == subscribe_type)
            .collect();
        assert_eq!(
            registrations.len(),
            2,
            "pre-attach and post-ready kernel subscriptions"
        );
        let sent: Vec<_> = all
            .iter()
            .filter(|frame| app.permits_outbound(frame))
            .collect();
        let expected: Vec<_> = all
            .iter()
            .filter(|frame| backend != "edge" || frame[4] != subscribe_type)
            .collect();
        assert_eq!(sent, expected, "no other operation is suppressed");
        assert_eq!(app.host_paths_enabled, backend != "edge");
        assert_eq!(
            app.send(vec![registrations[0].clone()]).is_ok(),
            backend == "edge",
            "edge never transmits the unsupported request; native still uses its transport"
        );
        let error = app.session.on_frame(FrameKind::Error {
            request_id: None,
            code: phux_protocol::wire::frame::ErrorCode::UnknownMessageType,
            message: "genuine operation refusal".to_owned(),
        });
        assert!(error.panes, "inbound errors are still published");
        assert_eq!(app.session.pane_error(), Some("genuine operation refusal"));
        app.dispose();
    }
}

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
        let mut session = crate::Connection::new(&vt, 80, 24);
        let hello = session.on_frame(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new(),
            server_id: Vec::new(),
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        });
        assert_eq!(hello.send.len(), 2, "subscribe before attach snapshot");
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
