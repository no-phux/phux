//! Retagged return-leg streams and terminal closes.

use super::*;

#[test]
fn session_retags_subscribed_streams_local_to_satellite() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);
    subscribe(&mut session, 9, ClientId(1), out_tx);

    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        }))
        .expect("valid satellite frame");
    session
        .handle_inbound(&encode(&FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(9),
            stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
            seq: 42,
            bytes: bytes::Bytes::from_static(b"hi"),
        }))
        .expect("valid satellite frame");
    // A different terminal: nothing must reach the subscriber.
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(10)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        }))
        .expect("valid satellite frame");

    let Outbound::Frame(first) = out_rx.try_recv().expect("event fanned out") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert_eq!(
        first,
        FrameKind::Event {
            terminal: Some(ResourceId::satellite("devbox", 9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        }
    );
    let Outbound::Frame(second) = out_rx.try_recv().expect("output fanned out") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert!(matches!(
        second,
        FrameKind::ResourceOutput { terminal_id, seq: 42, .. }
            if terminal_id == ResourceId::satellite("devbox", 9)
    ));
    assert!(out_rx.try_recv().is_err(), "unsubscribed terminal leaked");
}

#[test]
fn session_drops_chained_satellite_tags_from_the_return_leg() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);
    subscribe(&mut session, 9, ClientId(1), out_tx);
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::satellite("nested", 9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        }))
        .expect("valid satellite frame");
    assert!(out_rx.try_recv().is_err());
}

#[test]
fn terminal_closed_retags_and_drops_the_proxy_subscription() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);
    subscribe(&mut session, 9, ClientId(1), out_tx);
    session
        .handle_inbound(&encode(&FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(9),
            exit_status: Some(0),
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        }))
        .expect("valid satellite frame");
    let Outbound::Frame(frame) = out_rx.try_recv().expect("closed fanned out") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert_eq!(
        frame,
        FrameKind::ResourceClosed {
            terminal_id: ResourceId::satellite("devbox", 9),
            exit_status: Some(0),
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        }
    );
    // Subscription is gone: further frames for id 9 do not fan out.
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        }))
        .expect("valid satellite frame");
    assert!(out_rx.try_recv().is_err());
}

#[test]
fn terminal_close_follows_retained_output_before_subscription_removal() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(1);
    subscribe(&mut session, 9, ClientId(1), out_tx.clone());
    out_tx
        .try_send(Outbound::Frame(FrameKind::Detach))
        .expect("fill downstream mailbox");
    session.fan_out(9, &output_frame(9, 2, b"before-close"));
    session
        .handle_inbound(&encode(&FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(9),
            exit_status: Some(0),
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        }))
        .expect("valid close");
    assert!(session.subscribers.contains_key(&9));

    let _ = out_rx.try_recv().expect("filler");
    session.flush_pending_snapshots();
    assert!(matches!(
        out_rx.try_recv().expect("retained output first"),
        Outbound::Frame(FrameKind::ResourceOutput { .. })
    ));
    session.flush_pending_snapshots();
    assert!(matches!(
        out_rx.try_recv().expect("close follows output"),
        Outbound::Frame(FrameKind::ResourceClosed { .. })
    ));
    assert!(!session.subscribers.contains_key(&9));
    assert!(
        session.take_delivery_detaches().is_empty(),
        "a resource already closed upstream needs no detach after downstream delivery"
    );
}

#[test]
fn idempotent_reattach_cannot_cancel_a_queued_terminal_close() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (old_tx, _old_rx) = mpsc::channel(1);
    old_tx
        .try_send(Outbound::Frame(FrameKind::Detach))
        .expect("fill old mailbox");
    subscribe(&mut session, 9, ClientId(1), old_tx);
    session.fan_out(9, &output_frame(9, 2, b"before-close"));
    session
        .handle_inbound(&encode(&FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(9),
            exit_status: Some(0),
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        }))
        .expect("valid close");

    let (new_tx, mut new_rx) = mpsc::channel(2);
    let registration = session.register_subscriber(ProxySubscription {
        resource_kind: None,
        terminal: 9,
        client: ClientId(1),
        out_tx: new_tx,
        consumer_cancel: CancellationToken::new(),
        seq: 0,
        awaits_snapshot: true,
        bootstrap_profile: None,
        bootstrap_limits: None,
    });
    assert!(matches!(registration, Registration::Idempotent));
    assert!(session.subscribers[&9][0].retire_after_flush);

    session.flush_pending_snapshots();
    assert!(matches!(
        new_rx.try_recv().expect("retained output"),
        Outbound::Frame(FrameKind::ResourceOutput { .. })
    ));
    assert!(matches!(
        new_rx.try_recv().expect("queued close"),
        Outbound::Frame(FrameKind::ResourceClosed { .. })
    ));
    assert!(!session.subscribers.contains_key(&9));
    assert!(session.take_delivery_detaches().is_empty());
}

#[test]
fn close_overflow_fences_the_terminal_generation_without_detaching_closed_upstream() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, _out_rx) = mpsc::channel(1);
    subscribe(&mut session, 9, ClientId(1), out_tx);
    let stream_id = StreamId::new(4).expect("stream");
    let bootstrap_id = BootstrapId::new(5).expect("bootstrap");
    let sub = &mut session.subscribers.get_mut(&9).expect("subscriber")[0];
    sub.current_generation = Some((stream_id, bootstrap_id, 17));
    sub.gate = SnapshotGate::Retained {
        frames: VecDeque::from([output_frame(9, 17, b"retained")]),
        retained_bytes: MAX_RELAY_SUBSCRIBER_RETAINED_BYTES,
        open_after: true,
    };
    session.retained_bytes = MAX_RELAY_SUBSCRIBER_RETAINED_BYTES;
    session.retained_frames = 1;

    session
        .handle_inbound(&encode(&FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(9),
            exit_status: Some(0),
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        }))
        .expect("valid close");
    let SnapshotGate::Retiring {
        failure,
        detach_upstream,
        ..
    } = &session.subscribers[&9][0].gate
    else {
        panic!("close overflow retires with an explicit generation fence")
    };
    assert!(!detach_upstream);
    assert!(matches!(
        failure,
        FrameKind::BootstrapTombstone {
            terminal_id,
            stream_id: failed_stream,
            bootstrap_id: failed_bootstrap,
            last_valid_seq: 17,
            reason: TombstoneReason::OutboundGap,
        } if terminal_id == &ResourceId::satellite("devbox", 9)
            && *failed_stream == stream_id
            && *failed_bootstrap == bootstrap_id
    ));
    assert!(session.take_delivery_detaches().is_empty());
}

#[test]
fn terminal_close_supersedes_an_existing_delivery_gap_retirement() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(1);
    out_tx
        .try_send(Outbound::Frame(FrameKind::Detach))
        .expect("fill downstream mailbox");
    subscribe(&mut session, 9, ClientId(1), out_tx);
    for chunk_seq in 0..=MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES {
        session.fan_out(
            9,
            &FrameKind::BootstrapChunk {
                terminal_id: ResourceId::satellite("devbox", 9),
                stream_id: StreamId::new(1).expect("stream"),
                bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
                chunk_seq: u32::try_from(chunk_seq).expect("small sequence"),
                payload: bytes::Bytes::from_static(b"x"),
            },
        );
    }
    assert!(matches!(
        session.subscribers[&9][0].gate,
        SnapshotGate::Retiring {
            failure: FrameKind::BootstrapTombstone { .. },
            detach_upstream: true,
            ..
        }
    ));

    session
        .handle_inbound(&encode(&FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(9),
            exit_status: Some(23),
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        }))
        .expect("valid close");
    assert!(matches!(
        session.subscribers[&9][0].gate,
        SnapshotGate::Retiring {
            failure: FrameKind::ResourceClosed {
                exit_status: Some(23),
                ..
            },
            detach_upstream: false,
            ..
        }
    ));

    let _ = out_rx.try_recv().expect("filler");
    session.flush_pending_snapshots();
    assert!(matches!(
        out_rx.try_recv().expect("authoritative close"),
        Outbound::Frame(FrameKind::ResourceClosed {
            exit_status: Some(23),
            ..
        })
    ));
    assert!(!session.subscribers.contains_key(&9));
    assert!(session.take_delivery_detaches().is_empty());
}

// --- attach snapshot ordering under backpressure (phux-v45.12) --------
