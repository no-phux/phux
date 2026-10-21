//! Relayed events, lease mirroring, and spawn bookkeeping across teardown.

use super::*;

#[test]
fn hub_restamps_a_satellite_pane_closed_with_the_consumers_actor_by_operation_id() {
    let journal = crate::state::SharedState::new();
    let consumer = journal.with_mut(crate::state::ServerState::new_client_id);
    let (registry_tx, mut registry_rx) = mpsc::channel(8);
    journal.with_mut(|s| {
        s.subscribe_satellite_events(
            consumer,
            ResourceId::satellite("devbox", 9),
            crate::state::EventFilter::all(),
            None,
            registry_tx,
        );
    });
    let fence = crate::hub::operation_fence::OperationFence::default();
    let mut session = keyed_session(&[ServerFeature::KeyedSignal], [1; 16], &fence);
    session.set_journal(Some(journal));
    let (proxy_tx, _proxy_rx) = mpsc::channel(8);
    subscribe(&mut session, 9, consumer, proxy_tx);
    let (wire, _rx) = send_keyed(&mut session, keyed_kill(7), consumer);
    assert!(wire.is_some());

    let key = phux_protocol::ids::IdempotencyKey::new([7; 16]);
    let other = phux_protocol::ids::IdempotencyKey::new([8; 16]);
    for operation_id in [key, other] {
        let stamp =
            phux_protocol::wire::frame::EventStamp::new(40, 1_234).with_operation_id(operation_id);
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(9)),
                event: AgentEvent::ResourceClosed { exit_status: None },
                stamp: Some(Box::new(stamp)),
            }))
            .expect("valid satellite frame");
    }
    let stamps: Vec<_> = std::iter::from_fn(|| registry_rx.try_recv().ok())
        .map(|out| match out {
            Outbound::Frame(FrameKind::Event { stamp, .. }) => stamp.expect("stamped by the hub"),
            other => panic!("expected an EVENT, got {other:?}"),
        })
        .collect();
    let [ours, theirs] = stamps.as_slice() else {
        panic!("two relayed pane_closed events: {stamps:?}");
    };
    assert_eq!(ours.operation_id, key, "the key crosses the hub");
    assert!(
        ours.actor.is_some(),
        "the event names the consumer that sent the keyed kill, not the link"
    );
    assert_eq!(theirs.operation_id, other);
    assert_eq!(
        theirs.actor, None,
        "an operation this hub never forwarded names no actor"
    );
}

/// L1 §9.1: every id inside a satellite's event is re-tagged or vetted
/// before a hub consumer sees it. An operation id a consumer forwarded
/// for one terminal cannot be claimed on another (neither the id nor
/// the consumer survives); the satellite's client ids become the hub's
/// lease holder or a foreign id; a resource parent is re-tagged; and an
/// approval event naming a hub approval is dropped.
#[allow(
    clippy::too_many_lines,
    reason = "one relayed-event scenario per id kind"
)]
#[test]
fn satellite_event_ids_are_retagged_or_vetted_before_hub_consumers_see_them() {
    let journal = crate::state::SharedState::new();
    let consumer = journal.with_mut(crate::state::ServerState::new_client_id);
    let (registry_tx, mut registry_rx) = mpsc::channel(32);
    for terminal in [5, 9] {
        journal.with_mut(|s| {
            s.subscribe_satellite_events(
                consumer,
                ResourceId::satellite("devbox", terminal),
                crate::state::EventFilter::all(),
                None,
                registry_tx.clone(),
            );
        });
    }
    let fence = crate::hub::operation_fence::OperationFence::default();
    let mut session = keyed_session(&[ServerFeature::KeyedSignal], [1; 16], &fence);
    session.set_journal(Some(journal.clone()));
    let (proxy_tx, _proxy_rx) = mpsc::channel(8);
    subscribe(&mut session, 9, consumer, proxy_tx.clone());
    subscribe(&mut session, 5, consumer, proxy_tx);
    let (wire, _rx) = send_keyed(&mut session, keyed_kill(7), consumer);
    assert!(wire.is_some(), "the kill for terminal 9 is forwarded");
    let mut relay = |terminal: u32, event: AgentEvent, stamp: Option<Box<_>>| {
        session
            .handle_inbound(&encode(&FrameKind::Event {
                terminal: Some(ResourceId::local(terminal)),
                event,
                stamp,
            }))
            .expect("valid satellite frame");
        std::iter::from_fn(|| registry_rx.try_recv().ok())
            .filter_map(|out| match out {
                Outbound::Frame(FrameKind::Event { event, stamp, .. }) => Some((event, stamp)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let control = |holder: u32| AgentEvent::TerminalControl {
        lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Running,
        exit_status: None,
        input_holder: Some(phux_protocol::ids::ClientId::new(holder)),
        action: ControlAction::Interrupted,
        actor: Some(phux_protocol::ids::ClientId::new(holder)),
    };
    let wire_consumer = crate::runtime::wire_client(consumer);

    // The kill's key, claimed on terminal 5: dropped with its consumer.
    let key = phux_protocol::ids::IdempotencyKey::new([7; 16]);
    let stamp = phux_protocol::wire::frame::EventStamp::new(1, 1).with_operation_id(key);
    let got = relay(5, control(wire_consumer.get()), Some(Box::new(stamp)));
    let [(event, Some(stamp))] = got.as_slice() else {
        panic!("one relayed event: {got:?}");
    };
    assert_eq!(
        stamp.operation_id, None,
        "the id is not the satellite's to claim"
    );
    assert_eq!(stamp.actor, None);
    let AgentEvent::TerminalControl {
        input_holder,
        actor,
        ..
    } = event
    else {
        panic!("expected terminal_control: {event:?}");
    };
    assert_ne!(
        *input_holder,
        Some(wire_consumer),
        "a satellite client id never reads as a hub consumer"
    );
    assert_eq!(*input_holder, Some(foreign_client(wire_consumer)));
    assert_eq!(*actor, Some(foreign_client(wire_consumer)));

    // On terminal 9 the same key is the consumer's own operation, and
    // the hub's lease ledger names the holder.
    let (lease_tx, _lease_rx) = mpsc::channel(1);
    journal.with_mut(|s| {
        s.set_satellite_lease(host(), 9, consumer, lease_tx);
    });
    let stamp = phux_protocol::wire::frame::EventStamp::new(2, 1).with_operation_id(key);
    let got = relay(9, control(1), Some(Box::new(stamp)));
    let [(event, Some(stamp))] = got.as_slice() else {
        panic!("one relayed event: {got:?}");
    };
    assert_eq!(stamp.operation_id, key);
    assert!(stamp.actor.is_some(), "the consumer's own kill names it");
    let AgentEvent::TerminalControl {
        input_holder,
        actor,
        ..
    } = event
    else {
        panic!("expected terminal_control: {event:?}");
    };
    assert_eq!(
        *input_holder,
        Some(wire_consumer),
        "the hub's ledger holder"
    );
    assert_eq!(*actor, Some(wire_consumer), "the operation's sender");

    // A child's parent is re-tagged into the hub's id space.
    let got = relay(
        5,
        AgentEvent::ResourceSpawned {
            kind: phux_protocol::ids::ResourceKind::AgentSession,
            parent: Some(ResourceId::local(9)),
        },
        None,
    );
    assert!(
        matches!(
            got.as_slice(),
            [(AgentEvent::ResourceSpawned { parent: Some(parent), .. }, _)]
                if *parent == ResourceId::satellite(host(), 9)
        ),
        "{got:?}"
    );

    // An approval event naming the hub's own pending approval is dropped.
    let held = Command::KillResource {
        terminal_id: ResourceId::satellite(host(), 9),
        operation_id: None,
    };
    let approval = journal
        .with_mut(|s| s.open_approval(consumer, &held))
        .expect("hold opened")
        .id;
    let got = relay(
        9,
        AgentEvent::ApprovalDecided {
            id: approval,
            outcome: phux_protocol::wire::frame::ApprovalOutcome::Denied,
        },
        None,
    );
    assert!(
        !got.iter()
            .any(|(event, _)| matches!(event, AgentEvent::ApprovalDecided { .. })),
        "{got:?}"
    );
    assert!(
        journal.with(|s| s.pending_approval(approval).is_some()),
        "the hub's approval is untouched"
    );
}

#[test]
fn instance_only_close_preserves_satellite_fence_and_correlates_refusal() {
    use phux_protocol::wire::frame::{KillConditions, KillPrecondition};
    let precondition = KillPrecondition {
        instance: Some(phux_protocol::ids::ServerInstance::new([3; 16])),
        conditions: KillConditions::NONE,
    };
    let (target, command) = route_to_satellite(&Command::KillResourceIf {
        terminal_id: ResourceId::satellite(host(), 9),
        precondition,
        operation_id: None,
    })
    .expect("satellite route");
    assert_eq!(target, host());
    let mut session = RelaySession::new_negotiated(
        host(),
        BootstrapLimits::default(),
        BootstrapProfile::SynthesizedVtRaw,
        ServerFeatureSet::with(&[ServerFeature::ConditionalKill]),
    );
    let (reply, mut rx) = oneshot::channel();
    let wire = session
        .handle_request_checked(RelayRequest::Command {
            command,
            reply,
            subscribe: None,
        })
        .expect("instance-only kill reaches capable satellite");
    let FrameKind::Command {
        request_id,
        command,
    } = decode(&wire)
    else {
        panic!("expected command");
    };
    assert_eq!(
        command,
        Command::KillResourceIf {
            terminal_id: ResourceId::local(9),
            precondition,
            operation_id: None,
        }
    );
    let refusal = CommandResult::Error {
        code: ErrorCode::PreconditionFailed,
        message: "satellite incarnation changed".into(),
    };
    session
        .handle_inbound(&encode(&FrameKind::CommandResult {
            request_id,
            result: refusal.clone(),
        }))
        .expect("valid satellite refusal");
    assert_eq!(rx.try_recv().expect("correlated outcome"), refusal);
    assert!(session.pending.is_empty());
}

#[tokio::test]
async fn handle_and_session_preserve_spawn_owner_geometry_and_pty_options() {
    let (handle, mut mailbox) = RelayHandle::new(host());
    let spawn = SatelliteSpawn {
        group: GroupId::new(7),
        command: Some(vec!["/bin/cat".to_owned()]),
        cwd: Some("/work".to_owned()),
        env: Some(vec![("SPLIT".to_owned(), "yes".to_owned())]),
        term: Some("xterm-256color".to_owned()),
        owner_terminal: Some(91),
        initial_size: Some((132, 43)),
        resource: None,
    };
    let expected = FrameKind::SpawnResource {
        request_id: 0,
        group: spawn.group,
        command: spawn.command.clone(),
        cwd: spawn.cwd.clone(),
        env: spawn.env.clone(),
        term: spawn.term.clone(),
        satellite: None,
        owner_terminal: Some(ResourceId::local(91)),
        agent_session: None,
        initial_size: Some((132, 43)),
        resource: None,
    };
    let consumer = handle.spawn(spawn);
    let satellite = async {
        let request = mailbox.requests.recv().await.expect("spawn enqueued");
        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let mut frame = decode(&session.handle_request(request));
        let FrameKind::SpawnResource { request_id, .. } = &mut frame else {
            panic!("spawn frame");
        };
        let link_request_id = *request_id;
        *request_id = 0;
        assert_eq!(frame, expected);
        session
            .handle_inbound(&encode(&FrameKind::ResourceSpawned {
                request_id: link_request_id,
                result: SpawnResult::Ok(ResourceId::local(92)),
            }))
            .expect("spawn reply");
    };
    let (result, ()) = tokio::join!(consumer, satellite);
    assert_eq!(result, SpawnResult::Ok(ResourceId::satellite("devbox", 92)));
}

#[test]
fn teardown_and_fail_fast_resolve_spawns_with_satellite_unreachable() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (reply, mut rx) = oneshot::channel();
    let _ = session.handle_request(spawn_request(reply));
    session.teardown("satellite went away");
    assert!(matches!(
        rx.try_recv().expect("pending spawn failed"),
        SpawnResult::Err(SpawnError::SatelliteUnreachable(_))
    ));

    let (reply, mut rx) = oneshot::channel();
    fail_fast(spawn_request(reply), &host(), "backoff");
    assert!(matches!(
        rx.try_recv().expect("spawn failed fast"),
        SpawnResult::Err(SpawnError::SatelliteUnreachable(_))
    ));
}

#[test]
fn prune_abandoned_covers_pending_spawns() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (reply, rx) = oneshot::channel();
    let _ = session.handle_request(spawn_request(reply));
    assert_eq!(session.prune_abandoned(), 0, "consumer still waits");
    drop(rx);
    assert_eq!(session.prune_abandoned(), 1, "abandoned spawn pruned");
}

// --- session: journal re-stamp (ADR-0123) -----------------------------
