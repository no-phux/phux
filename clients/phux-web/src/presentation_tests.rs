//! Browser regressions against the actual Ghostty replicas and canvas.
#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use super::*;
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
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
    for frame in super::tests::attached_frames(count, b"\x1b[48;2;255;0;0m \x1b[0mreal") {
        assert!(app.borrow_mut().session.on_frame(frame).fatal.is_none());
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

#[wasm_bindgen_test]
async fn glass_hosted_mode_clears_backdrop_borders_and_cursor_then_returns_opaque() {
    let client = fixture(2).await;
    paint_and_publish(&client.app);
    let sample = |x: i32, y: i32| {
        client
            .app
            .borrow()
            .ctx
            .get_image_data(x, y, 1, 1)
            .unwrap()
            .data()
            .to_vec()
    };
    let metrics = client.app.borrow().metrics.clone();
    let blank_y = (metrics.cell_h * 2.0 + 2.0) as i32;
    assert_eq!(sample(2, blank_y)[3], 255, "default client remains opaque");
    client.set_glass_mode(true);
    paint_and_publish(&client.app);
    assert_eq!(sample(2, blank_y), [0, 0, 0, 0], "default rows transparent");
    assert_eq!(sample(0, blank_y), [0, 0, 0, 0], "no native pane border");
    assert_eq!(
        sample((metrics.cell_w * 39.0 + 2.0) as i32, blank_y),
        [0, 0, 0, 0],
        "no opaque split separator"
    );
    assert_eq!(
        sample(3, 3),
        [255, 0, 0, 255],
        "explicit ANSI survives full paint"
    );
    let grid = client.app.borrow().session.grid();
    let cursor_x = (f64::from(grid.cursor_col) * metrics.cell_w + 1.0) as i32;
    let cursor_y = (f64::from(grid.cursor_row) * metrics.cell_h + 1.0) as i32;
    assert_eq!(sample(cursor_x, cursor_y)[3], 255);
    client.app.borrow().blink();
    assert_eq!(
        sample(cursor_x, cursor_y)[3],
        0,
        "cursor row clears rather than filling opaque default"
    );
    client.app.borrow().blink();
    assert_eq!(sample(cursor_x, cursor_y)[3], 255);
    client.app.borrow().blink();
    assert_eq!(sample(cursor_x, cursor_y)[3], 0);
    assert_eq!(sample(3, 3), [255, 0, 0, 255]);
    client.set_glass_mode(false);
    paint_and_publish(&client.app);
    assert_eq!(
        sample(2, blank_y)[3],
        255,
        "opt-out restores opaque presentation"
    );
    client.close();
}
