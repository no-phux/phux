//! Teardown, unsubscribes, withdrawal receipts, and detach barriers.

use super::*;

#[test]
fn teardown_fails_pending_and_notifies_each_consumer_once() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (reply, mut reply_rx) = oneshot::channel();
    let _ = session.handle_request(RelayRequest::Command {
        command: Command::Upgrade,
        reply,
        subscribe: None,
    });
    let (out_tx, mut out_rx) = mpsc::channel(8);
    // Two subscriptions for the same client: one notification.
    subscribe(&mut session, 1, ClientId(7), out_tx.clone());
    subscribe(&mut session, 2, ClientId(7), out_tx);

    session.teardown("satellite went away");

    assert!(matches!(
        reply_rx.try_recv().expect("pending failed"),
        CommandResult::Error {
            code: ErrorCode::SatelliteUnreachable,
            ..
        }
    ));
    let Outbound::Frame(frame) = out_rx.try_recv().expect("consumer notified") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert!(matches!(
        frame,
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::SatelliteUnreachable,
            ..
        }
    ));
    assert!(
        out_rx.try_recv().is_err(),
        "one typed notification per consumer, not per subscription"
    );
}

#[test]
fn unsubscribe_client_stops_fan_out() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);
    subscribe(&mut session, 9, ClientId(1), out_tx);
    let frames = session.handle_unsubscribe(Unsubscribe::Client(ClientId(1)));
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        }))
        .expect("valid satellite frame");
    assert!(out_rx.try_recv().is_err());
    // The last proxy left: detach upstream.
    assert_eq!(frames.len(), 1, "one satellite-side detach expected");
    let FrameKind::Command { command, .. } = decode(&frames[0]) else {
        panic!("expected COMMAND on the wire");
    };
    assert_eq!(
        command,
        Command::DetachResource {
            terminal_id: ResourceId::local(9),
        }
    );
}

// --- lifecycle hardening (phux-v45.11) ---------------------------------

#[test]
fn unsubscribe_keeps_satellite_streaming_while_other_subscribers_remain() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (tx_a, _rx_a) = mpsc::channel(8);
    let (tx_b, mut rx_b) = mpsc::channel(8);
    subscribe(&mut session, 9, ClientId(1), tx_a);
    subscribe(&mut session, 9, ClientId(2), tx_b);
    // Client 2 still observes it: no upstream detach.
    let frames = session.handle_unsubscribe(Unsubscribe::Terminal {
        client: ClientId(1),
        terminal: 9,
        seq: 2,
        reply: None,
    });
    assert!(
        frames.is_empty(),
        "satellite-side detach must wait for the last subscriber"
    );
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        }))
        .expect("valid satellite frame");
    let Outbound::Frame(_) = rx_b.try_recv().expect("client 2 still fanned out") else {
        panic!("unexpected terminal outbound sentinel");
    };
    // Now the last subscriber leaves: exactly one detach goes out.
    let frames = session.handle_unsubscribe(Unsubscribe::Terminal {
        client: ClientId(2),
        terminal: 9,
        seq: 2,
        reply: None,
    });
    assert_eq!(frames.len(), 1);
}

#[test]
fn stale_terminal_unsubscribe_does_not_tear_down_a_fresher_reattach() {
    // Reorder guard: a stale detach (token 2) applied after a re-attach
    // (token 3) that raced ahead is dropped.
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);
    subscribe_at(&mut session, 9, ClientId(1), 1, out_tx.clone());
    subscribe_at(&mut session, 9, ClientId(1), 3, out_tx);

    let frames = session.handle_unsubscribe(Unsubscribe::Terminal {
        client: ClientId(1),
        terminal: 9,
        seq: 2,
        reply: None,
    });
    assert!(
        frames.is_empty(),
        "a stale detach must not emit a satellite-side DETACH for a re-attached terminal"
    );

    // The re-attached stream is intact.
    session
        .handle_inbound(&encode(&FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(9),
            stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
            seq: 7,
            bytes: bytes::Bytes::from_static(b"live"),
        }))
        .expect("valid satellite frame");
    let Outbound::Frame(frame) = out_rx.try_recv().expect("re-attached stream torn down") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert!(matches!(
        frame,
        FrameKind::ResourceOutput { terminal_id, seq: 7, .. }
            if terminal_id == ResourceId::satellite("devbox", 9)
    ));

    // A newer detach still withdraws.
    let frames = session.handle_unsubscribe(Unsubscribe::Terminal {
        client: ClientId(1),
        terminal: 9,
        seq: 4,
        reply: None,
    });
    assert_eq!(frames.len(), 1, "an in-order detach still tears down");
}

#[tokio::test]
async fn unsubscribe_survives_a_full_relay_mailbox() {
    // Unsubscribes ride an unbounded channel.
    let (handle, mut mailbox) = RelayHandle::new(host());
    for _ in 0..RELAY_MAILBOX {
        handle.forward(FrameKind::Detach);
    }
    handle.unsubscribe_client(ClientId(1));
    let detach = handle.unsubscribe_resource(ClientId(2), 7);
    tokio::pin!(detach);
    assert!(futures_util::poll!(&mut detach).is_pending());
    assert!(matches!(
        mailbox.unsubscribes.try_recv().expect("delivered"),
        Unsubscribe::Client(ClientId(1))
    ));
    assert!(matches!(
        mailbox.unsubscribes.try_recv().expect("delivered"),
        Unsubscribe::Terminal {
            client: ClientId(2),
            terminal: 7,
            ..
        }
    ));
    assert_eq!(detach.await, CommandResult::Ok);
}

#[tokio::test]
async fn withdrawal_receipt_fences_retained_replay_and_live_fanout() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(1);
    subscribe(&mut session, 9, ClientId(1), out_tx.clone());
    out_tx.try_send(Outbound::Frame(FrameKind::Detach)).unwrap();
    session.handle_inbound(&encode(&snapshot_frame(9))).unwrap();
    assert!(session.retained_bytes > 0);
    let (reply, received) = oneshot::channel();
    let frames = session.handle_unsubscribe(Unsubscribe::Terminal {
        client: ClientId(1),
        terminal: 9,
        seq: 2,
        reply: Some(WithdrawalReceipt::new(reply)),
    });
    received.await.unwrap();
    assert_eq!(
        frames.len(),
        1,
        "last observer releases the upstream subscription"
    );
    assert_eq!(session.retained_bytes, 0);
    assert!(!session.subscribers.contains_key(&9));
    out_rx.try_recv().unwrap();
    session.flush_pending_snapshots();
    session
        .handle_inbound(&encode(&output_frame(9, 1, b"after detach")))
        .unwrap();
    assert!(
        out_rx.try_recv().is_err(),
        "no retained or new terminal frame after receipt"
    );
}

#[tokio::test(start_paused = true)]
async fn withdrawal_timeout_cancels_unapplied_teardown() {
    let (handle, mut mailbox) = RelayHandle::new(host());
    let detach = handle.unsubscribe_resource(ClientId(1), 9);
    tokio::pin!(detach);
    assert!(futures_util::poll!(&mut detach).is_pending());
    tokio::time::advance(RELAY_COMMAND_TIMEOUT).await;
    assert!(matches!(
        detach.await,
        CommandResult::Error {
            code: ErrorCode::SatelliteUnreachable,
            ..
        }
    ));
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, _out_rx) = mpsc::channel(1);
    subscribe_at(&mut session, 9, ClientId(1), 0, out_tx);
    let frames = session.handle_unsubscribe(mailbox.unsubscribes.try_recv().unwrap());
    assert!(
        frames.is_empty(),
        "safe refusal must cancel queued teardown"
    );
    assert!(session.subscribers.contains_key(&9));
}

#[tokio::test(start_paused = true)]
async fn applied_withdrawal_wins_even_when_waiter_resumes_past_deadline() {
    let (handle, mut mailbox) = RelayHandle::new(host());
    let detach = handle.unsubscribe_resource(ClientId(1), 9);
    tokio::pin!(detach);
    assert!(futures_util::poll!(&mut detach).is_pending());
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, _out_rx) = mpsc::channel(8);
    subscribe_at(&mut session, 9, ClientId(1), 0, out_tx);
    assert_eq!(
        session
            .handle_unsubscribe(mailbox.unsubscribes.try_recv().unwrap())
            .len(),
        1
    );
    tokio::time::advance(RELAY_COMMAND_TIMEOUT).await;
    assert_eq!(
        detach.await,
        CommandResult::Ok,
        "a committed removal is never a safe refusal"
    );
    assert!(!session.subscribers.contains_key(&9));
}

#[tokio::test]
async fn abandoned_withdrawal_still_cleans_up_without_a_timeout_refusal() {
    let (handle, mut mailbox) = RelayHandle::new(host());
    let mut detach = Box::pin(handle.unsubscribe_resource(ClientId(1), 9));
    assert!(futures_util::poll!(&mut detach).is_pending());
    drop(detach);
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, _out_rx) = mpsc::channel(8);
    subscribe_at(&mut session, 9, ClientId(1), 0, out_tx);
    assert_eq!(
        session
            .handle_unsubscribe(mailbox.unsubscribes.try_recv().unwrap())
            .len(),
        1
    );
    assert!(
        !session.subscribers.contains_key(&9),
        "disconnected callers still get undroppable cleanup"
    );
}

#[tokio::test]
async fn withdrawal_without_a_live_session_is_idempotent() {
    let (handle, mut mailbox) = RelayHandle::new(host());
    let detach = handle.unsubscribe_resource(ClientId(1), 9);
    tokio::pin!(detach);
    assert!(futures_util::poll!(&mut detach).is_pending());
    // No session: dropping the receipt certifies no proxy can emit.
    drop(mailbox.unsubscribes.try_recv().unwrap());
    assert_eq!(detach.await, CommandResult::Ok);
    drop(mailbox);
    assert_eq!(
        handle.unsubscribe_resource(ClientId(1), 9).await,
        CommandResult::Ok
    );
}

#[tokio::test]
async fn stale_withdrawal_is_refused_while_the_newer_proxy_stays_live() {
    let (handle, mut mailbox) = RelayHandle::new(host());
    let detach = handle.unsubscribe_resource(ClientId(1), 9);
    tokio::pin!(detach);
    assert!(futures_util::poll!(&mut detach).is_pending());
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);
    // Model the separate request mailbox overtaking the queued withdrawal.
    subscribe_at(&mut session, 9, ClientId(1), u64::MAX, out_tx);
    assert!(
        session
            .handle_unsubscribe(mailbox.unsubscribes.try_recv().unwrap())
            .is_empty()
    );
    assert!(matches!(
        detach.await,
        CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            ..
        }
    ));
    session
        .handle_inbound(&encode(&output_frame(9, 1, b"newer attachment")))
        .unwrap();
    assert!(matches!(
        out_rx.try_recv(),
        Ok(Outbound::Frame(FrameKind::ResourceOutput { .. }))
    ));
    assert!(
        session.pending_detaches.is_empty(),
        "newer upstream stream is untouched"
    );
}

#[test]
fn upstream_detach_barrier_discards_old_frames_before_a_fresh_attach() {
    let mut session = RelaySession::new_negotiated(
        host(),
        BootstrapLimits::default(),
        BootstrapProfile::SynthesizedVtRaw,
        ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
    );
    session.handle_inbound(&encode(&begin_frame(9, 1))).unwrap();
    session.handle_inbound(&encode(&snapshot_frame(9))).unwrap();
    let (events_tx, mut events_rx) = mpsc::channel(8);
    subscribe(&mut session, 9, ClientId(2), events_tx);
    let (out_tx, mut out_rx) = mpsc::channel(8);
    let (reply, _received) = oneshot::channel();
    let request = RelayRequest::Command {
        command: Command::attach_terminal(ResourceId::local(9), None),
        reply,
        subscribe: Some(ProxySubscription {
            resource_kind: None,
            terminal: 9,
            client: ClientId(1),
            out_tx,
            consumer_cancel: CancellationToken::new(),
            seq: 1,
            awaits_snapshot: true,
            bootstrap_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: Some(BootstrapLimits::default()),
        }),
    };
    let prepared = session.prepare_request(&request);
    let [barrier, restore_events] = prepared.as_slice() else {
        panic!("detach and event restoration before attach");
    };
    assert!(matches!(
        decode(restore_events),
        FrameKind::SubscribeEvents {
            terminal: Some(ResourceId::Local { id: 9 }),
            ..
        }
    ));
    let FrameKind::Command {
        request_id,
        command: Command::DetachResource { terminal_id },
    } = decode(barrier)
    else {
        panic!("detach barrier");
    };
    assert_eq!(terminal_id, ResourceId::local(9));
    assert!(
        session.prepare_request(&request).is_empty(),
        "reuse an in-flight barrier"
    );
    session.handle_request_checked(request).unwrap();
    let event = FrameKind::Event {
        terminal: Some(ResourceId::local(9)),
        event: phux_protocol::wire::frame::AgentEvent::CommandStarted,
        stamp: None,
    };
    session.handle_inbound(&encode(&event)).unwrap();
    assert!(
        matches!(
            events_rx.try_recv(),
            Ok(Outbound::Frame(FrameKind::Event { .. }))
        ),
        "an event observer does not suppress the first content barrier and stays live during it"
    );
    assert!(
        out_rx.try_recv().is_err(),
        "new content proxy still waits for its own prefix"
    );
    // A prefix queued before withdrawal opens no new gate.
    session.handle_inbound(&encode(&begin_frame(9, 2))).unwrap();
    session.handle_inbound(&encode(&snapshot_frame(9))).unwrap();
    session
        .handle_inbound(&encode(&output_frame(9, 1, b"old")))
        .unwrap();
    assert!(out_rx.try_recv().is_err());
    session
        .handle_inbound(&encode(&FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        }))
        .unwrap();
    assert!(session.pending_detaches.is_empty());
    assert!(!session.bootstrap_flows.contains_key(&9));
    session.handle_inbound(&encode(&begin_frame(9, 1))).unwrap();
    session.handle_inbound(&encode(&snapshot_frame(9))).unwrap();
    session
        .handle_inbound(&encode(&output_frame(9, 1, b"fresh")))
        .unwrap();
    assert_eq!(
        out_rx.len(),
        3,
        "only the fresh BEGIN, READY and output reach the proxy"
    );
}

#[test]
fn detach_without_a_proxy_retires_automatic_spawn_output() {
    let mut session = RelaySession::new_negotiated(
        host(),
        BootstrapLimits::default(),
        BootstrapProfile::SynthesizedVtRaw,
        ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
    );
    session.handle_inbound(&encode(&begin_frame(9, 1))).unwrap();
    session.handle_inbound(&encode(&snapshot_frame(9))).unwrap();
    let frames = session.handle_unsubscribe(Unsubscribe::Terminal {
        client: ClientId(1),
        terminal: 9,
        seq: 1,
        reply: None,
    });
    assert_eq!(
        frames.len(),
        1,
        "unobserved spawn producer still needs upstream detach"
    );
    let FrameKind::Command { request_id, .. } = decode(&frames[0]) else {
        panic!("detach command");
    };
    session
        .handle_inbound(&encode(&output_frame(9, 1, b"queued before detach")))
        .unwrap();
    session
        .handle_inbound(&encode(&FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        }))
        .unwrap();
    assert!(session.pending_detaches.is_empty());
    assert!(session.bootstrap_flows.is_empty());
}

#[test]
fn upstream_detach_barrier_refusal_fails_the_link_closed() {
    for result in [
        CommandResult::Error {
            code: ErrorCode::SatelliteUnreachable,
            message: "refused".to_owned(),
        },
        CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::Json(
            "{}".to_owned(),
        )),
    ] {
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let frame = session.encode_terminal_detach(9);
        let FrameKind::Command { request_id, .. } = decode(&frame) else {
            panic!("detach command");
        };
        assert!(
            session
                .handle_inbound(&encode(&FrameKind::CommandResult { request_id, result }))
                .is_err()
        );
    }
}

#[tokio::test(start_paused = true)]
async fn upstream_detach_barriers_expire_and_reserve_their_request_ids() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let frame = session.encode_terminal_detach(9);
    let FrameKind::Command { request_id, .. } = decode(&frame) else {
        panic!("detach command");
    };
    session.next_request_id = request_id;
    assert_ne!(
        session.allocate_request_id(),
        request_id,
        "in-flight detach owns its correlation"
    );
    session.check_detach_deadlines().unwrap();
    tokio::time::advance(RELAY_COMMAND_TIMEOUT).await;
    assert!(session.check_detach_deadlines().is_err());
    session.teardown("expired upstream detach");
    assert!(session.pending_detaches.is_empty());
}

#[tokio::test]
async fn subscribe_on_a_full_mailbox_registers_nothing_and_notifies_the_consumer() {
    // Register + forward is atomic: on a full mailbox nothing registers
    // and the consumer gets an error push.
    let (handle, _mailbox) = RelayHandle::new(host());
    for _ in 0..RELAY_MAILBOX {
        handle.forward(FrameKind::Detach);
    }
    let (out_tx, mut out_rx) = mpsc::channel(8);
    handle.subscribe(
        ProxySubscription {
            resource_kind: None,
            terminal: 9,
            client: ClientId(1),
            out_tx,
            consumer_cancel: CancellationToken::new(),
            // Stamped by `handle.subscribe` at enqueue.
            seq: 0,
            awaits_snapshot: false,
            bootstrap_profile: None,
            bootstrap_limits: None,
        },
        FrameKind::SubscribeEvents {
            terminal: Some(ResourceId::local(9)),
            after_seq: None,
        },
    );
    let Outbound::Frame(frame) = out_rx.try_recv().expect("typed error pushed") else {
        panic!("unexpected terminal outbound sentinel")
    };
    assert!(matches!(
        frame,
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::ResourceExhausted,
            ..
        }
    ));
}
