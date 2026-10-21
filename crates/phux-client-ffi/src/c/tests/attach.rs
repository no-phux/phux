//! Attach and detach lifecycle, effect queues, and frame limits.

use super::*;

#[test]
fn queued_effects_are_projected_only_when_read() {
    let client = negotiated_client(&[]);
    client::EFFECT_VIEW_BUILDS.set(0);
    for index in 0..64 {
        assert_eq!(
            feed(
                client,
                &FrameKind::Error {
                    code: phux_protocol::wire::frame::ErrorCode::InvalidCommand,
                    request_id: None,
                    message: format!("error {index}"),
                }
            ),
            PhuxClientResult::Ok
        );
    }
    assert_eq!(unsafe { phux_client_effect_count(client) }, 64);
    assert_eq!(
        client::EFFECT_VIEW_BUILDS.get(),
        0,
        "feeding must not repeatedly project the growing effect backlog"
    );
    for index in 0..64 {
        let mut effect = PhuxClientEffect::default();
        assert_eq!(
            unsafe { phux_client_effect_get(client, index, &raw mut effect) },
            PhuxClientResult::Ok
        );
        assert_eq!((effect.kind, effect.detail), (2, 4));
        let message = unsafe { std::slice::from_raw_parts(effect.bytes.data, effect.bytes.len) };
        assert_eq!(message, format!("InvalidCommand: error {index}").as_bytes());
    }
    assert_eq!(client::EFFECT_VIEW_BUILDS.get(), 64);
    // Pending effects remain hidden until successful processing publishes them.
    let mut satellite = OwnedEffect::simple(2, 2, phux_protocol::ResourceId::satellite("peer", 9));
    satellite.bytes = b"satellite title".to_vec();
    satellite.stream_id = 11;
    satellite.bootstrap_id = 12;
    satellite.seq = 13;
    satellite.first_row = 3;
    satellite.last_row = 5;
    unsafe { (*client).inner.owned_effects.push(satellite) };
    let mut effect = PhuxClientEffect::default();
    assert_eq!(unsafe { phux_client_effect_count(client) }, 64);
    assert_eq!(
        unsafe { phux_client_effect_get(client, 64, &raw mut effect) },
        PhuxClientResult::NoValue
    );
    unsafe { (*client).inner.publish_effects() };
    assert_eq!(
        unsafe { phux_client_effect_get(client, 64, &raw mut effect) },
        PhuxClientResult::Ok
    );
    assert_eq!((effect.terminal_id.kind, effect.terminal_id.id), (1, 9));
    assert_eq!(
        unsafe {
            std::slice::from_raw_parts(effect.terminal_id.host.data, effect.terminal_id.host.len)
        },
        b"peer"
    );
    assert_eq!(
        unsafe { std::slice::from_raw_parts(effect.bytes.data, effect.bytes.len) },
        b"satellite title"
    );
    assert_eq!(
        (effect.stream_id, effect.bootstrap_id, effect.seq),
        (11, 12, 13)
    );
    assert_eq!((effect.first_row, effect.last_row), (3, 5));
    assert_eq!(
        unsafe { phux_client_effect_clear(client) },
        PhuxClientResult::Ok
    );
    assert_eq!(unsafe { phux_client_effect_count(client) }, 0);
    let mut effect = PhuxClientEffect::default();
    assert_eq!(
        unsafe { phux_client_effect_get(client, 0, &raw mut effect) },
        PhuxClientResult::NoValue
    );
    unsafe { phux_client_free(client) };
}

#[test]
fn attach_id_zero_is_rejected_without_output() {
    let client = negotiated_client(&[]);
    let options = PhuxAttachOptions {
        size: mem::size_of::<PhuxAttachOptions>(),
        version: ABI_VERSION,
        attach_id: 0,
        target_kind: 0,
        session_id: 0,
        name: PhuxBytes::default(),
        cols: 80,
        rows: 24,
        has_pixel_size: false,
        pixel_width: 0,
        pixel_height: 0,
        request_scrollback: true,
        scrollback_limit_lines: 1_000,
    };
    assert_eq!(
        unsafe { phux_client_queue_attach(client, &raw const options) },
        PhuxClientResult::InvalidArgument
    );
    let client_ref = unsafe { &*client };
    assert!(client_ref.inner.outgoing.is_empty());
    unsafe { phux_client_free(client) };
}

#[test]
fn last_attach_queues_one_server_resolved_request_without_create_fallback() {
    let client = negotiated_client(&[]);
    let options = PhuxAttachOptions {
        size: mem::size_of::<PhuxAttachOptions>(),
        version: ABI_VERSION,
        attach_id: 7,
        target_kind: 0,
        session_id: 0,
        name: PhuxBytes::default(),
        cols: 80,
        rows: 24,
        has_pixel_size: false,
        pixel_width: 0,
        pixel_height: 0,
        request_scrollback: true,
        scrollback_limit_lines: 1_000,
    };

    assert_eq!(
        unsafe { phux_client_queue_attach(client, &raw const options) },
        PhuxClientResult::Ok,
    );
    let client_ref = unsafe { &*client };
    assert_eq!(
        client_ref.inner.outgoing.len(),
        1,
        "the bridge must not queue a client-derived fallback attempt",
    );
    let (decoded, remaining) =
        FrameKind::decode(&client_ref.inner.outgoing[0]).expect("ATTACH decodes");
    assert!(remaining.is_empty());
    assert!(matches!(
        decoded,
        FrameKind::Attach {
            attach_id: 7,
            target: AttachTarget::Last,
            ..
        }
    ));

    unsafe { phux_client_free(client) };
}

#[test]
fn outbound_text_limit_rejects_overflow_and_accepts_boundary() {
    let too_large = vec![b'a'; crate::c::error::MAX_OUTBOUND_BYTES + 1];
    let rejected = new_client();
    assert_eq!(
        unsafe { phux_client_queue_hello(rejected, bytes_out(&too_large)) },
        PhuxClientResult::InvalidArgument
    );
    let rejected_client = unsafe { &*rejected };
    assert!(rejected_client.inner.outgoing.is_empty());
    unsafe { phux_client_free(rejected) };

    let boundary = vec![b'a'; crate::c::error::MAX_OUTBOUND_BYTES];
    let accepted = new_client();
    assert_eq!(
        unsafe { phux_client_queue_hello(accepted, bytes_out(&boundary)) },
        PhuxClientResult::Ok
    );
    let accepted_client = unsafe { &*accepted };
    let (decoded, remaining) =
        FrameKind::decode(&accepted_client.inner.outgoing[0]).expect("boundary HELLO decodes");
    assert!(remaining.is_empty());
    assert!(
        matches!(decoded, FrameKind::Hello { client_name, .. } if client_name.len() == boundary.len())
    );
    unsafe { phux_client_free(accepted) };
}

#[test]
fn native_arrangement_negotiates_metadata_before_issuing_layout_reads() {
    let client = new_client();
    assert_eq!(
        unsafe { phux_client_queue_hello(client, bytes_out(b"arrangement")) },
        PhuxClientResult::Ok
    );
    let state = unsafe { &*client };
    let (frame, remaining) = FrameKind::decode(&state.inner.outgoing[0]).expect("outbound HELLO");
    assert!(remaining.is_empty());
    unsafe { phux_client_free(client) };
    let FrameKind::Hello { client_caps, .. } = frame else {
        panic!("expected HELLO");
    };
    assert!(client_caps.layers.contains(phux_protocol::Layer::L3));
}

#[test]
fn feed_rejects_payload_above_current_limit_before_lifecycle_dispatch() {
    let client = new_client();
    let payload = vec![0_u8; 2 * 1024];
    assert_eq!(
        feed(
            client,
            &FrameKind::BootstrapChunk {
                terminal_id: phux_protocol::ResourceId::local(7),
                stream_id: phux_protocol::StreamId::new(1).expect("stream"),
                bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
                chunk_seq: 0,
                payload: payload.into(),
            },
        ),
        PhuxClientResult::ProtocolError
    );
    assert!(!unsafe {
        (*client)
            .inner
            .active_attach_contains(&phux_protocol::ResourceId::local(7))
    });
    unsafe { phux_client_free(client) };
}

#[test]
fn terminal_state_frames_require_an_active_attach_participant() {
    let terminal_id = phux_protocol::ResourceId::local(7);
    let client = attaching(&[], 7);
    let begin = FrameKind::BootstrapBegin {
        terminal_id: terminal_id.clone(),
        stream_id: phux_protocol::StreamId::new(1).expect("stream"),
        bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
        profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
        cols: 80,
        rows: 24,
        base_seq: 0,
    };
    assert_eq!(feed(client, &begin), PhuxClientResult::ProtocolError);
    assert_eq!(
        feed(
            client,
            &FrameKind::ResourceClosed {
                terminal_id: terminal_id.clone(),
                exit_status: None,
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
                signal: None,
            },
        ),
        PhuxClientResult::ProtocolError
    );
    assert!(!unsafe { (*client).inner.active_attach_contains(&terminal_id) });
    let snapshot = single_terminal_snapshot(terminal_id.clone(), 80, 24);
    assert_eq!(
        feed(client, &attached_frame(7, snapshot)),
        PhuxClientResult::Ok
    );
    assert!(unsafe { (*client).inner.active_attach_contains(&terminal_id) });
    assert_eq!(
        feed(
            client,
            &FrameKind::BootstrapBegin {
                terminal_id: phux_protocol::ResourceId::local(8),
                stream_id: phux_protocol::StreamId::new(2).expect("stream"),
                bootstrap_id: phux_protocol::BootstrapId::new(2).expect("bootstrap"),
                profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 80,
                rows: 24,
                base_seq: 0,
            },
        ),
        PhuxClientResult::ProtocolError
    );
    unsafe { phux_client_free(client) };
}

#[test]
fn detached_before_attach_cleanly_ends_prehello_and_negotiated_connections() {
    for client in [new_client(), negotiated_client(&[])] {
        assert_eq!(
            feed(
                client,
                &FrameKind::Detached {
                    reason: Some(DetachReason::ProtocolError),
                    message: "connection refused".to_owned()
                }
            ),
            PhuxClientResult::Ok
        );
        assert!(unsafe { (*client).inner.detached });
        assert_eq!(
            feed(client, &FrameKind::Ping { nonce: 7 }),
            PhuxClientResult::ProtocolError,
            "nothing follows DETACHED"
        );
        unsafe { phux_client_free(client) };
    }
}

#[test]
fn detached_releases_attach_and_rejects_subsequent_output_transactionally() {
    let terminal_id = phux_protocol::ResourceId::local(7);
    let client = attaching(&[], 7);
    let snapshot = single_terminal_snapshot(terminal_id.clone(), 80, 24);
    assert_eq!(
        feed(client, &attached_frame(7, snapshot)),
        PhuxClientResult::Ok
    );
    assert_eq!(
        feed(
            client,
            &FrameKind::Detached {
                reason: None,
                message: String::new()
            }
        ),
        PhuxClientResult::Ok
    );
    let client_ref = unsafe { &*client };
    assert!(!client_ref.inner.active_attach_contains(&terminal_id));
    let effects_before = client_ref.inner.owned_effects.len();
    assert_eq!(
        feed(
            client,
            &FrameKind::ResourceOutput {
                terminal_id,
                stream_id: phux_protocol::StreamId::new(1).expect("stream"),
                bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
                seq: 1,
                bytes: bytes::Bytes::from_static(b"late"),
            },
        ),
        PhuxClientResult::ProtocolError
    );
    let client_ref = unsafe { &*client };
    assert_eq!(client_ref.inner.owned_effects.len(), effects_before);
    unsafe { phux_client_free(client) };
}

/// The DETACHED status effect carries the reason as a stable wire value
/// and the message verbatim; an unstated reason is UNSTATED, never the
/// zero-default `REQUESTED`.
#[test]
fn detached_status_effect_carries_the_reason_and_message() {
    for (reason, expected_code) in [
        (None, DETACH_REASON_UNSTATED),
        (Some(DetachReason::Requested), 0),
        (Some(DetachReason::ServerShutdown), 1),
        (Some(DetachReason::AuthenticationFailed), 5),
        (Some(DetachReason::AuthorizationRevoked), 6),
        (Some(DetachReason::AuthorizationExpired), 7),
        (Some(DetachReason::InternalError), 255),
    ] {
        let client = attaching(&[], 7);
        assert_eq!(
            feed(
                client,
                &FrameKind::Detached {
                    reason,
                    message: "server is stopping".to_owned(),
                }
            ),
            PhuxClientResult::Ok
        );
        let client_ref = unsafe { &*client };
        let effect = client_ref
            .inner
            .owned_effects
            .last()
            .expect("DETACHED pushes a status effect");
        assert_eq!(effect.kind, 2);
        assert_eq!(effect.detail, 5);
        assert_eq!(effect.status_code, expected_code, "reason {reason:?}");
        assert_eq!(effect.bytes, b"server is stopping");
        unsafe { phux_client_free(client) };
    }
}
