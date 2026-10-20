//! App lifetime against the live demo server: establishment, retention, teardown.

use super::*;

#[wasm_bindgen_test]
async fn dropping_polled_establishment_disposes_app_reader_and_transport() {
    let ws = open_websocket().await;
    let vt = Vt::load().await.unwrap();
    let tx = crate::client::WireTx::Ws(crate::client::WsTx::new(ws.clone()));
    let (app, _ready) = crate::client::build_app(&vt, tx, test_canvas(), 80, 24, false).unwrap();
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
        let _guard = crate::client::AppEstablishment::new(app);
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
        let guard = crate::client::WebTransportAttempt::new(transport);
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
    let app = crate::client::RETAINED_APP_FOR_TEST
        .with(|retained| retained.borrow().as_ref().and_then(Weak::upgrade))
        .expect("exported start retained its live App");
    assert!(!app.borrow().session.is_failed());

    crate::client::close_with_transport_error(&app, "test cleanup");
    drop(app);
    assert!(
        crate::client::RETAINED_APP_FOR_TEST
            .with(|retained| retained.borrow().as_ref().and_then(Weak::upgrade))
            .is_none()
    );
    canvas.remove();
}

/// A protocol-major refusal stops the client for good: the canvas says
/// why, and the armed reconnect is dropped instead of redialing.
#[wasm_bindgen_test]
async fn a_version_refusal_marks_the_canvas_and_never_redials() {
    let ws = open_websocket().await;
    let vt = Vt::load().await.unwrap();
    let tx = crate::client::WireTx::Ws(crate::client::WsTx::new(ws));
    let canvas = test_canvas();
    let (app, _ready) = crate::client::build_app(&vt, tx, canvas.clone(), 80, 24, false).unwrap();
    // A redial to a closed port would fail and mark `disconnected`.
    app.borrow()
        .reconnect
        .replace(Some(crate::client::ReconnectConfig {
            wt_url: None,
            ws_url: "ws://127.0.0.1:9/".to_owned(),
            canvas: canvas.clone(),
            cols: 80,
            rows: 24,
        }));
    let mut refusal = bytes::BytesMut::new();
    FrameKind::Error {
        request_id: None,
        code: phux_protocol::wire::frame::ErrorCode::VersionIncompatible,
        message: "client protocol 9.0 is not supported".to_owned(),
    }
    .encode(&mut refusal);

    assert_eq!(
        crate::client::process_webtransport_chunk(&app, &mut FrameBuffer::new(), &refusal),
        ReceiveFlow::Stop
    );
    assert!(app.borrow().session.is_version_refused());
    assert!(
        app.borrow().reconnect.borrow().is_none(),
        "reconnect dropped"
    );
    TimeoutFuture::new(600).await;
    assert_eq!(
        canvas
            .get_attribute(crate::client::CONNECTION_ATTRIBUTE)
            .as_deref(),
        Some("incompatible"),
        "no redial replaced the verdict"
    );
    let reason = canvas
        .get_attribute(crate::client::ERROR_ATTRIBUTE)
        .unwrap_or_default();
    assert!(reason.contains("client protocol 9.0"), "{reason}");
    assert!(crate::session::is_version_refusal(&reason), "{reason}");
}

#[wasm_bindgen_test]
async fn closed_ws_send_and_repeated_reconnect_teardown_release_every_app() {
    let ws = closed_websocket().await;
    let vt = Vt::load().await.unwrap();
    let document = web_sys::window().unwrap().document().unwrap();
    for _ in 0..8 {
        let tx = crate::client::WireTx::Ws(crate::client::WsTx::new(ws.clone()));
        let (app, _ready) =
            crate::client::build_app(&vt, tx, test_canvas(), 80, 24, false).unwrap();
        crate::client::install_transport_failure_hook(&app);
        crate::client::install_websocket_handlers(&app, &ws);
        crate::client::install_input(&app).unwrap();
        crate::client::install_cursor_blink(&app).unwrap();
        app.borrow().self_owner.replace(Some(Rc::clone(&app)));
        let weak = Rc::downgrade(&app);

        crate::client::send_handshake(&app);
        assert!(app.borrow().session.is_failed());
        assert!(app.borrow().self_owner.borrow().is_none());
        assert_eq!(
            app.borrow()
                .canvas
                .get_attribute(crate::client::CONNECTION_ATTRIBUTE)
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
    let tx = crate::client::WireTx::Ws(crate::client::WsTx::new(ws));
    let (app, _ready) = crate::client::build_app(&vt, tx, test_canvas(), 80, 24, false).unwrap();
    let pending = |app: &Rc<std::cell::RefCell<crate::client::App>>| {
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
