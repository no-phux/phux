use phux_protocol::caps::BootstrapLimits;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::hub::relay::{HubRelays, RelayHandle, RelaySession};
use crate::state::{ClientId, Outbound, SharedState};

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
    let reply = reply_satellite_command(
        &ctx,
        9,
        &host,
        CommandResult::error(ErrorCode::SatelliteUnreachable, "offline"),
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
    use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};

    use crate::hub::relay::{ProxySubscription, RELAY_COMMAND_TIMEOUT, RelayRequest};
    let state = SharedState::new();
    let host = phux_protocol::ids::SatelliteHost::from("sat");
    let (handle, mut mailbox) = RelayHandle::new(host.clone());
    let relays = HubRelays::default();
    relays.insert(handle);
    state.with_mut(|s| {
        s.federation_mut().set_relays(relays);
        s.federation_mut()
            .register_satellite_proxy_attach(ClientId(1), host.clone(), 7);
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
    assert!(state.with(|s| {
        s.federation()
            .has_satellite_proxy_attach(ClientId(1), &host, 7)
    }));
    // The delayed link processes the withdrawal only after the refusal.
    assert!(
        session
            .handle_unsubscribe(mailbox.unsubscribes.try_recv().unwrap().unsubscribe)
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
    state.with_mut(|s| s.federation_mut().set_relays(relays));
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
    session.handle_unsubscribe(withdrawal.unsubscribe);
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
    state.with_mut(|s| {
        s.federation_mut()
            .register_satellite_proxy_attach(ClientId(1), host.clone(), 7);
    });
    let terminal = phux_protocol::ids::ResourceId::local(7);
    let detach = resolve_hub_detach_terminal(&handle, ClientId(1), &terminal);
    tokio::pin!(detach);
    assert!(futures_util::poll!(&mut detach).is_pending());
    let crate::hub::relay::Unsubscribe::Terminal {
        reply: Some(reply), ..
    } = mailbox.unsubscribes.try_recv().unwrap().unsubscribe
    else {
        panic!("withdrawal receipt");
    };
    reply.apply(|| {
        (
            CommandResult::error(ErrorCode::InvalidCommand, "superseded"),
            Vec::new(),
        )
    });
    assert!(matches!(detach.await, CommandResult::Error { .. }));
    assert!(state.with(|s| {
        s.federation()
            .has_satellite_proxy_attach(ClientId(1), &host, 7)
    }));
}

fn live_federation() -> (
    SharedState,
    phux_protocol::ids::SatelliteHost,
    crate::hub::relay::RelayHandle,
    crate::hub::relay::RelayMailbox,
    RelaySession,
) {
    let state = SharedState::new();
    let host = phux_protocol::ids::SatelliteHost::new("sat");
    let (handle, mailbox) = RelayHandle::new(host.clone());
    let mut session = RelaySession::new(host.clone(), BootstrapLimits::default());
    session.set_resource_kinds(std::sync::Arc::clone(&mailbox.resource_kinds));
    session.set_journal(Some(state.clone()));
    let relays = HubRelays::default();
    relays.insert(handle.clone());
    state.with_mut(|s| {
        s.federation_mut().set_relays(relays);
        s.set_client_connection_cancellation(ClientId(1), CancellationToken::new());
    });
    (state, host, handle, mailbox, session)
}

fn satellite_reply(session: &mut RelaySession, request_id: u32) {
    let mut bytes = bytes::BytesMut::new();
    FrameKind::CommandResult {
        request_id,
        result: CommandResult::Ok,
    }
    .encode(&mut bytes);
    session.handle_inbound(&bytes).unwrap();
}

#[tokio::test]
async fn whole_client_disconnect_clears_proxy_without_a_terminal_detach() {
    use crate::hub::relay::{ProxySubscription, RelayRequest};
    let (state, host, _handle, mut mailbox, mut session) = live_federation();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
    session
        .handle_request_checked(RelayRequest::Subscribe {
            subscription: ProxySubscription {
                terminal: 7,
                client: ClientId(1),
                out_tx,
                consumer_cancel: CancellationToken::new(),
                seq: 1,
                awaits_snapshot: false,
                resource_kind: None,
                bootstrap_profile: None,
                bootstrap_limits: None,
            },
            forward: FrameKind::SubscribeEvents {
                terminal: Some(phux_protocol::ids::ResourceId::local(7)),
                after_seq: None,
            },
        })
        .unwrap();
    let owner = state.with_mut(|s| {
        s.federation_mut()
            .register_satellite_proxy_attach(ClientId(1), host.clone(), 7);
        s.federation_mut().consumer_registration(ClientId(1))
    });
    crate::runtime::connections::detach_and_release_consumer_state(&state, ClientId(1));
    assert!(owner.is_cancelled());
    assert!(!state.with(|s| {
        s.federation()
            .has_satellite_proxy_attach(ClientId(1), &host, 7)
    }));
    assert_eq!(
        session
            .handle_queued_unsubscribe(mailbox.unsubscribes.try_recv().unwrap())
            .len(),
        1
    );
    let mut bytes = bytes::BytesMut::new();
    FrameKind::Event {
        terminal: Some(phux_protocol::ids::ResourceId::local(7)),
        event: phux_protocol::wire::frame::AgentEvent::CommandStarted,
        stamp: None,
    }
    .encode(&mut bytes);
    session.handle_inbound(&bytes).unwrap();
    assert!(
        out_rx.try_recv().is_err(),
        "a disconnected client receives no proxy output"
    );
    assert!(
        state.with(|s| s.federation().relay(&host).is_some()),
        "configured host route survives consumer teardown"
    );
}

#[tokio::test]
async fn an_acknowledged_old_attach_cannot_reinstall_after_link_loss() {
    let (state, host, _handle, mut mailbox, mut session) = live_federation();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
    let ctx = CommandContext::for_test(&state, ClientId(1), &out_tx);
    let mut attach = Box::pin(handle_satellite_command(
        &ctx,
        17,
        &host,
        Command::attach_terminal(phux_protocol::ids::ResourceId::local(7), None),
    ));
    assert!(futures_util::poll!(&mut attach).is_pending());
    let frames = session.handle_queued_request(mailbox.requests.try_recv().unwrap());
    let request_id = frames
        .iter()
        .find_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::Command {
                request_id,
                command: Command::AttachResource { .. },
            } => Some(request_id),
            _ => None,
        })
        .expect("attach reaches the satellite");
    satellite_reply(&mut session, request_id);
    // The reply is ready, but its consumer continuation has not run.
    session.teardown("disconnected before consumer resumed");
    assert!(
        state.with(|s| s.federation().relay(&host).is_some()),
        "configured route survives link loss"
    );
    attach.await;
    assert!(!state.with(|s| {
        s.federation()
            .has_satellite_proxy_attach(ClientId(1), &host, 7)
    }));
    let replies: Vec<_> = std::iter::from_fn(|| out_rx.try_recv().ok()).collect();
    assert!(replies.iter().any(|out| matches!(
        out,
        Outbound::Frame(FrameKind::CommandResult {
            request_id: 17,
            result: CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                ..
            }
        })
    )));
}

#[tokio::test]
async fn a_ready_acquire_reply_cannot_reinstall_after_client_or_link_teardown() {
    use super::satellite_lease::{SatelliteLeaseTarget, relay_satellite_acquire_input};
    use phux_protocol::wire::frame::InputMode;
    for disconnect_link in [false, true] {
        let (state, host, handle, mut mailbox, mut session) = live_federation();
        let terminal = phux_protocol::ids::ResourceId::local(7);
        let target = SatelliteLeaseTarget::new(&state, &host, ClientId(1), &terminal);
        let (out_tx, _out_rx) = tokio::sync::mpsc::channel(8);
        let command = Command::AcquireInput {
            terminal_id: terminal,
            mode: InputMode::Seize,
            ttl_ms: 0,
        };
        let mut acquire = Box::pin(relay_satellite_acquire_input(
            &target,
            &handle,
            InputMode::Seize,
            &command,
            &out_tx,
        ));
        assert!(futures_util::poll!(&mut acquire).is_pending());
        let frames = session.handle_queued_request(mailbox.requests.try_recv().unwrap());
        let FrameKind::Command { request_id, .. } = FrameKind::decode(&frames[0]).unwrap().0 else {
            panic!("acquire reaches the satellite");
        };
        satellite_reply(&mut session, request_id);
        if disconnect_link {
            session.teardown("link lost");
        } else {
            crate::runtime::connections::detach_and_release_consumer_state(&state, ClientId(1));
        }
        assert!(matches!(
            acquire.await,
            CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                ..
            }
        ));
        assert_eq!(
            state.with(|s| s.federation().satellite_lease_holder(&host, 7)),
            None
        );
        assert_eq!(
            state.with(|s| s.federation().satellite_lease_mirror_entries()),
            0
        );
    }
}

#[tokio::test]
async fn dropping_an_acquire_clears_only_its_own_marker() {
    use super::satellite_lease::{SatelliteLeaseTarget, relay_satellite_acquire_input};
    use phux_protocol::wire::frame::InputMode;
    let (state, host, handle, _mailbox, _session) = live_federation();
    let terminal = phux_protocol::ids::ResourceId::local(7);
    let target = SatelliteLeaseTarget::new(&state, &host, ClientId(1), &terminal);
    let (out_tx, _out_rx) = tokio::sync::mpsc::channel(8);
    let command = Command::AcquireInput {
        terminal_id: terminal,
        mode: InputMode::Seize,
        ttl_ms: 0,
    };
    let mut acquire = Box::pin(relay_satellite_acquire_input(
        &target,
        &handle,
        InputMode::Seize,
        &command,
        &out_tx,
    ));
    assert!(futures_util::poll!(&mut acquire).is_pending());
    assert_eq!(
        state.with(|s| s.federation().satellite_lease_mirror_entries()),
        1
    );
    drop(acquire);
    assert_eq!(
        state.with(|s| s.federation().satellite_lease_mirror_entries()),
        0
    );

    let mut prior = Box::pin(relay_satellite_acquire_input(
        &target,
        &handle,
        InputMode::Seize,
        &command,
        &out_tx,
    ));
    assert!(futures_util::poll!(&mut prior).is_pending());
    let mut replacement = Box::pin(relay_satellite_acquire_input(
        &target,
        &handle,
        InputMode::Seize,
        &command,
        &out_tx,
    ));
    assert!(futures_util::poll!(&mut replacement).is_pending());
    drop(prior);
    assert_eq!(
        state.with(|s| s.federation().satellite_lease_mirror_entries()),
        1,
        "an older cancellation cannot clear a newer acquire"
    );
    drop(replacement);
    assert_eq!(
        state.with(|s| s.federation().satellite_lease_mirror_entries()),
        0
    );
}

#[tokio::test]
async fn a_delayed_detach_receipt_cannot_erase_a_newer_proxy_registration() {
    use crate::hub::relay::{ProxySubscription, RelayRequest};
    let (state, host, handle, mut mailbox, mut session) = live_federation();
    let (out_tx, _out_rx) = tokio::sync::mpsc::channel(8);
    let subscription = |seq| ProxySubscription {
        terminal: 7,
        client: ClientId(1),
        out_tx: out_tx.clone(),
        consumer_cancel: CancellationToken::new(),
        seq,
        awaits_snapshot: true,
        resource_kind: None,
        bootstrap_profile: Some(phux_protocol::caps::BootstrapProfile::SynthesizedVtRaw),
        bootstrap_limits: Some(BootstrapLimits::default()),
    };
    let terminal = phux_protocol::ids::ResourceId::local(7);
    let (reply, _received) = tokio::sync::oneshot::channel();
    let frame = session
        .handle_request_checked(RelayRequest::Command {
            command: Command::attach_terminal(terminal.clone(), None),
            reply,
            subscribe: Some(subscription(0)),
        })
        .unwrap();
    let FrameKind::Command { request_id, .. } = FrameKind::decode(&frame).unwrap().0 else {
        panic!("first attach");
    };
    satellite_reply(&mut session, request_id);
    assert!(state.with(|s| {
        s.federation()
            .has_satellite_proxy_attach(ClientId(1), &host, 7)
    }));
    let mut detach = Box::pin(resolve_hub_detach_terminal(&handle, ClientId(1), &terminal));
    assert!(futures_util::poll!(&mut detach).is_pending());
    let frames = session.handle_queued_unsubscribe(mailbox.unsubscribes.try_recv().unwrap());
    assert!(
        !state.with(|s| s
            .federation()
            .has_satellite_proxy_attach(ClientId(1), &host, 7)),
        "entitlement is removed at the acknowledged mutation, not when its waiter resumes"
    );
    let FrameKind::Command { request_id, .. } = FrameKind::decode(&frames[0]).unwrap().0 else {
        panic!("upstream withdrawal barrier");
    };
    satellite_reply(&mut session, request_id);
    let (reply, _replacement_received) = tokio::sync::oneshot::channel();
    let frame = session
        .handle_request_checked(RelayRequest::Command {
            command: Command::attach_terminal(terminal.clone(), None),
            reply,
            subscribe: Some(subscription(2)),
        })
        .unwrap();
    let FrameKind::Command { request_id, .. } = FrameKind::decode(&frame).unwrap().0 else {
        panic!("replacement attach");
    };
    satellite_reply(&mut session, request_id);
    assert_eq!(detach.await, CommandResult::Ok);
    assert!(state.with(|s| {
        s.federation()
            .has_satellite_proxy_attach(ClientId(1), &host, 7)
    }));
}
