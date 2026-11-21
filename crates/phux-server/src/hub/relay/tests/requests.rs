//! Request ids, correlated errors, and relayed spawns.

use super::*;

#[test]
fn session_remaps_request_ids_and_resolves_replies() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (reply_a, mut rx_a) = oneshot::channel();
    let (reply_b, mut rx_b) = oneshot::channel();

    let wire_a = session.handle_request(RelayRequest::Command {
        command: Command::GetState {
            scope: StateScope::Server,
        },
        reply: reply_a,
        subscribe: None,
    });
    let wire_b = session.handle_request(RelayRequest::Command {
        command: Command::Upgrade,
        reply: reply_b,
        subscribe: None,
    });

    let FrameKind::Command {
        request_id: id_a, ..
    } = decode(&wire_a)
    else {
        panic!("expected COMMAND on the wire");
    };
    let FrameKind::Command {
        request_id: id_b, ..
    } = decode(&wire_b)
    else {
        panic!("expected COMMAND on the wire");
    };
    assert_ne!(id_a, id_b, "link-side request ids must be distinct");

    // Resolve out of order: the remap must correlate, not FIFO.
    session.feed(&FrameKind::CommandResult {
        request_id: id_b,
        result: CommandResult::OkWith(CommandValue::Json("b".to_owned())),
    });
    session.feed(&FrameKind::CommandResult {
        request_id: id_a,
        result: CommandResult::Ok,
    });

    assert_eq!(rx_a.try_recv().expect("a resolved"), CommandResult::Ok);
    assert_eq!(
        rx_b.try_recv().expect("b resolved"),
        CommandResult::OkWith(CommandValue::Json("b".to_owned()))
    );
}

#[test]
fn session_maps_correlated_error_frames_to_command_errors() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (reply, mut rx) = oneshot::channel();
    let wire = session.handle_request(RelayRequest::Command {
        command: Command::Upgrade,
        reply,
        subscribe: None,
    });
    let FrameKind::Command { request_id, .. } = decode(&wire) else {
        panic!("expected COMMAND");
    };
    session.feed(&FrameKind::Error {
        request_id: Some(request_id),
        code: ErrorCode::TerminalNotFound,
        message: "nope".to_owned(),
    });
    assert_eq!(
        rx.try_recv().expect("resolved"),
        CommandResult::error(ErrorCode::TerminalNotFound, "nope")
    );
}

// --- session: spawn relay (phux-v45.6) --------------------------------

pub(super) fn spawn_request(reply: oneshot::Sender<SpawnResult>) -> RelayRequest {
    RelayRequest::Spawn {
        spawn: SatelliteSpawn {
            group: GroupId::new(1),
            command: None,
            cwd: None,
            env: None,
            term: None,
            owner_terminal: None,
            initial_size: None,
            resource: None,
        },
        reply,
    }
}

/// [`spawn_request`] carrying `resource`.
pub(super) fn spawn_with(
    reply: oneshot::Sender<SpawnResult>,
    resource: phux_protocol::wire::frame::SpawnResource,
) -> RelayRequest {
    let mut request = spawn_request(reply);
    if let RelayRequest::Spawn { spawn, .. } = &mut request {
        spawn.resource = Some(Box::new(resource));
    }
    request
}

/// Relay `request` (a spawn) and answer it from the satellite with
/// `answer`: the `SPAWN_RESOURCE` that crossed the link, and the reply.
fn spawn_answered(
    session: &mut RelaySession,
    request: impl FnOnce(oneshot::Sender<SpawnResult>) -> RelayRequest,
    answer: SpawnResult,
) -> (FrameKind, SpawnResult) {
    let (reply, mut rx) = oneshot::channel();
    let wire = decode(&session.handle_request(request(reply)));
    let FrameKind::SpawnResource { request_id, .. } = &wire else {
        panic!("expected SPAWN_RESOURCE on the wire, got {wire:?}");
    };
    session.feed(&FrameKind::ResourceSpawned {
        request_id: *request_id,
        result: answer,
    });
    (wire, rx.try_recv().expect("spawn resolved"))
}

#[test]
fn session_rejects_chained_ids_and_relays_spawn_errors_verbatim() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    // A Satellite-tagged id in the satellite's own reply never chains.
    let (_, chained) = spawn_answered(
        &mut session,
        spawn_request,
        SpawnResult::Ok(ResourceId::satellite("nested", 7)),
    );
    assert!(matches!(
        chained,
        SpawnResult::Err(SpawnError::SpawnFailed(_))
    ));
    // A typed satellite-side error relays verbatim.
    let (_, typed) = spawn_answered(
        &mut session,
        spawn_request,
        SpawnResult::Err(SpawnError::GroupNotFound),
    );
    assert_eq!(typed, SpawnResult::Err(SpawnError::GroupNotFound));
}

/// ADR-0109: the instance token comes back beside the re-tagged id.
#[test]
fn session_keeps_a_bound_spawn_token_while_retagging_the_id() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let instance = phux_protocol::ids::ServerInstance::new([3; 16]);
    let (wire, reply) = spawn_answered(
        &mut session,
        |reply| {
            spawn_with(
                reply,
                phux_protocol::wire::frame::SpawnResource::default().with_bind_instance(true),
            )
        },
        SpawnResult::OkBound {
            id: ResourceId::local(42),
            instance,
        },
    );
    assert!(
        matches!(wire, FrameKind::SpawnResource { resource: Some(r), .. } if r.bind_instance),
        "the bind request crosses the link"
    );
    assert_eq!(
        reply,
        SpawnResult::OkBound {
            id: ResourceId::satellite("devbox", 42),
            instance,
        }
    );
}

/// ADR-0126: a replayed reply stays replayed through the hub.
#[test]
fn session_keeps_a_replayed_spawn_replayed_while_retagging_the_id() {
    let key = phux_protocol::ids::IdempotencyKey::new([7; 16]);
    let instance = phux_protocol::ids::ServerInstance::new([3; 16]);
    for original in [None, Some(instance)] {
        let mut session = negotiated(&[ServerFeature::SpawnIdempotency]);
        let (wire, reply) = spawn_answered(
            &mut session,
            |reply| {
                spawn_with(
                    reply,
                    phux_protocol::wire::frame::SpawnResource::default().with_idempotency_key(key),
                )
            },
            SpawnResult::Replayed {
                id: ResourceId::local(42),
                instance: original,
            },
        );
        let FrameKind::SpawnResource { resource, .. } = wire else {
            unreachable!("spawn_answered checked the frame");
        };
        assert_eq!(
            resource.and_then(|r| r.idempotency_key),
            key,
            "{original:?}: the key crosses the link"
        );
        assert_eq!(
            reply,
            SpawnResult::Replayed {
                id: ResourceId::satellite("devbox", 42),
                instance: original,
            },
            "{original:?}"
        );
    }
}
