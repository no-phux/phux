use super::*;
use crate::hub::relay::{HubRelays, RelayHandle, RelaySession};

#[tokio::test]
async fn cancellation_interrupts_a_blocked_satellite_reply() {
    let state = SharedState::new();
    let host = phux_protocol::ids::SatelliteHost::from("sat");
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(1);
    out_tx
        .send(Outbound::Frame(FrameKind::Pong { nonce: 1 }))
        .await
        .expect("fill consumer mailbox");
    let retained_sender = out_tx.clone();
    let ctx = CommandContext::for_test(&state, ClientId(17), &out_tx);
    let token = ctx.token.clone();
    let command = Command::DetachResource {
        terminal_id: phux_protocol::ids::ResourceId::local(7),
    };
    let reply = reply_satellite_command(
        &ctx,
        9,
        &host,
        &command,
        CommandResult::Error {
            code: ErrorCode::SatelliteUnreachable,
            message: "offline".to_owned(),
        },
    );
    tokio::pin!(reply);
    assert!(futures_util::poll!(&mut reply).is_pending());

    token.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), reply)
        .await
        .expect("cancellation interrupts the full-mailbox reply");
    assert!(
        !retained_sender.is_closed(),
        "extra production-equivalent sender remains owned"
    );
    assert!(matches!(
        out_rx.try_recv(),
        Ok(Outbound::Frame(FrameKind::Pong { nonce: 1 }))
    ));
}

#[tokio::test(start_paused = true)]
async fn timed_out_hub_detach_preserves_resumed_generation_with_shared_observer() {
    use crate::hub::relay::{ProxySubscription, RELAY_COMMAND_TIMEOUT, RelayRequest};
    use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
    let state = SharedState::new();
    let host = phux_protocol::ids::SatelliteHost::from("sat");
    let (handle, mut mailbox) = RelayHandle::new(host.clone());
    let relays = HubRelays::default();
    relays.insert(handle);
    state.with_mut(|s| {
        s.set_hub_relays(relays);
        s.register_satellite_proxy_attach(ClientId(1), host.clone(), 7);
    });
    let mut session = RelaySession::new(host.clone(), BootstrapLimits::default());
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
    let (observer_tx, mut observer_rx) = tokio::sync::mpsc::channel(8);
    for (client, out_tx) in [(ClientId(1), out_tx.clone()), (ClientId(2), observer_tx)] {
        session
            .handle_request_checked(RelayRequest::Subscribe {
                subscription: ProxySubscription {
                    resource_kind: None,
                    terminal: 7,
                    client,
                    out_tx,
                    consumer_cancel: CancellationToken::new(),
                    seq: 0,
                    awaits_snapshot: false,
                    bootstrap_profile: None,
                    bootstrap_limits: None,
                },
                forward: FrameKind::SubscribeEvents {
                    terminal: Some(ResourceId::local(7)),
                    after_seq: None,
                },
            })
            .unwrap();
    }
    let ctx = CommandContext::for_test(&state, ClientId(1), &out_tx);
    let detach = handle_satellite_command(
        &ctx,
        42,
        &host,
        Command::DetachResource {
            terminal_id: ResourceId::local(7),
        },
    );
    tokio::pin!(detach);
    assert!(futures_util::poll!(&mut detach).is_pending());
    tokio::time::advance(RELAY_COMMAND_TIMEOUT).await;
    detach.await;
    assert!(matches!(
        out_rx.try_recv(),
        Ok(Outbound::Frame(FrameKind::CommandResult {
            request_id: 42,
            result: CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                ..
            }
        }))
    ));
    assert!(state.with(|s| s.has_satellite_proxy_attach(ClientId(1), &host, 7)));
    // The delayed link processes the withdrawal only after the refusal.
    assert!(
        session
            .handle_unsubscribe(mailbox.unsubscribes.try_recv().unwrap())
            .is_empty()
    );
    let frame = FrameKind::ResourceOutput {
        terminal_id: ResourceId::local(7),
        stream_id: StreamId::new(1).unwrap(),
        bootstrap_id: BootstrapId::new(1).unwrap(),
        seq: 12,
        bytes: bytes::Bytes::from_static(b"resumed generation"),
    };
    let mut encoded = bytes::BytesMut::new();
    frame.encode(&mut encoded);
    session.handle_inbound(&encoded).unwrap();
    assert!(matches!(
        observer_rx.try_recv(),
        Ok(Outbound::Frame(FrameKind::ResourceOutput { seq: 12, .. }))
    ));
    assert!(
        matches!(
            out_rx.try_recv(),
            Ok(Outbound::Frame(FrameKind::ResourceOutput { seq: 12, .. }))
        ),
        "ordinary detach refusal must preserve the resumed client's output, not just its input entitlement"
    );
}

#[tokio::test]
async fn satellite_detach_reply_waits_for_proxy_withdrawal() {
    let state = SharedState::new();
    let host = phux_protocol::ids::SatelliteHost::from("sat");
    let (handle, mut mailbox) = RelayHandle::new(host.clone());
    let relays = HubRelays::default();
    relays.insert(handle);
    state.with_mut(|s| s.set_hub_relays(relays));
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
    let ctx = CommandContext::for_test(&state, ClientId(1), &out_tx);
    let detach = handle_satellite_command(
        &ctx,
        42,
        &host,
        Command::DetachResource {
            terminal_id: phux_protocol::ids::ResourceId::local(7),
        },
    );
    tokio::pin!(detach);
    assert!(
        futures_util::poll!(&mut detach).is_pending(),
        "DETACH_RESOURCE replied before the link applied its proxy withdrawal"
    );
    assert!(
        out_rx.try_recv().is_err(),
        "no correlated success before withdrawal"
    );
    let withdrawal = mailbox
        .unsubscribes
        .try_recv()
        .expect("undroppable withdrawal");
    let mut session = RelaySession::new(host.clone(), BootstrapLimits::default());
    session.handle_unsubscribe(withdrawal);
    detach.await;
    assert!(matches!(
        out_rx.recv().await,
        Some(Outbound::Frame(FrameKind::CommandResult {
            request_id: 42,
            result: CommandResult::Ok
        }))
    ));
}

#[tokio::test]
async fn refused_hub_detach_preserves_the_newer_proxy_input_registration() {
    let state = SharedState::new();
    let host = phux_protocol::ids::SatelliteHost::from("sat");
    let (handle, mut mailbox) = RelayHandle::new(host.clone());
    state.with_mut(|s| s.register_satellite_proxy_attach(ClientId(1), host.clone(), 7));
    let terminal = phux_protocol::ids::ResourceId::local(7);
    let detach = resolve_hub_detach_terminal(&state, &handle, ClientId(1), &host, &terminal);
    tokio::pin!(detach);
    assert!(futures_util::poll!(&mut detach).is_pending());
    let crate::hub::relay::Unsubscribe::Terminal {
        reply: Some(reply), ..
    } = mailbox.unsubscribes.try_recv().unwrap()
    else {
        panic!("withdrawal receipt");
    };
    reply.apply(|| {
        (
            CommandResult::Error {
                code: ErrorCode::InvalidCommand,
                message: "superseded".to_owned(),
            },
            Vec::new(),
        )
    });
    assert!(matches!(detach.await, CommandResult::Error { .. }));
    assert!(state.with(|s| s.has_satellite_proxy_attach(ClientId(1), &host, 7)));
}
