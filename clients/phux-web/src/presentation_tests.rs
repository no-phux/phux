//! Browser regressions against the actual Ghostty replicas and canvas.
#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use super::*;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
};
use phux_protocol::ids::{BootstrapId, ClientId, ResourceId, SessionId, StreamId, WindowId};
use phux_protocol::input::paste::PasteTrust;
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
use wasm_bindgen_test::wasm_bindgen_test;

async fn fixture(count: u32) -> Client {
    let vt = Vt::load().await.unwrap();
    let document = web_sys::window().unwrap().document().unwrap();
    let canvas = document
        .create_element("canvas")
        .unwrap()
        .dyn_into()
        .unwrap();
    let socket = WebSocket::new("ws://127.0.0.1:1/").unwrap();
    let (app, _ready) =
        build_app(&vt, WireTx::Ws(WsTx::new(socket)), canvas, 80, 24, false).unwrap();
    {
        let mut app = app.borrow_mut();
        app.session.on_frame(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new(),
            server_id: vec![],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        });
        app.session.on_frame(FrameKind::Attached {
            attach_id: 1,
            initial_client_id: ClientId::new(1),
            snapshot: SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1))
                .with_resources(
                    (1..=count)
                        .map(|id| ResourceInfo::new(ResourceId::new(id), WindowId::new(1), 80, 24))
                        .collect(),
                ),
        });
        for id in 1..=count {
            let terminal_id = ResourceId::new(id);
            let stream_id = StreamId::new(u64::from(id)).unwrap();
            let bootstrap_id = BootstrapId::new(u64::from(id)).unwrap();
            for frame in [
                FrameKind::BootstrapBegin {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    profile: BootstrapStreamProfile::SynthesizedVtRaw,
                    cols: 80,
                    rows: 24,
                    base_seq: 0,
                },
                FrameKind::BootstrapChunk {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    chunk_seq: 0,
                    payload: bytes::Bytes::from_static(b"\x1b[48;2;255;0;0m \x1b[0mreal"),
                },
                FrameKind::BootstrapReady {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    history_cursor: None,
                },
            ] {
                assert!(app.session.on_frame(frame).fatal.is_none());
            }
        }
        assert!(
            app.session
                .on_frame(FrameKind::AttachReady { attach_id: 1 })
                .fatal
                .is_none()
        );
    }
    Client { app }
}

fn field(value: &JsValue, key: &str) -> JsValue {
    js_sys::Reflect::get(value, &key.into()).unwrap()
}
fn number(value: &JsValue, key: &str) -> f64 {
    field(value, key).as_f64().unwrap()
}

#[wasm_bindgen_test]
async fn presentation_metadata_matches_real_pane_rectangles_and_painted_pixels() {
    let client = fixture(2).await;
    let canvas = client.app.borrow().canvas.clone();
    let renders = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&renders);
    let app = Rc::clone(&client.app);
    let listener =
        Closure::<dyn FnMut(web_sys::CustomEvent)>::new(move |event: web_sys::CustomEvent| {
            // A listener is allowed to borrow mutably: dispatch must be outside App's borrow.
            let reentrant = Client {
                app: Rc::clone(&app),
            };
            reentrant.focus_pane("@1").unwrap();
            recorded.borrow_mut().push(event.detail());
        });
    canvas
        .add_event_listener_with_callback(RENDER_EVENT, listener.as_ref().unchecked_ref())
        .unwrap();
    assert!(renders.borrow().is_empty(), "no event before painting");
    paint_and_publish(&client.app);
    let detail = renders.borrow()[0].clone();
    assert_eq!(number(&detail, "cols"), 80.0);
    assert_eq!(number(&detail, "rows"), 24.0);
    assert_eq!(number(&detail, "pixelWidth"), f64::from(canvas.width()));
    assert_eq!(number(&detail, "pixelHeight"), f64::from(canvas.height()));
    let panes = js_sys::Array::from(&field(&detail, "panes"));
    assert_eq!(panes.length(), 2);
    assert_eq!(
        field(&panes.get(0), "id").as_string().as_deref(),
        Some("@1")
    );
    assert_eq!(number(&panes.get(0), "x"), 0.0);
    assert_eq!(number(&panes.get(0), "cols"), 39.0);
    assert_eq!(number(&panes.get(1), "x"), 40.0);
    assert_eq!(number(&panes.get(1), "cols"), 40.0);
    assert_eq!(number(&panes.get(0), "cursorRow"), 0.0);
    assert_eq!(number(&panes.get(1), "cursorRow"), 0.0);
    {
        let mut app = client.app.borrow_mut();
        app.session.on_frame(FrameKind::ResourceOutput {
            terminal_id: ResourceId::new(2),
            stream_id: StreamId::new(2).unwrap(),
            bootstrap_id: BootstrapId::new(2).unwrap(),
            seq: 1,
            bytes: bytes::Bytes::from_static(b"\r\nnext\r\nrow"),
        });
    }
    paint_and_publish(&client.app);
    let latest = renders.borrow().last().unwrap().clone();
    let latest_panes = js_sys::Array::from(&field(&latest, "panes"));
    assert_eq!(number(&latest_panes.get(0), "cursorRow"), 0.0);
    assert_eq!(number(&latest_panes.get(1), "cursorRow"), 2.0);
    let pixel = client
        .app
        .borrow()
        .ctx
        .get_image_data(3, 3, 1, 1)
        .unwrap()
        .data();
    assert!(
        pixel[0] > 200 && pixel[1] < 60 && pixel[2] < 60,
        "real red VT cell painted"
    );
    canvas
        .remove_event_listener_with_callback(RENDER_EVENT, listener.as_ref().unchecked_ref())
        .unwrap();
    client.close();
}

#[wasm_bindgen_test]
async fn presentation_state_preserves_old_fields_and_direct_focus_rejects_hidden_ids() {
    let client = fixture(2).await;
    let canvas = client.app.borrow().canvas.clone();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let record = Rc::clone(&seen);
    let listener =
        Closure::<dyn FnMut(web_sys::CustomEvent)>::new(move |event: web_sys::CustomEvent| {
            record.borrow_mut().push(event.detail());
        });
    canvas
        .add_event_listener_with_callback("phux-panes", listener.as_ref().unchecked_ref())
        .unwrap();
    client.focus_pane("@2").unwrap();
    let detail = seen.borrow().last().unwrap().clone();
    assert_eq!(number(&detail, "count"), 2.0);
    assert_eq!(field(&detail, "focused").as_string().as_deref(), Some("@2"));
    assert_eq!(field(&detail, "pending").as_bool(), Some(false));
    assert_eq!(js_sys::Array::from(&field(&detail, "panes")).length(), 2);
    assert!(client.focus_pane("@999").is_err());
    assert!(
        client.focus_pane("2").is_err(),
        "canonical exact id required"
    );
    client.app.borrow_mut().session.close_view().unwrap();
    assert!(
        client.focus_pane("@2").is_err(),
        "closed view is not targetable"
    );
    assert!(
        client
            .app
            .borrow()
            .session
            .pane_terminal(&ResourceId::new(2))
            .is_some(),
        "closing view preserves resource"
    );
    client.focus_pane("@1").unwrap();
    client.app.borrow_mut().session.fail_protocol("test");
    assert!(client.focus_pane("@1").is_err());
    canvas
        .remove_event_listener_with_callback("phux-panes", listener.as_ref().unchecked_ref())
        .unwrap();
    client.close();
}

#[wasm_bindgen_test]
async fn presentation_single_pane_resize_uses_published_geometry_until_new_bootstrap() {
    let client = fixture(1).await;
    let _ = client.app.borrow_mut().session.resize_panes(100, 30);
    let detail = client.app.borrow().presentation_detail();
    assert_eq!(number(&detail, "cols"), 80.0);
    assert_eq!(number(&detail, "rows"), 24.0);
    let pane = js_sys::Array::from(&field(&detail, "panes")).get(0);
    assert_eq!(number(&pane, "cols"), 80.0);
    assert_eq!(number(&pane, "rows"), 24.0);
    client.close();
}

#[wasm_bindgen_test]
async fn presentation_send_text_bounds_utf8_and_refuses_unavailable_input() {
    let client = fixture(1).await;
    assert!(client.send_text("").is_ok());
    assert!(client.send_text(&"a".repeat(4097)).is_err());
    assert!(client.send_text(&"é".repeat(2049)).is_err());
    client.app.borrow_mut().session.fail_protocol("test");
    assert!(client.send_text("echo actual").is_err());
    client.close();
}

/// Run against an owned loopback WebSocket echo fixture. This inspects the
/// actual encoded INPUT_PASTE sent by the client, not a JavaScript imitation.
#[wasm_bindgen_test]
#[ignore = "requires owned PHUX_PRESENTATION_ECHO_URL loopback fixture"]
async fn presentation_send_text_sends_one_bounded_structured_paste_without_enter() {
    let url = option_env!("PHUX_PRESENTATION_ECHO_URL").expect("set owned loopback echo fixture");
    let client = fixture(1).await;
    let socket = WebSocket::new(url).unwrap();
    socket.set_binary_type(BinaryType::Arraybuffer);
    for _ in 0..200 {
        if socket.ready_state() == WebSocket::OPEN {
            break;
        }
        TimeoutFuture::new(10).await;
    }
    assert_eq!(socket.ready_state(), WebSocket::OPEN);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let record = Rc::clone(&seen);
    let listener = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        record
            .borrow_mut()
            .push(js_sys::Uint8Array::new(&event.data()).to_vec());
    });
    socket.set_onmessage(Some(listener.as_ref().unchecked_ref()));
    client.app.borrow_mut().tx = WireTx::Ws(WsTx::new(socket.clone()));
    client.focus_pane("@1").unwrap();
    client.send_text("echo café").unwrap();
    for _ in 0..200 {
        if !seen.borrow().is_empty() {
            break;
        }
        TimeoutFuture::new(10).await;
    }
    assert_eq!(seen.borrow().len(), 1);
    let (frame, _) = FrameKind::decode(&seen.borrow()[0]).unwrap();
    match frame {
        FrameKind::InputPaste { terminal_id, event } => {
            assert_eq!(terminal_id, ResourceId::new(1));
            assert_eq!(event.trust, PasteTrust::Trusted);
            assert_eq!(event.data, "echo café".as_bytes());
        }
        other => panic!("expected INPUT_PASTE, got {other:?}"),
    }
    socket.set_onmessage(None);
    client.close();
}
