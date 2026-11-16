//! What a hub forwards only to a satellite that advertises it, and incarnation fences.

use super::*;

#[test]
fn ordinary_hub_admission_requires_each_commands_own_extended_bit() {
    use phux_protocol::checkout::CheckoutRequest;
    use phux_protocol::service_http::ServiceHttpRequest;
    let commands = [
        (
            Command::GetFile {
                host: None,
                path: "/tmp/file".into(),
                offset: 0,
                max_bytes: 1,
            },
            ServerFeatureExt::FileDownload,
        ),
        (
            Command::Mirror {
                host: None,
                op: phux_protocol::wire::frame::MirrorOp::Targets,
            },
            ServerFeatureExt::Mirrors,
        ),
        (
            Command::Checkout {
                terminal_id: ResourceId::local(1),
                request: CheckoutRequest::Status,
            },
            ServerFeatureExt::Checkout,
        ),
        (
            Command::ServiceHttpOpen {
                request: Box::new(ServiceHttpRequest {
                    host: None,
                    service: "test".into(),
                    method: "GET".into(),
                    target: "/".into(),
                    headers: vec![],
                    body: vec![],
                }),
            },
            ServerFeatureExt::HostServiceHttp,
        ),
        (
            Command::ServiceHttpRead {
                exchange: 1,
                max_bytes: 1,
            },
            ServerFeatureExt::HostServiceHttp,
        ),
        (
            Command::ServiceHttpClose { exchange: 1 },
            ServerFeatureExt::HostServiceHttp,
        ),
    ];
    for ext in std::iter::once(ServerFeatureExtSet::new()).chain(
        ServerFeatureExt::ALL
            .iter()
            .map(|&feature| ServerFeatureExtSet::with(&[feature])),
    ) {
        let mut session = negotiated(ServerFeature::ALL);
        session.set_satellite_features_ext(ext);
        for (command, needed) in &commands {
            assert_eq!(
                session.command_rejection(command).is_none(),
                ext.contains(*needed),
                "{command:?} with {ext:?}"
            );
        }
    }
}

/// ADR-0124: `retain_secs` crosses only with `RETAIN_ON_EXIT`; the hub
/// refuses otherwise. A spawn without it still relays.
#[test]
fn hub_forwards_retain_secs_only_to_a_satellite_that_honors_it() {
    let mut capable = negotiated(&[ServerFeature::RetainOnExit]);
    let retained =
        || phux_protocol::wire::frame::SpawnResource::default().with_retain_secs(Some(600));
    let (reply, _rx) = oneshot::channel();
    let request = spawn_with(reply, retained());
    let wire = capable
        .handle_request_checked(request)
        .expect("relayed to a satellite that honors retain_secs");
    let FrameKind::SpawnResource { resource, .. } = decode(&wire) else {
        panic!("expected SPAWN_RESOURCE on the wire");
    };
    assert_eq!(
        resource.and_then(|r| r.retain_secs),
        Some(600),
        "retain_secs crosses the link"
    );

    let mut older = RelaySession::new(host(), BootstrapLimits::default());
    let (reply, mut rx) = oneshot::channel();
    assert!(
        older
            .handle_request_checked(spawn_with(reply, retained()))
            .is_none(),
        "the satellite never sees the retained spawn"
    );
    assert!(older.pending_spawns.is_empty());
    assert!(matches!(
        rx.try_recv().expect("typed refusal"),
        SpawnResult::Err(SpawnError::SpawnFailed(message))
            if message.contains("lacks RETAIN_ON_EXIT")
    ));

    let (reply, _rx) = oneshot::channel();
    assert!(
        older.handle_request_checked(spawn_request(reply)).is_some(),
        "a spawn that omits retain_secs still relays"
    );
}

/// ADR-0126: a keyed spawn to a satellite without `SPAWN_IDEMPOTENCY`
/// is refused; an unkeyed one relays.
#[test]
fn hub_refuses_a_keyed_satellite_spawn_when_the_satellite_lacks_the_bit() {
    let key = phux_protocol::ids::IdempotencyKey::new([7; 16]);
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (reply, mut rx) = oneshot::channel();
    let keyed = phux_protocol::wire::frame::SpawnResource::default().with_idempotency_key(key);
    assert!(
        session
            .handle_request_checked(spawn_with(reply, keyed))
            .is_none(),
        "the satellite never sees the keyed spawn"
    );
    assert!(session.pending_spawns.is_empty());
    assert!(matches!(
        rx.try_recv().expect("typed refusal"),
        SpawnResult::Err(SpawnError::SpawnFailed(message))
            if message.contains("lacks SPAWN_IDEMPOTENCY")
    ));

    let (reply, _rx) = oneshot::channel();
    assert!(
        session
            .handle_request_checked(spawn_request(reply))
            .is_some(),
        "an unkeyed spawn to the same satellite still relays"
    );
}

/// ADR-0109: without `CONDITIONAL_KILL` the consumer is refused at once;
/// with it the precondition crosses unchanged.
#[test]
fn a_conditional_kill_reaches_only_a_satellite_that_evaluates_it() {
    let command = Command::KillResourceIf {
        terminal_id: ResourceId::local(9),
        precondition: phux_protocol::wire::frame::KillPrecondition::spawned_and_unattached(
            phux_protocol::ids::ServerInstance::new([3; 16]),
        ),
        operation_id: None,
    };
    let mut older = RelaySession::new(host(), BootstrapLimits::default());
    let (wire, mut rx) = send_command(&mut older, command.clone());
    assert!(wire.is_none(), "an older satellite never sees the tag");
    assert!(older.pending.is_empty());
    assert!(matches!(
        rx.try_recv().expect("typed refusal"),
        CommandResult::Error {
            code: ErrorCode::PreconditionFailed,
            ..
        }
    ));

    let mut current = negotiated(&[ServerFeature::ConditionalKill]);
    let relayed = forwarded(send_command(&mut current, command.clone()).0);
    assert_eq!(relayed, command, "the precondition crosses unchanged");
}

// --- keyed operations and the incarnation fence (L1 §9.1) -----------

fn spawn_with(
    reply: oneshot::Sender<SpawnResult>,
    resource: phux_protocol::wire::frame::SpawnResource,
) -> RelayRequest {
    let mut request = spawn_request(reply);
    if let RelayRequest::Spawn { spawn, .. } = &mut request {
        spawn.resource = Some(Box::new(resource));
    }
    request
}

pub(super) fn keyed_session(
    features: &[ServerFeature],
    incarnation: [u8; 16],
    fence: &crate::hub::operation_fence::OperationFence,
) -> RelaySession {
    let mut session = negotiated(features);
    session.set_incarnation(Some(incarnation));
    session.set_operation_fence(fence.clone());
    session
}

pub(super) fn apply_input_to(terminal_id: ResourceId) -> Command {
    Command::ApplyInput {
        operation_id: phux_protocol::InputOperationId::new([6; 16]).expect("id"),
        terminal_id,
        events: vec![],
    }
}

pub(super) fn keyed_kill(byte: u8) -> Command {
    Command::KillResource {
        terminal_id: ResourceId::local(9),
        operation_id: phux_protocol::ids::IdempotencyKey::new([byte; 16]),
    }
}

/// Send `command` as `actor`'s keyed request: wire bytes and reply.
pub(super) fn send_keyed(
    session: &mut RelaySession,
    command: Command,
    actor: ClientId,
) -> (Option<Vec<u8>>, oneshot::Receiver<CommandResult>) {
    let (reply, rx) = oneshot::channel();
    let wire = session.handle_request_checked(RelayRequest::Keyed {
        command,
        actor,
        reply,
    });
    (wire, rx)
}

pub(super) fn forwarded(wire: Option<Vec<u8>>) -> Command {
    let FrameKind::Command { command, .. } = decode(&wire.expect("forwarded to the satellite"))
    else {
        panic!("expected COMMAND on the wire");
    };
    command
}

/// `GET_FILE`, `SERVICE_HTTP_*` and `CHECKOUT` (on a satellite Terminal,
/// with the id made local) reach only a satellite that advertises their
/// bit; an older one is refused `UNSUPPORTED_SATELLITE_ROUTE` and never
/// sees the unknown tag.
#[test]
fn hub_refuses_extended_commands_to_a_satellite_without_their_bit() {
    let request = phux_protocol::checkout::CheckoutRequest::Diff {
        path: "src/main.rs".to_owned(),
        area: phux_protocol::checkout::CheckoutArea::Staged,
    };
    let (target, checkout) = route_to_satellite(&Command::Checkout {
        terminal_id: ResourceId::satellite(host(), 9),
        request: request.clone(),
    })
    .expect("CHECKOUT routes to its satellite");
    assert_eq!(target, host());
    assert_eq!(
        checkout,
        Command::Checkout {
            terminal_id: ResourceId::local(9),
            request,
        }
    );
    let get_file = Command::GetFile {
        host: None,
        path: "/tmp/deck.pptx".to_owned(),
        offset: 0,
        max_bytes: 1,
    };
    let read = Command::ServiceHttpRead {
        exchange: 7,
        max_bytes: 1,
    };
    for (command, bit, name) in [
        (get_file, ServerFeatureExt::FileDownload, "FILE_DOWNLOAD"),
        (read, ServerFeatureExt::HostServiceHttp, "HOST_SERVICE_HTTP"),
        (checkout, ServerFeatureExt::Checkout, "CHECKOUT"),
    ] {
        let mut older = RelaySession::new(host(), BootstrapLimits::default());
        let (wire, mut rx) = send_command(&mut older, command.clone());
        assert!(wire.is_none(), "an older satellite never sees {command:?}");
        assert!(matches!(
            rx.try_recv().expect("typed refusal"),
            CommandResult::Error {
                code: ErrorCode::UnsupportedSatelliteRoute,
                ref message,
            } if message.contains(&format!("lacks {name}"))
        ));
        assert!(older.pending.is_empty());

        let mut newer = RelaySession::new(host(), BootstrapLimits::default());
        newer.set_satellite_features_ext(ServerFeatureExtSet::with(&[bit]));
        assert_eq!(
            forwarded(send_command(&mut newer, command.clone()).0),
            command
        );
    }
}

#[test]
fn hub_forwards_keyed_apply_input_to_a_satellite_that_advertises_the_bit() {
    let (target, command) = route_to_satellite(&apply_input_to(ResourceId::satellite(host(), 9)))
        .expect("APPLY_INPUT routes to its satellite");
    assert_eq!(target, host());
    assert_eq!(
        command,
        apply_input_to(ResourceId::local(9)),
        "the batch and its operation id cross verbatim"
    );
    let fence = crate::hub::operation_fence::OperationFence::default();
    let mut session = keyed_session(
        &[ServerFeature::AcknowledgedInput, ServerFeature::KeyedSignal],
        [1; 16],
        &fence,
    );
    let (wire, _rx) = send_keyed(&mut session, command.clone(), ClientId(4));
    assert_eq!(forwarded(wire), command);
    let (wire, _rx) = send_keyed(&mut session, keyed_kill(3), ClientId(4));
    assert_eq!(
        forwarded(wire),
        keyed_kill(3),
        "a keyed kill crosses with its key; the satellite dedupes it"
    );
}

#[test]
fn hub_refuses_keyed_ops_to_a_satellite_without_the_bit_with_unsupported_satellite_route() {
    let fence = crate::hub::operation_fence::OperationFence::default();
    let mut older = keyed_session(&[], [1; 16], &fence);
    for command in [apply_input_to(ResourceId::local(9)), keyed_kill(3)] {
        let (wire, mut rx) = send_keyed(&mut older, command, ClientId(4));
        assert!(
            wire.is_none(),
            "an older satellite never sees the operation"
        );
        assert!(matches!(
            rx.try_recv().expect("typed refusal"),
            CommandResult::Error {
                code: ErrorCode::UnsupportedSatelliteRoute,
                ..
            }
        ));
    }
    assert!(older.pending.is_empty());
    let key = phux_protocol::ids::IdempotencyKey::new([3; 16]).expect("key");
    assert_eq!(
        fence.event_operation(&key, 9),
        crate::hub::operation_fence::EventOperation::Unknown,
        "a refused operation leaves nothing in the fence"
    );
    let unkeyed = Command::KillResource {
        terminal_id: ResourceId::local(9),
        operation_id: None,
    };
    assert!(
        send_command(&mut older, unkeyed).0.is_some(),
        "an unkeyed kill still relays to an older satellite"
    );
}

#[test]
fn satellite_restart_between_attempts_yields_incarnation_changed_and_no_forward() {
    let fence = crate::hub::operation_fence::OperationFence::default();
    let features = [ServerFeature::AcknowledgedInput, ServerFeature::KeyedSignal];
    let mut before = keyed_session(&features, [1; 16], &fence);
    let (wire, _rx) = send_keyed(&mut before, keyed_kill(5), ClientId(4));
    assert!(wire.is_some(), "the first attempt reaches the satellite");
    let (wire, _rx) = send_keyed(
        &mut before,
        apply_input_to(ResourceId::local(9)),
        ClientId(4),
    );
    assert!(wire.is_some());

    let mut after = keyed_session(&features, [2; 16], &fence);
    for retry in [keyed_kill(5), apply_input_to(ResourceId::local(9))] {
        let (wire, mut rx) = send_keyed(&mut after, retry, ClientId(4));
        assert!(
            wire.is_none(),
            "a retry across the restart is not forwarded"
        );
        assert!(matches!(
            rx.try_recv().expect("typed refusal"),
            CommandResult::Error {
                code: ErrorCode::IncarnationChanged,
                ..
            }
        ));
    }
    assert!(after.pending.is_empty());
    let (wire, _rx) = send_keyed(&mut after, keyed_kill(6), ClientId(4));
    assert!(wire.is_some(), "a new id is new to the restarted satellite");
}

/// A hub relays `MIRROR` to the satellite its host names, with the host
/// taken out so the satellite answers for its own screens.
#[test]
fn hub_relays_a_host_named_mirror_without_its_host() {
    let mut command = Command::Mirror {
        host: Some("powerhouse-1".to_owned()),
        op: phux_protocol::wire::frame::MirrorOp::Start {
            target: "display:DP-1".to_owned(),
            jpeg: true,
        },
    };
    let named = crate::runtime::commands::take_named_host(&mut command);
    assert_eq!(named.as_deref(), Some("powerhouse-1"));
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    session.set_satellite_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Mirrors]));
    assert!(matches!(
        forwarded(send_command(&mut session, command).0),
        Command::Mirror { host: None, op: phux_protocol::wire::frame::MirrorOp::Start { ref target, jpeg: true } }
            if target == "display:DP-1"
    ));
}
