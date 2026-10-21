//! Fed attaches and transport batches: ordering, publication, and fatal endings.

use super::*;

#[cfg(feature = "engine")]
fn observed_text(plane: &ControlPlane) -> String {
    plane
        .publication()
        .acquire(&terminal())
        .expect("published frame")
        .text()
}

#[cfg(not(feature = "engine"))]
fn observed_text(plane: &ControlPlane) -> String {
    String::from_utf8_lossy(&plane.engine().expect("engine").take_output(&terminal())).into_owned()
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "one attach-to-input scenario; every assert! scores as a branch"
)]
fn a_fed_attach_publishes_the_terminal_and_input_goes_out_as_frames() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"ready");
    assert_eq!(plane.status(), Status::Attached);
    assert!(plane.attached_once());
    let topology = plane.topology().expect("topology");
    assert_eq!(topology.panes.len(), 1);
    assert_eq!(topology.panes[0].session_name, "main");
    assert_eq!(plane.attached_session(), Some(1));

    let events = plane.take_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::TerminalChanged { terminal_id } if *terminal_id == terminal())),
        "the bootstrap changed the terminal: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::TopologyChanged))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Attached { attach_id: id } if *id == attach_id))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::StatusChanged(Status::Attached)))
    );
    assert!(observed_text(&plane).contains("ready"));

    #[cfg(feature = "engine")]
    {
        let first = plane.publication().acquire(&terminal()).expect("frame");
        assert_eq!(first.generation, 1);
        assert_eq!(plane.publication().generation(&terminal()), Some(1));
        output(&mut plane, 1, b"\x1b[3;1Hthird");
        let second = plane.publication().acquire(&terminal()).expect("frame");
        assert_eq!(second.generation, 2);
        assert_eq!(second.row_text(2), "third");
        assert!(second.is_row_dirty(2));
        assert_eq!(first.text(), "ready", "the held frame never changes");
    }
    #[cfg(not(feature = "engine"))]
    {
        output(&mut plane, 1, b"more");
        assert_eq!(observed_text(&plane), "more");
    }
    assert!(plane.input_ready(&terminal()));
    // The kernel re-subscribes to events when the attach barrier releases.
    let released = plane.take_outbound();
    assert!(
        released
            .iter()
            .any(|frame| matches!(decode(frame), FrameKind::SubscribeEvents { .. }))
    );

    assert!(plane.send_text(&terminal(), "hi"));
    let keys = plane.take_outbound();
    assert_eq!(keys.len(), 2);
    assert!(matches!(decode(&keys[0]), FrameKind::InputKey { .. }));

    let request_id = plane.spawn_terminal(SpawnRequest {
        command: Some(vec!["/bin/cat".to_owned()]),
        ..SpawnRequest::default()
    });
    let spawn = plane.take_outbound();
    assert!(matches!(
        decode(&spawn[0]),
        FrameKind::SpawnResource { request_id: id, .. } if id == request_id
    ));
    plane
        .feed(FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Ok(ResourceId::local(9)),
        })
        .expect("RESOURCE_SPAWNED");
    let events = plane.take_events();
    assert!(events.iter().any(|event| matches!(
        event,
        Event::TerminalSpawned { request_id: id, terminal_id: Some(spawned), error: None }
            if *id == request_id && *spawned == ResourceId::local(9)
    )));
    let refresh = plane.take_outbound();
    assert!(matches!(
        decode(&refresh[0]),
        FrameKind::Command {
            command: Command::GetState { .. },
            ..
        }
    ));
}

#[cfg(feature = "engine")]
#[test]
fn a_transport_batch_publishes_once_and_preserves_ordered_changes() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"ready");
    let _ = plane.take_events();
    // A caught-up consumer, so the batch publishes without being asked.
    let before = plane
        .publication()
        .acquire(&terminal())
        .expect("bootstrap published")
        .generation;
    let event_count = 32_u64;
    let frames = (1..=event_count)
        .map(|seq| {
            encode(&FrameKind::ResourceOutput {
                terminal_id: terminal(),
                stream_id: StreamId::new(1).expect("stream"),
                bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                seq,
                bytes: b"x".to_vec().into(),
            })
        })
        .collect::<Vec<_>>();

    plane
        .feed_bytes_batch(&frames)
        .expect("ordered transport batch");

    assert_eq!(
        plane.publication().generation(&terminal()),
        Some(before + 1)
    );
    assert_eq!(
        observed_text(&plane).matches('x').count(),
        usize::try_from(event_count).expect("fixture count fits usize")
    );
    assert_eq!(
        plane
            .take_events()
            .into_iter()
            .filter(|event| matches!(event, Event::TerminalChanged { .. }))
            .count(),
        1,
        "the binding wake queue coalesces duplicate terminal changes"
    );
}

#[cfg(feature = "engine")]
#[test]
fn a_control_frame_splits_engine_batches_without_reordering() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"ready");
    let _ = plane.take_outbound();
    let before = plane
        .publication()
        .acquire(&terminal())
        .expect("bootstrap published")
        .generation;
    let output = |seq, bytes: &'static [u8]| {
        encode(&FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).expect("stream"),
            bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
            seq,
            bytes: bytes.to_vec().into(),
        })
    };
    let frames = vec![
        output(1, b"one"),
        encode(&FrameKind::Ping { nonce: 17 }),
        output(2, b"two"),
    ];

    plane.feed_bytes_batch(&frames).expect("ordered batch");

    let frame = plane.publication().acquire(&terminal()).expect("published");
    assert_eq!(frame.generation, before + 2);
    assert_eq!(frame.last_seq, 2);
    assert!(matches!(
        decode(&plane.take_outbound()[0]),
        FrameKind::Pong { nonce: 17 }
    ));
}

#[cfg(feature = "engine")]
#[test]
fn a_valid_batch_prefix_is_applied_before_a_later_decode_error() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"ready");
    let before = plane
        .publication()
        .generation(&terminal())
        .expect("bootstrap published");
    let frames = vec![
        encode(&FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).expect("stream"),
            bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
            seq: 1,
            bytes: b"valid".to_vec().into(),
        }),
        vec![0, 0, 0, 0],
    ];

    let error = plane
        .feed_bytes_batch(&frames)
        .expect_err("malformed second frame");

    assert!(matches!(error, ControlError::Protocol(_)));
    let frame = plane.publication().acquire(&terminal()).expect("published");
    assert_eq!(frame.generation, before + 1);
    assert_eq!(frame.last_seq, 1);
}

#[cfg(feature = "engine")]
#[test]
fn a_control_ending_outranks_an_earlier_stale_engine_frame() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"ready");
    let frames = vec![
        encode(&FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: StreamId::new(2).expect("stale stream"),
            bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
            seq: 1,
            bytes: b"stale".to_vec().into(),
        }),
        encode(&FrameKind::Detached {
            reason: Some(DetachReason::ProtocolError),
            message: "fatal ending".to_owned(),
        }),
    ];

    let error = plane
        .feed_bytes_batch(&frames)
        .expect_err("fatal ending must not be hidden");

    assert!(
        matches!(&error, ControlError::Refused(message) if message.contains("fatal ending")),
        "expected the ending to outrank stale generation, got {error:?}"
    );
}

/// A revoked or expired authorization ends the attach before the socket
/// closes; nothing typed in that window may be admitted toward the wire.
#[test]
fn input_is_not_admitted_after_the_server_ends_the_attach() {
    for reason in [
        DetachReason::AuthorizationRevoked,
        DetachReason::AuthorizationExpired,
        DetachReason::SessionKilled,
    ] {
        let (mut plane, attach_id) = negotiated();
        attach(&mut plane, attach_id, b"ready");
        assert!(plane.input_ready(&terminal()));
        let _ = plane.take_outbound();
        plane
            .feed(FrameKind::Detached {
                reason: Some(reason),
                message: String::new(),
            })
            .expect("a non-fatal ending");
        assert!(!plane.input_ready(&terminal()), "{reason:?}");
        assert!(plane.take_events().iter().any(|event| matches!(
            event,
            Event::Detached { reason: Some(ended), .. } if *ended == reason
        )));
        // A fresh connection may attach again.
        plane.connection_opened();
        assert!(!plane.input_ready(&terminal()), "not before a new attach");
    }
}

#[cfg(feature = "engine")]
#[test]
fn a_batch_cannot_hide_a_fatal_error_behind_an_earlier_stale_generation() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"ready");
    let error = plane
        .apply_engine_events(vec![
            EngineEvent::Output {
                terminal_id: terminal(),
                stream_id: StreamId::new(2).expect("stale stream"),
                bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                seq: 1,
                bytes: b"stale".to_vec().into(),
            },
            EngineEvent::Output {
                terminal_id: terminal(),
                stream_id: StreamId::new(1).expect("stream"),
                bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                seq: 2,
                bytes: b"gap".to_vec().into(),
            },
        ])
        .expect_err("the current generation has a fatal sequence gap");

    assert!(
        matches!(&error, ControlError::Protocol(message) if message.contains("sequence gap")),
        "expected the fatal sequence gap to outrank stale generation, got {error:?}"
    );
}
