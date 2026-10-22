//! Relayed command errors, timeouts, and full mailboxes.

use super::*;

#[test]
fn satellite_error_rolls_back_the_commands_subscription() {
    // A refused subscribing command leaves no registration.
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);
    let (reply, mut reply_rx) = oneshot::channel();
    let wire = session.handle_request(RelayRequest::Command {
        command: Command::attach_terminal(ResourceId::local(9), None),
        reply,
        subscribe: Some(ProxySubscription {
            resource_kind: None,
            terminal: 9,
            client: ClientId(1),
            out_tx,
            consumer_cancel: CancellationToken::new(),
            seq: 1,
            // Ungated so a rollback regression would visibly leak.
            awaits_snapshot: false,
            bootstrap_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: Some(BootstrapLimits::default()),
        }),
    });
    let FrameKind::Command { request_id, .. } = decode(&wire) else {
        panic!("expected COMMAND");
    };
    session
        .handle_inbound(&encode(&FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                message: "nope".to_owned(),
            },
        }))
        .expect("valid satellite frame");
    assert!(matches!(
        reply_rx.try_recv().expect("resolved"),
        CommandResult::Error { .. }
    ));
    // The rolled-back registration must not fan anything out.
    session
        .handle_inbound(&encode(&FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(9),
            stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
            seq: 1,
            bytes: bytes::Bytes::from_static(b"leak"),
        }))
        .expect("valid satellite frame");
    assert!(out_rx.try_recv().is_err(), "rolled-back subscriber leaked");
}

#[test]
fn errored_upgrade_preserves_a_concurrently_retained_snapshot() {
    // B upgrades (re-gated), C attaches and B retains C's snapshot, then
    // B's upgrade errors: the rollback must not replace Retained with
    // Open (L1 §9.1).
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (tx_b, mut rx_b) = mpsc::channel(2);
    let (tx_c, mut rx_c) = mpsc::channel(8);

    // B's event-only stream is established and demonstrably Open.
    subscribe(&mut session, 9, ClientId(2), tx_b.clone());
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: AgentEvent::CommandStarted,
            stamp: None,
        }))
        .expect("valid satellite frame");
    assert!(rx_b.try_recv().is_ok(), "B's event stream must be open");

    let (reply_b, mut reply_rx_b) = oneshot::channel();
    let wire = session.handle_request(RelayRequest::Command {
        command: Command::attach_terminal(ResourceId::local(9), None),
        reply: reply_b,
        subscribe: Some(ProxySubscription {
            resource_kind: None,
            terminal: 9,
            client: ClientId(2),
            out_tx: tx_b.clone(),
            consumer_cancel: CancellationToken::new(),
            seq: 2,
            awaits_snapshot: true,
            bootstrap_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: Some(BootstrapLimits::default()),
        }),
    });
    let FrameKind::Command { request_id, .. } = decode(&wire) else {
        panic!("expected B's attach COMMAND");
    };

    attach(&mut session, 9, ClientId(3), tx_c);
    tx_b.try_send(Outbound::Frame(FrameKind::Detach))
        .expect("B filler one");
    tx_b.try_send(Outbound::Frame(FrameKind::Detach))
        .expect("B filler two");
    session
        .handle_inbound(&encode(&snapshot_frame(9)))
        .expect("valid satellite frame");
    assert!(matches!(
        rx_c.try_recv().expect("C receives its snapshot"),
        Outbound::Frame(FrameKind::BootstrapReady { .. })
    ));

    session
        .handle_inbound(&encode(&FrameKind::Error {
            request_id: Some(request_id),
            code: ErrorCode::InternalError,
            message: "transient".to_owned(),
        }))
        .expect("valid satellite frame");
    assert!(matches!(
        reply_rx_b.try_recv().expect("B's upgrade resolves"),
        CommandResult::Error { .. }
    ));

    // B's next delta flushes the retained snapshot first.
    assert!(matches!(
        rx_b.try_recv().expect("B filler one drains"),
        Outbound::Frame(FrameKind::Detach)
    ));
    assert!(matches!(
        rx_b.try_recv().expect("B filler two drains"),
        Outbound::Frame(FrameKind::Detach)
    ));
    session
        .handle_inbound(&encode(&output_frame(9, 1, b"after-error")))
        .expect("valid satellite frame");
    assert!(matches!(
        rx_b.try_recv().expect("B's retained snapshot flushes"),
        Outbound::Frame(FrameKind::BootstrapReady { .. })
    ));
    assert!(matches!(
        rx_b.try_recv().expect("B's delta follows the snapshot"),
        Outbound::Frame(FrameKind::ResourceOutput { seq: 1, .. })
    ));
}

#[test]
fn satellite_error_never_rolls_back_a_preexisting_subscription() {
    // An errored upgrade re-subscribe restores the original stream's gate.
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = mpsc::channel(8);
    subscribe(&mut session, 9, ClientId(1), out_tx.clone());
    let (reply, _reply_rx) = oneshot::channel();
    let wire = session.handle_request(RelayRequest::Command {
        command: Command::attach_terminal(ResourceId::local(9), None),
        reply,
        subscribe: Some(ProxySubscription {
            resource_kind: None,
            terminal: 9,
            client: ClientId(1),
            out_tx,
            consumer_cancel: CancellationToken::new(),
            seq: 2,
            awaits_snapshot: true,
            bootstrap_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: Some(BootstrapLimits::default()),
        }),
    });
    let FrameKind::Command { request_id, .. } = decode(&wire) else {
        panic!("expected COMMAND");
    };
    session
        .handle_inbound(&encode(&FrameKind::Error {
            request_id: Some(request_id),
            code: ErrorCode::InternalError,
            message: "transient".to_owned(),
        }))
        .expect("valid satellite frame");
    session
        .handle_inbound(&encode(&FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(9),
            stream_id: phux_protocol::ids::StreamId::new(1).expect("test stream id"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
            seq: 1,
            bytes: bytes::Bytes::from_static(b"still here"),
        }))
        .expect("valid satellite frame");
    assert!(
        out_rx.try_recv().is_ok(),
        "pre-existing subscription must survive the errored re-subscribe"
    );
}

// --- fail-fast + handle backpressure -----------------------------------

#[tokio::test]
async fn fail_fast_resolves_commands_with_satellite_unreachable() {
    let (reply, rx) = oneshot::channel();
    fail_fast(
        RelayRequest::Command {
            command: Command::Upgrade,
            reply,
            subscribe: None,
        },
        &host(),
        "backoff",
    );
    assert!(matches!(
        rx.await.expect("resolved"),
        CommandResult::Error {
            code: ErrorCode::SatelliteUnreachable,
            ..
        }
    ));
}

#[tokio::test(start_paused = true)]
async fn handle_command_times_out_against_a_silent_satellite() {
    let (handle, rx) = RelayHandle::new(host());
    // The mailbox accepts, nothing answers; paused time hits the deadline.
    let result = handle.command(Command::Upgrade).await;
    assert!(matches!(
        result,
        CommandResult::Error {
            code: ErrorCode::SatelliteUnreachable,
            ref message,
        } if message.contains("did not answer within")
    ));
    drop(rx);
}

#[test]
fn prune_abandoned_drops_only_commands_whose_consumer_gave_up() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (reply_live, mut live_rx) = oneshot::channel();
    let (reply_gone, gone_rx) = oneshot::channel();
    let _ = session.handle_request(RelayRequest::Command {
        command: Command::Upgrade,
        reply: reply_live,
        subscribe: None,
    });
    let wire = session.handle_request(RelayRequest::Command {
        command: Command::Upgrade,
        reply: reply_gone,
        subscribe: None,
    });
    let FrameKind::Command { request_id, .. } = decode(&wire) else {
        panic!("expected COMMAND");
    };

    assert_eq!(session.prune_abandoned(), 0, "both consumers still wait");

    // The consumer of the second command times out / disconnects.
    drop(gone_rx);
    assert_eq!(session.prune_abandoned(), 1, "abandoned entry pruned");

    // A late reply for the pruned id is dropped.
    session
        .handle_inbound(&encode(&FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        }))
        .expect("valid satellite frame");
    assert!(live_rx.try_recv().is_err(), "live command still pending");
    session.teardown("done");
    assert!(matches!(
        live_rx.try_recv().expect("live command survived pruning"),
        CommandResult::Error {
            code: ErrorCode::SatelliteUnreachable,
            ..
        }
    ));
}

#[tokio::test]
async fn handle_command_fails_fast_when_mailbox_is_full_or_closed() {
    let (handle, mut mailbox) = RelayHandle::new(host());
    // Fill the bounded mailbox.
    for _ in 0..RELAY_MAILBOX {
        handle.forward(FrameKind::Detach);
    }
    let result = handle.command(Command::Upgrade).await;
    assert!(matches!(
        result,
        CommandResult::Error {
            code: ErrorCode::ResourceExhausted,
            ..
        }
    ));
    // Drop the receiver: the link task is gone.
    mailbox.requests.close();
    while mailbox.requests.try_recv().is_ok() {}
    drop(mailbox);
    let result = handle.command(Command::Upgrade).await;
    assert!(matches!(
        result,
        CommandResult::Error {
            code: ErrorCode::SatelliteUnreachable,
            ..
        }
    ));
}

// --- relayed LIST_DIRECTORY (docs/protocol.md#commands-events-resources) ---------------------
