//! Browser input delivery, ordering, reconnect, and presentation-fence regressions.

use super::*;

fn paste_input(text: &str) -> InputEvent {
    InputEvent::Paste(crate::input::paste_event(text).unwrap())
}

fn input_attempt(frame: &FrameKind) -> (u32, phux_protocol::ids::InputOperationId) {
    match frame {
        FrameKind::Command { request_id, command: phux_protocol::wire::frame::Command::ApplyInput { operation_id, .. } } => (*request_id, *operation_id),
        other => panic!("expected APPLY_INPUT, got {other:?}"),
    }
}

fn settle_input(session: &mut crate::Connection, request_id: u32) {
    session.on_frame(FrameKind::CommandResult {
        request_id,
        result: phux_protocol::wire::frame::CommandResult::Ok,
    });
}

#[wasm_bindgen_test]
async fn acknowledged_paste_orders_later_keys_and_refuses_large_raw_overtake() {
    use phux_client_core::input_replay::InputSubmission;
    let client = fixture_with_server(1, Some(7)).await;
    let mut app = client.app.borrow_mut();
    let (first, raw) = app.session.submit_browser_input(paste_input("small\npaste"));
    assert!(matches!(first, InputSubmission::AwaitingReceipt { .. }));
    assert!(raw.is_none());
    let first_frame = app.session.input_attempts().remove(0);
    let key = crate::input::key_events_for_text("x").remove(0);
    let (later, raw) = app.session.submit_browser_input(InputEvent::Key(key));
    assert!(matches!(later, InputSubmission::AwaitingReceipt { .. }));
    assert!(raw.is_none(), "a key cannot overtake the unresolved paste");
    assert!(app.session.input_attempts().is_empty());
    let large = "z".repeat(128 * 1024);
    assert!(matches!(app.session.prepare_user_paste(&large), Err(InputSubmission::Refused { .. })));
    let refused = app.session.take_input_reports();
    assert_eq!(refused.len(), 1);
    assert!(refused[0].1.message.contains("unresolved"));
    settle_input(&mut app.session, input_attempt(&first_frame).0);
    let key_frame = app.session.input_attempts().remove(0);
    assert_ne!(input_attempt(&first_frame).1, input_attempt(&key_frame).1);
    settle_input(&mut app.session, input_attempt(&key_frame).0);
    let (submission, frame) = app.session.submit_browser_input(paste_input(&large));
    assert_eq!(submission, InputSubmission::QueuedRaw);
    let frame = frame.unwrap();
    assert!(matches!(phux_wire_fixtures::decode_one(&frame), FrameKind::InputPaste { event, .. } if event.data == large.as_bytes()));
    assert!(app.session.input_attempts().is_empty(), "raw fallback stays one atomic frame");
}

#[wasm_bindgen_test]
async fn lost_receipt_reconnect_reuses_original_operation_and_correlation() {
    use phux_client_core::input_replay::{InputSubmission, ReplayDisposition};
    let first = fixture_with_server(1, Some(7)).await;
    let (state, original, delivery_id) = {
        let mut app = first.app.borrow_mut();
        let (submission, _) = app.session.submit_browser_input(paste_input("once"));
        let InputSubmission::AwaitingReceipt { delivery_id } = submission else { panic!("{submission:?}"); };
        let original = app.session.input_attempts().remove(0);
        app.session.fail_protocol("reply lost");
        (app.session.input_delivery_state(), original, delivery_id)
    };
    let second = fixture_with_server(1, Some(7)).await;
    let mut app = second.app.borrow_mut();
    app.session.adopt_input_delivery(state);
    assert!(first.app.borrow().session.input_attempts().is_empty(), "the retired browser connection cannot claim replay handoffs");
    let retry = app.session.input_attempts().remove(0);
    assert_eq!(input_attempt(&original).1, input_attempt(&retry).1);
    assert_ne!(input_attempt(&original).0, input_attempt(&retry).0);
    settle_input(&mut app.session, input_attempt(&original).0);
    assert!(app.session.take_input_reports().is_empty(), "old socket reply cannot settle new correlation");
    settle_input(&mut app.session, input_attempt(&retry).0);
    let reports = app.session.take_input_reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].0, delivery_id);
    assert_eq!(reports[0].1.disposition, ReplayDisposition::Delivered);
}

#[wasm_bindgen_test]
async fn replacement_incarnation_distinguishes_attempted_from_definitely_unsent() {
    use phux_client_core::input_replay::ReplayDisposition;
    let first = fixture_with_server(2, Some(7)).await;
    let state = {
        let mut app = first.app.borrow_mut();
        app.session.focus_pane(&ResourceId::new(1)).unwrap();
        let _ = app.session.submit_browser_input(paste_input("attempted"));
        app.session.focus_pane(&ResourceId::new(2)).unwrap();
        let _ = app.session.submit_browser_input(paste_input("never handed off"));
        let frames = app.session.input_attempts();
        assert_eq!(frames.len(), 2);
        app.session.rollback_input_unsent(&frames[1..]);
        app.session.fail_protocol("socket failed during first handoff");
        app.session.input_delivery_state()
    };
    let replacement = fixture_with_server(2, Some(8)).await;
    let mut app = replacement.app.borrow_mut();
    app.session.adopt_input_delivery(state);
    let reports = app.session.take_input_reports();
    // The attempted operation cannot cross incarnation; the never-sent action
    // is still safe to attempt for the first time against the replacement.
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].1.disposition, ReplayDisposition::Unknown);
    assert!(first.app.borrow().session.input_presentation(&ResourceId::new(1)).is_none(), "stale pixels from the retired connection are not presentation evidence");
    let frames = app.session.input_attempts();
    assert_eq!(frames.len(), 1);
    app.session.rollback_input_unsent(&frames);
    app.session.stop_input_delivery(false);
    assert_eq!(app.session.take_input_reports()[0].1.disposition, ReplayDisposition::Refused);
}

#[wasm_bindgen_test]
async fn stale_hidden_and_clipped_presentation_cannot_release_delivery_fence() {
    use phux_client_core::input_replay::InputSubmission;
    let client = fixture_with_server(1, Some(7)).await;
    let terminal = ResourceId::new(1);
    let canvas = client.app.borrow().canvas.clone();
    let document = canvas.owner_document().unwrap();
    document.body().unwrap().append_child(&canvas).unwrap();
    canvas.style().set_property("position", "fixed").unwrap();
    canvas.style().set_property("left", "0").unwrap();
    canvas.style().set_property("top", "0").unwrap();
    canvas.style().set_property("width", "300px").unwrap();
    canvas.style().set_property("height", "200px").unwrap();
    {
        let mut app = client.app.borrow_mut();
        let _ = app.session.submit_browser_input(paste_input("ambiguous"));
        let attempt = app.session.input_attempts().remove(0);
        app.session.on_frame(FrameKind::CommandResult {
            request_id: input_attempt(&attempt).0,
            result: phux_protocol::wire::frame::CommandResult::Error {
                code: phux_protocol::wire::frame::ErrorCode::InputDeliveryUnknown,
                message: "write completion was lost".to_owned(),
            },
        });
        let old = app.session.input_presentation(&terminal).unwrap();
        app.session.on_frame(phux_wire_fixtures::output_on(&terminal, [1, 1], 1, b"fresh"));
        app.session.present_input(&terminal, &old);
        assert!(matches!(app.session.submit_browser_input(paste_input("blocked")).0, InputSubmission::Refused { .. }));
        let previous_generation = app.session.input_presentation(&terminal).unwrap();
        for frame in phux_wire_fixtures::bootstrap_frames(&terminal, [1, 2], [80, 24], b"replacement") {
            assert!(app.session.on_frame(frame).fatal.is_none());
        }
        app.session.present_input(&terminal, &previous_generation);
        assert!(matches!(app.session.submit_browser_input(paste_input("stale generation")).0, InputSubmission::Refused { .. }));
    }
    canvas.style().set_property("visibility", "hidden").unwrap();
    assert!(client.app.borrow().paint());
    assert!(matches!(client.app.borrow_mut().session.submit_browser_input(paste_input("still blocked")).0, InputSubmission::Refused { .. }));
    canvas.style().set_property("visibility", "visible").unwrap();
    canvas.style().set_property("left", "-20px").unwrap();
    assert!(client.app.borrow().paint());
    assert!(matches!(client.app.borrow_mut().session.submit_browser_input(paste_input("clipped")).0, InputSubmission::Refused { .. }));
    canvas.style().set_property("left", "0").unwrap();
    canvas.style().set_property("clip-path", "inset(1px)").unwrap();
    assert!(client.app.borrow().paint());
    assert!(matches!(client.app.borrow_mut().session.submit_browser_input(paste_input("masked")).0, InputSubmission::Refused { .. }));
    canvas.style().remove_property("clip-path").unwrap();
    assert!(client.app.borrow().paint());
    assert!(matches!(client.app.borrow_mut().session.submit_browser_input(paste_input("now admitted")).0, InputSubmission::AwaitingReceipt { .. }));
    canvas.remove();
}

#[wasm_bindgen_test]
async fn expiration_strands_attempted_paste_and_refuses_its_unsent_successor() {
    use phux_client_core::input_replay::{INPUT_RETRY_HORIZON, ReplayDisposition};
    let client = fixture_with_server(1, Some(7)).await;
    let mut app = client.app.borrow_mut();
    let _ = app.session.submit_browser_input(paste_input("attempted"));
    assert_eq!(app.session.input_attempts().len(), 1);
    let _ = app.session.submit_browser_input(paste_input("unsent successor"));
    let now = web_sys::window().unwrap().performance().unwrap().now() as u64;
    assert!(app.session.input_attempts_at(now + INPUT_RETRY_HORIZON.as_millis() as u64 + 1).is_empty());
    let reports = app.session.take_input_reports();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].1.disposition, ReplayDisposition::Unknown);
    assert_eq!(reports[1].1.disposition, ReplayDisposition::Refused);
    assert!(app.session.key_frame(crate::input::key_events_for_text("x").remove(0)).is_none());
    assert!(app.session.input_presentation(&ResourceId::new(1)).is_some());
    app.session.on_frame(phux_wire_fixtures::closed(&ResourceId::new(1), Some(0)));
    assert!(app.session.input_presentation(&ResourceId::new(1)).is_none(), "authoritative retirement clears its fence");
}

#[wasm_bindgen_test]
async fn websocket_rejection_before_send_is_refused_not_unknown_or_delivered() {
    let client = fixture_with_server(1, Some(7)).await;
    // The fixture's real WebSocket is not OPEN. This exercises the transport's
    // actual ready-state refusal, not a fake delivery or injected receipt.
    assert!(client.submit_user_paste("cannot be handed off").is_err());
    let canvas = client.app.borrow().canvas.clone();
    assert_eq!(canvas.get_attribute("data-phux-input-status").as_deref(), Some("refused"));
    assert!(canvas.get_attribute("data-phux-input-notice").unwrap().contains("safe to retype"));
    assert!(canvas.has_attribute("data-phux-input-delivery-id"));
    assert!(client.app.borrow().session.input_attempts().is_empty());
}

#[wasm_bindgen_test]
async fn unsupported_server_and_satellite_pastes_remain_atomic_raw() {
    use phux_client_core::input_replay::InputSubmission;
    let unsupported = fixture(1).await;
    let (outcome, raw) = unsupported.app.borrow_mut().session.submit_browser_input(paste_input("compatibility"));
    assert_eq!(outcome, InputSubmission::QueuedRaw);
    assert!(matches!(phux_wire_fixtures::decode_one(&raw.unwrap()), FrameKind::InputPaste { .. }));
    assert!(unsupported.app.borrow().session.input_attempts().is_empty());

    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
    let terminal = ResourceId::satellite("remote", 1);
    let vt = Vt::load().await.unwrap();
    let mut session = crate::Connection::new(&vt, 80, 24);
    let mut hello = super::super::tests::attached_frames(1, b"")[0].clone();
    if let FrameKind::HelloOk { server_id, server_caps, .. } = &mut hello {
        *server_id = vec![7; 16];
        server_caps.features = phux_protocol::caps::ServerFeatureSet::with(&[
            phux_protocol::caps::ServerFeature::AcknowledgedInput,
        ]);
    }
    assert!(session.on_frame(hello).fatal.is_none());
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal.clone())
        .with_resources(vec![ResourceInfo::new(terminal.clone(), WindowId::new(1), 80, 24)]);
    assert!(session.on_frame(phux_wire_fixtures::attached(1, snapshot, 1)).fatal.is_none());
    for frame in phux_wire_fixtures::bootstrap_frames(&terminal, [1, 1], [80, 24], b"remote") {
        assert!(session.on_frame(frame).fatal.is_none());
    }
    session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    let (outcome, raw) = session.submit_browser_input(paste_input("remote paste"));
    assert_eq!(outcome, InputSubmission::QueuedRaw);
    assert!(matches!(phux_wire_fixtures::decode_one(&raw.unwrap()), FrameKind::InputPaste { terminal_id, event } if terminal_id == terminal && event.data == b"remote paste"));
    assert!(session.input_attempts().is_empty(), "a hub capability is not a satellite route guarantee");
}

#[wasm_bindgen_test]
async fn canonical_receipt_event_preserves_correlation_and_allows_reentrant_client_calls() {
    use phux_client_core::input_replay::InputSubmission;
    let client = fixture(1).await;
    let canvas = client.app.borrow().canvas.clone();
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&events);
    let app = Rc::clone(&client.app);
    let listener = Closure::<dyn FnMut(web_sys::CustomEvent)>::new(move |event: web_sys::CustomEvent| {
        let reentrant = Client { app: Rc::clone(&app) };
        reentrant.focus_pane("@1").unwrap();
        recorded.borrow_mut().push(event.detail());
    });
    canvas.add_event_listener_with_callback(INPUT_DELIVERY_EVENT, listener.as_ref().unchecked_ref()).unwrap();
    let submission = client.submit_user_paste(&"x".repeat(MAX_OUTBOUND_BYTES)).unwrap();
    let InputSubmission::Refused { delivery_id } = submission else { panic!("{submission:?}"); };
    assert!(events.borrow().is_empty(), "receipt dispatch cannot run under the app borrow");
    TimeoutFuture::new(0).await;
    let event = events.borrow()[0].clone();
    assert_eq!(field(&event, "status").as_string().as_deref(), Some("refused"));
    assert_eq!(field(&event, "deliveryId").as_string().unwrap(), delivery_id.to_string());
    assert!(field(&event, "message").as_string().unwrap().contains("byte budget"));
    canvas.remove_event_listener_with_callback(INPUT_DELIVERY_EVENT, listener.as_ref().unchecked_ref()).unwrap();
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
