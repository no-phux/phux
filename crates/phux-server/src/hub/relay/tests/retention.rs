//! Retention budgets, saturated subscribers, and retirement.

use super::*;

#[test]
fn generation_byte_budget_fails_without_allocating_chunk_payloads() {
    let mut session = RelaySession::new_negotiated(
        host(),
        BootstrapLimits::default(),
        native_profile(),
        ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
    );
    let stream_id = StreamId::new(1).expect("stream");
    let bootstrap_id = BootstrapId::new(1).expect("bootstrap");
    session
        .begin_bootstrap_flow(7, stream_id, bootstrap_id, native_stream_profile())
        .expect("begin");
    for chunk_seq in 0..64 {
        session
            .accept_bootstrap_chunk(7, stream_id, bootstrap_id, chunk_seq, 256 * 1024)
            .expect("within 16 MiB generation budget");
    }
    let error = session
        .accept_bootstrap_chunk(7, stream_id, bootstrap_id, 64, 256 * 1024)
        .expect_err("generation above 16 MiB must fail");
    assert!(error.contains("per-generation bootstrap byte budget"));
}

#[test]
fn saturated_subscribers_converge_or_receive_an_explicit_failure() {
    let mut open = RelaySession::new(host(), BootstrapLimits::default());
    let (open_tx, mut open_rx) = mpsc::channel(1);
    subscribe(&mut open, 9, ClientId(1), open_tx.clone());
    open_tx
        .try_send(Outbound::Frame(FrameKind::Detach))
        .expect("fill open mailbox");
    open.fan_out(
        9,
        &FrameKind::Event {
            terminal: Some(ResourceId::satellite("devbox", 9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        },
    );
    let _ = open_rx.try_recv().expect("filler remains");
    open.flush_pending_snapshots();
    assert!(open.subscribers.contains_key(&9));
    assert!(matches!(
        open_rx
            .try_recv()
            .expect("retained event resumes after drain"),
        Outbound::Frame(FrameKind::Event { .. })
    ));

    let mut closed = RelaySession::new(host(), BootstrapLimits::default());
    let (closed_tx, closed_rx) = mpsc::channel(1);
    subscribe(&mut closed, 9, ClientId(1), closed_tx);
    drop(closed_rx);
    closed.fan_out(
        9,
        &FrameKind::Event {
            terminal: Some(ResourceId::satellite("devbox", 9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        },
    );
    assert!(!closed.subscribers.contains_key(&9));

    let mut retained = RelaySession::new(host(), BootstrapLimits::default());
    let (retained_tx, mut retained_rx) = mpsc::channel(1);
    subscribe(&mut retained, 9, ClientId(1), retained_tx.clone());
    retained_tx
        .try_send(Outbound::Frame(FrameKind::Detach))
        .expect("fill retained mailbox");
    for chunk_seq in 0..=MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES {
        retained.fan_out(
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
    assert!(retained.subscribers.contains_key(&9));
    assert_eq!(retained.retained_bytes, 0);
    assert_eq!(retained.retained_frames, 0);
    let _ = retained_rx.try_recv().expect("filler remains");
    retained.flush_pending_snapshots();
    assert!(!retained.subscribers.contains_key(&9));
    let Outbound::Frame(FrameKind::BootstrapTombstone {
        terminal_id,
        reason,
        ..
    }) = retained_rx.try_recv().expect("explicit saturation failure")
    else {
        panic!("content saturation must deliver a generation tombstone")
    };
    assert_eq!(terminal_id, ResourceId::satellite("devbox", 9));
    assert_eq!(reason, TombstoneReason::OutboundGap);
    assert_eq!(retained.take_delivery_detaches().len(), 1);
}

#[test]
fn connection_wide_retention_budget_marks_excess_subscriber_for_failure() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let mut receivers = Vec::new();
    for client in 1..=9 {
        let (tx, rx) = mpsc::channel(1);
        subscribe(&mut session, 9, ClientId(client), tx.clone());
        tx.try_send(Outbound::Frame(FrameKind::Detach))
            .expect("fill consumer mailbox");
        receivers.push(rx);
    }
    let payload = bytes::Bytes::from(vec![0; 1024 * 1024 - RETAINED_FRAME_OVERHEAD]);
    session.fan_out(
        9,
        &FrameKind::BootstrapChunk {
            terminal_id: ResourceId::satellite("devbox", 9),
            stream_id: StreamId::new(1).expect("stream"),
            bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
            chunk_seq: 0,
            payload,
        },
    );
    assert_eq!(session.subscribers[&9].len(), 9);
    assert!(session.retained_bytes <= MAX_RELAY_CONNECTION_RETAINED_BYTES);
    assert_eq!(session.retained_frames, 7);
    assert!(matches!(
        session.subscribers[&9][7].gate,
        SnapshotGate::Retiring { .. }
    ));
    assert_eq!(receivers.len(), 9);
}

#[test]
fn live_and_history_payloads_hit_the_byte_limit_before_the_frame_limit() {
    let stream_id = StreamId::new(1).expect("stream");
    let bootstrap_id = BootstrapId::new(1).expect("bootstrap");

    let mut live = RelaySession::new(host(), BootstrapLimits::default());
    let (live_tx, _live_rx) = mpsc::channel(1);
    live_tx
        .try_send(Outbound::Frame(FrameKind::Detach))
        .expect("fill live mailbox");
    subscribe(&mut live, 9, ClientId(1), live_tx);
    live.fan_out(
        9,
        &FrameKind::ResourceOutput {
            terminal_id: ResourceId::satellite("devbox", 9),
            stream_id,
            bootstrap_id,
            seq: 8,
            bytes: bytes::Bytes::from(vec![0; MAX_RELAY_SUBSCRIBER_RETAINED_BYTES]),
        },
    );
    let SnapshotGate::Retiring { failure, .. } = &live.subscribers[&9][0].gate else {
        panic!("one legal large live frame must trip the byte bound before the frame bound")
    };
    assert!(matches!(
        failure,
        FrameKind::BootstrapTombstone {
            terminal_id,
            stream_id: failed_stream,
            bootstrap_id: failed_bootstrap,
            last_valid_seq: 7,
            ..
        } if terminal_id == &ResourceId::satellite("devbox", 9)
            && *failed_stream == stream_id
            && *failed_bootstrap == bootstrap_id
    ));

    let mut history = RelaySession::new(host(), BootstrapLimits::default());
    let (history_tx, mut history_rx) = mpsc::channel(1);
    history_tx
        .try_send(Outbound::Frame(FrameKind::Detach))
        .expect("fill history mailbox");
    subscribe(&mut history, 9, ClientId(1), history_tx);
    let cursor = bytes::Bytes::from_static(b"cursor-9");
    history.fan_out(
        9,
        &FrameKind::HistoryPage {
            terminal_id: ResourceId::satellite("devbox", 9),
            stream_id,
            bootstrap_id,
            page_seq: 1,
            cursor: cursor.clone(),
            next_cursor: None,
            payload: bytes::Bytes::from(vec![0; MAX_RELAY_SUBSCRIBER_RETAINED_BYTES]),
            rows: 1,
        },
    );
    let SnapshotGate::Retiring { failure, .. } = &history.subscribers[&9][0].gate else {
        panic!("one legal large history page must trip the byte bound before the frame bound")
    };
    assert!(matches!(
        failure,
        FrameKind::HistoryTombstone {
            terminal_id,
            stream_id: failed_stream,
            bootstrap_id: failed_bootstrap,
            cursor: failed_cursor,
            reason: HistoryTombstoneReason::Limit,
        } if terminal_id == &ResourceId::satellite("devbox", 9)
            && *failed_stream == stream_id
            && *failed_bootstrap == bootstrap_id
            && failed_cursor == &cursor
    ));
    let _ = history_rx.try_recv().expect("filler");
    history.flush_pending_snapshots();
    assert!(matches!(
        history_rx.try_recv().expect("history fence"),
        Outbound::Frame(FrameKind::HistoryTombstone { .. })
    ));
    assert!(!history.subscribers.contains_key(&9));
    assert_eq!(history.take_delivery_detaches().len(), 1);
}

#[test]
fn permanently_full_retirement_cancels_consumer_and_detaches_upstream() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(1);
    let production_owner = out_tx.clone();
    let consumer_cancel = CancellationToken::new();
    out_tx
        .try_send(Outbound::Frame(FrameKind::Detach))
        .expect("fill downstream mailbox");
    let wire = session.handle_request(RelayRequest::Subscribe {
        subscription: ProxySubscription {
            resource_kind: None,
            terminal: 9,
            client: ClientId(1),
            out_tx,
            consumer_cancel: consumer_cancel.clone(),
            seq: 1,
            awaits_snapshot: false,
            bootstrap_profile: None,
            bootstrap_limits: None,
        },
        forward: FrameKind::SubscribeEvents {
            terminal: Some(ResourceId::local(9)),
            after_seq: None,
        },
    });
    assert!(!wire.is_empty());
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
    let SnapshotGate::Retiring { deadline, .. } =
        &mut session.subscribers.get_mut(&9).expect("subscriber")[0].gate
    else {
        panic!("overflow starts bounded retirement")
    };
    *deadline = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_millis(1))
        .expect("one millisecond is representable");

    session.flush_pending_snapshots();
    assert!(!session.subscribers.contains_key(&9));
    assert_eq!(session.take_delivery_detaches().len(), 1);
    assert!(consumer_cancel.is_cancelled());
    let _ = out_rx.try_recv().expect("queued filler");
    assert!(matches!(
        out_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    drop(production_owner);
    assert!(matches!(
        out_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}
