use super::*;

fn sends_no_spawn(plane: &mut ControlPlane) -> bool {
    plane
        .take_outbound()
        .iter()
        .all(|bytes| !matches!(decode(bytes), FrameKind::SpawnResource { .. }))
}

#[test]
fn interactive_setup_spawn_is_not_replayed_even_when_server_supports_idempotency() {
    let mut plane = idempotent_home();
    let id = plane.spawn_terminal_once(SpawnRequest {
        command: Some(vec!["opencode".into(), "auth".into(), "login".into()]),
        ..SpawnRequest::default()
    });
    let sent = plane.take_outbound();
    assert!(matches!(
        decode(&sent[0]),
        FrameKind::SpawnResource { resource: None, .. }
    ));
    plane.connection_lost(Some("setup receipt lost".into()));
    plane.connection_opened();
    assert!(plane.take_events().iter().any(|event| matches!(event,
        Event::TerminalSpawned { request_id, terminal_id: None, error: Some(_) } if *request_id == id
    )));
    let _ = plane.take_outbound();
    plane
        .feed(hello_ok_with(&[ServerFeature::SpawnIdempotency]))
        .unwrap();
    finish_reconnected_attach(&mut plane);
    assert!(sends_no_spawn(&mut plane));
}

fn inventory_read(plane: &mut ControlPlane) -> u32 {
    state_read(&outbound(plane)).expect("topology read")
}

/// An empty session emits no terminal-close event to refresh the inventory.
#[test]
fn session_deletion_refreshes_inventory_and_fences_an_older_read() {
    use phux_protocol::ids::{Revision, SessionKey};
    use phux_protocol::wire::frame::AgentEvent;

    for read_in_flight in [false, true] {
        let mut plane = opened(ControlOptions::default(), hello_ok(PROTOCOL_VERSION.patch));
        let initial = inventory_read(&mut plane);
        let mut before = snapshot();
        before
            .sessions
            .push(SessionInfo::new(SessionId::new(2), "empty"));
        answer_state(&mut plane, initial, before.clone());
        let _ = plane.take_outbound();
        let _ = plane.take_events();
        assert!(plane.topology().unwrap().session_named("empty").is_some());
        let old_read = read_in_flight.then(|| plane.refresh_topology().unwrap());
        let _ = plane.take_outbound();
        plane
            .feed(FrameKind::Event {
                terminal: None,
                stamp: None,
                event: AgentEvent::SessionDeleted {
                    session: SessionKey::new([2; 16]).unwrap(),
                    name: "empty".into(),
                    revision: Revision::new(2),
                },
            })
            .unwrap();
        if let Some(old_read) = old_read {
            assert!(plane.take_outbound().is_empty(), "coalesce in-flight reads");
            answer_state(&mut plane, old_read, before);
        }
        let refresh = inventory_read(&mut plane);
        answer_state(&mut plane, refresh, snapshot());
        assert!(plane.topology().unwrap().session_named("empty").is_none());
        assert!(
            plane
                .take_events()
                .iter()
                .any(|event| matches!(event, Event::TopologyChanged))
        );
    }
}

fn replacement_hello(incarnation: u8) -> FrameKind {
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk { server_id, .. } = &mut hello {
        server_id.fill(incarnation);
    }
    hello
}

#[test]
fn keyed_spawn_never_replays_to_a_different_server_incarnation() {
    let mut plane = idempotent_home();
    let request_id = plane.spawn_terminal(SpawnRequest::default());
    let _ = plane.take_outbound();
    plane.connection_lost(None);
    plane.connection_opened();
    let _ = plane.take_outbound();
    let mut hello = hello_ok_with(&[ServerFeature::SpawnIdempotency]);
    if let FrameKind::HelloOk { server_id, .. } = &mut hello {
        server_id.fill(0xCD);
    }
    plane.feed(hello).unwrap();
    finish_reconnected_attach(&mut plane);
    assert!(sends_no_spawn(&mut plane));
    assert!(plane.take_events().iter().any(|event| matches!(event,
        Event::TerminalSpawned { request_id: id, terminal_id: None, error: Some(message) }
            if *id == request_id && message.contains("server restarted")
    )));
}

fn reconnect_target(plane: &mut ControlPlane, incarnation: u8) -> AttachTarget {
    plane.connection_lost(None);
    reopen(plane, replacement_hello(incarnation));
    plane
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Attach { target, .. } => Some(target),
            _ => None,
        })
        .expect("reconnect must request the selected session")
}

/// The session a reconnect asks for: `same` from the same server
/// incarnation, `other` from a different one.
fn assert_reconnect_targets(plane: &mut ControlPlane, same: &AttachTarget, other: &AttachTarget) {
    assert_eq!(&reconnect_target(plane, 0xAB), same, "same incarnation");
    assert_eq!(&reconnect_target(plane, 0xCD), other, "new incarnation");
}

#[test]
fn consumer_validated_sessions_browse_before_replaying_any_terminal_on_reconnect() {
    let mut plane = ControlPlane::new(ControlOptions {
        session_recovery: phux_client_runtime::control::SessionRecovery::CallerValidated,
        auto_attach_foreign_spawns: false,
        subscribe_agent_sessions: false,
        ..ControlOptions::default()
    });
    reopen(&mut plane, hello_ok(PROTOCOL_VERSION.patch));
    let initial = outbound(&mut plane);
    let request_id = initial
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::GetState { .. },
            } => Some(*request_id),
            _ => None,
        })
        .expect("inventory before selecting work");
    answer_state(&mut plane, request_id, two_session_snapshot(false));
    let _ = plane.take_outbound();
    assert!(!plane.attach_session(AttachTarget::ById(SessionId::new(1))));
    let _ = plane.take_outbound();
    plane.attach_terminal_preserving_geometry(&ResourceId::local(1));
    let _ = plane.take_outbound();
    for incarnation in [0xAB, 0xCD] {
        plane.connection_lost(None);
        reopen(&mut plane, replacement_hello(incarnation));
        let frames = outbound(&mut plane);
        assert!(frames.iter().any(|frame| matches!(
            frame,
            FrameKind::Command {
                command: Command::GetState { .. },
                ..
            }
        )));
        assert!(
            frames.iter().all(|frame| !matches!(
                frame,
                FrameKind::Attach { .. }
                    | FrameKind::Command {
                        command: Command::AttachResource { .. },
                        ..
                    }
            )),
            "unvalidated replay: {frames:?}"
        );
        assert!(plane.selected_session().is_none());
    }
}

#[test]
fn named_attach_reconnect_follows_a_rename_not_the_reused_name() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"original");
    let mut snapshot = two_session_snapshot(false);
    snapshot.sessions[0].name = "renamed".to_owned();
    snapshot.sessions[1].name = "main".to_owned();
    refresh_to(&mut plane, snapshot);
    assert_reconnect_targets(
        &mut plane,
        &AttachTarget::ById(SessionId::new(1)),
        &AttachTarget::ByName("renamed".to_owned()),
    );
}

#[test]
fn a_live_named_switch_reconnects_to_its_renamed_identity() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"original");
    refresh_to(&mut plane, two_session_snapshot(false));
    assert!(!plane.attach_session(AttachTarget::ByName("beta".to_owned())));
    let mut snapshot = two_session_snapshot(false);
    snapshot.sessions[0].name = "beta".to_owned();
    snapshot.sessions[1].name = "renamed-beta".to_owned();
    refresh_to(&mut plane, snapshot);
    assert_reconnect_targets(
        &mut plane,
        &AttachTarget::ById(SessionId::new(2)),
        &AttachTarget::ByName("renamed-beta".to_owned()),
    );
}

#[test]
fn a_new_server_cannot_inherit_an_unresolved_numeric_session_target() {
    let mut plane = ControlPlane::new(ControlOptions {
        attach: Some(AttachTarget::ById(SessionId::new(7))),
        ..ControlOptions::default()
    });
    plane.connection_opened();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    // The first server disappeared before confirming what session 7 meant.
    plane.connection_lost(None);
    plane.connection_opened();
    let _ = plane.take_outbound();
    assert!(matches!(
        plane.feed(replacement_hello(0xCD)),
        Err(ControlError::Refused(_))
    ));
    assert!(
        plane
            .take_outbound()
            .iter()
            .all(|bytes| !matches!(decode(bytes), FrameKind::Attach { .. }))
    );
}

#[test]
fn attach_refusals_end_the_attempt_but_unrelated_replies_do_not() {
    let (mut plane, attach_id) = negotiated();
    plane
        .feed(FrameKind::Error {
            request_id: Some(attach_id + 100),
            code: ErrorCode::SessionNotFound,
            message: "unrelated lookup failed".to_owned(),
        })
        .unwrap();
    assert_eq!(plane.status(), Status::Negotiated);
    // Command request IDs and attach IDs are distinct namespaces.
    plane
        .feed(FrameKind::Error {
            request_id: Some(attach_id),
            code: ErrorCode::SessionNotFound,
            message: "another command lookup failed".to_owned(),
        })
        .unwrap();
    assert_eq!(plane.status(), Status::Negotiated);

    // The server's session resolver sends this uncorrelated form.
    assert!(matches!(
        plane.feed(FrameKind::Error {
            request_id: None,
            code: ErrorCode::SessionNotFound,
            message: "selected session is gone".to_owned(),
        }),
        Err(ControlError::Refused(_))
    ));
}

#[test]
fn promoting_inventory_can_start_agent_streams_without_reconnecting() {
    let (mut plane, attach_id) = negotiated();
    let agent = ResourceId::local(31);
    let mut snapshot = two_session_snapshot(false);
    snapshot.resources.push(
        ResourceInfo::new(agent.clone(), WindowId::new(0), 0, 0)
            .with_kind(phux_protocol::ResourceKind::AgentSession)
            .with_parent(Some(terminal()))
            .with_agent(Some(phux_protocol::AgentFacet::new("claude", "working"))),
    );
    plane.attached(attach_id, snapshot.clone());
    let generation = u64::from(attach_id);
    empty_bootstrap(&mut plane, &terminal(), [generation, generation]);
    plane.feed(FrameKind::AttachReady { attach_id }).unwrap();
    assert!(plane.take_outbound().iter().all(|bytes| !matches!(
        decode(bytes),
        FrameKind::Command {
            command: Command::AttachResource { .. },
            ..
        }
    )));
    plane.watch_agent_sessions();
    let request_id = plane
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Command {
                request_id,
                command: Command::GetState { .. },
            } => Some(request_id),
            frame => {
                assert!(!matches!(
                    frame,
                    FrameKind::Attach { .. } | FrameKind::Hello { .. }
                ));
                None
            }
        })
        .unwrap();
    plane.watch_agent_sessions();
    assert!(plane.take_outbound().is_empty());
    answer_state(&mut plane, request_id, snapshot);
    let subscribed: Vec<_> = plane
        .take_outbound()
        .iter()
        .filter_map(|bytes| match decode(bytes) {
            FrameKind::Command {
                command: Command::AttachResource { terminal_id, .. },
                ..
            } => Some(terminal_id),
            _ => None,
        })
        .collect();
    assert_eq!(subscribed, vec![agent]);
}
