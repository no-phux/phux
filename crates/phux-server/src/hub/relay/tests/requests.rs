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
    session
        .handle_inbound(&encode(&FrameKind::CommandResult {
            request_id: id_b,
            result: CommandResult::OkWith(CommandValue::Json("b".to_owned())),
        }))
        .expect("valid satellite frame");
    session
        .handle_inbound(&encode(&FrameKind::CommandResult {
            request_id: id_a,
            result: CommandResult::Ok,
        }))
        .expect("valid satellite frame");

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
    session
        .handle_inbound(&encode(&FrameKind::Error {
            request_id: Some(request_id),
            code: ErrorCode::TerminalNotFound,
            message: "nope".to_owned(),
        }))
        .expect("valid satellite frame");
    assert_eq!(
        rx.try_recv().expect("resolved"),
        CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: "nope".to_owned(),
        }
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

#[test]
fn session_relays_spawn_with_stripped_addressing_and_retags_the_reply() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (reply, mut rx) = oneshot::channel();
    let wire = session.handle_request(spawn_request(reply));
    let FrameKind::SpawnResource {
        request_id,
        satellite,
        ..
    } = decode(&wire)
    else {
        panic!("expected SPAWN_RESOURCE on the wire");
    };
    assert_eq!(
        satellite, None,
        "the addressing field never crosses the link (no chaining)"
    );
    session
        .handle_inbound(&encode(&FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Ok(ResourceId::local(42)),
        }))
        .expect("valid satellite frame");
    assert_eq!(
        rx.try_recv().expect("spawn resolved"),
        SpawnResult::Ok(ResourceId::satellite("devbox", 42))
    );
}

#[test]
fn session_rejects_chained_ids_and_relays_spawn_errors_verbatim() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    // A Satellite-tagged id in the satellite's own reply never chains.
    let (reply, mut rx) = oneshot::channel();
    let wire = session.handle_request(spawn_request(reply));
    let FrameKind::SpawnResource { request_id, .. } = decode(&wire) else {
        panic!("expected SPAWN_RESOURCE");
    };
    session
        .handle_inbound(&encode(&FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Ok(ResourceId::satellite("nested", 7)),
        }))
        .expect("valid satellite frame");
    assert!(matches!(
        rx.try_recv().expect("resolved"),
        SpawnResult::Err(SpawnError::SpawnFailed(_))
    ));
    // A typed satellite-side error relays verbatim.
    let (reply, mut rx) = oneshot::channel();
    let wire = session.handle_request(spawn_request(reply));
    let FrameKind::SpawnResource { request_id, .. } = decode(&wire) else {
        panic!("expected SPAWN_RESOURCE");
    };
    session
        .handle_inbound(&encode(&FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Err(SpawnError::GroupNotFound),
        }))
        .expect("valid satellite frame");
    assert_eq!(
        rx.try_recv().expect("resolved"),
        SpawnResult::Err(SpawnError::GroupNotFound)
    );
}

/// ADR-0109: the instance token comes back beside the re-tagged id.
#[test]
fn session_keeps_a_bound_spawn_token_while_retagging_the_id() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (reply, mut rx) = oneshot::channel();
    let mut request = spawn_request(reply);
    if let RelayRequest::Spawn { spawn, .. } = &mut request {
        spawn.resource = Some(Box::new(
            phux_protocol::wire::frame::SpawnResource::default().with_bind_instance(true),
        ));
    }
    let wire = session.handle_request(request);
    let FrameKind::SpawnResource {
        request_id,
        resource,
        ..
    } = decode(&wire)
    else {
        panic!("expected SPAWN_RESOURCE on the wire");
    };
    assert!(
        resource.is_some_and(|r| r.bind_instance),
        "the bind request crosses the link"
    );
    let instance = phux_protocol::ids::ServerInstance::new([3; 16]);
    session
        .handle_inbound(&encode(&FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::OkBound {
                id: ResourceId::local(42),
                instance,
            },
        }))
        .expect("valid satellite frame");
    assert_eq!(
        rx.try_recv().expect("spawn resolved"),
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
        let mut session = RelaySession::new_negotiated(
            host(),
            BootstrapLimits::default(),
            BootstrapProfile::SynthesizedVtRaw,
            ServerFeatureSet::with(&[ServerFeature::SpawnIdempotency]),
        );
        let (reply, mut rx) = oneshot::channel();
        let mut request = spawn_request(reply);
        if let RelayRequest::Spawn { spawn, .. } = &mut request {
            spawn.resource = Some(Box::new(
                phux_protocol::wire::frame::SpawnResource::default().with_idempotency_key(key),
            ));
        }
        let wire = session.handle_request(request);
        let FrameKind::SpawnResource {
            request_id,
            resource,
            ..
        } = decode(&wire)
        else {
            panic!("expected SPAWN_RESOURCE on the wire");
        };
        assert_eq!(
            resource.and_then(|r| r.idempotency_key),
            key,
            "the key crosses the link"
        );
        session
            .handle_inbound(&encode(&FrameKind::ResourceSpawned {
                request_id,
                result: SpawnResult::Replayed {
                    id: ResourceId::local(42),
                    instance: original,
                },
            }))
            .expect("valid satellite frame");
        assert_eq!(
            rx.try_recv().expect("spawn resolved"),
            SpawnResult::Replayed {
                id: ResourceId::satellite("devbox", 42),
                instance: original,
            }
        );
    }
}
