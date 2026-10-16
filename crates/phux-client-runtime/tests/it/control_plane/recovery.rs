use super::*;

fn replacement_hello(incarnation: u8) -> FrameKind {
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk { server_id, .. } = &mut hello {
        server_id.fill(incarnation);
    }
    hello
}

fn reconnect_target(plane: &mut ControlPlane, incarnation: u8) -> AttachTarget {
    plane.connection_lost(None);
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(replacement_hello(incarnation)).unwrap();
    plane
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Attach { target, .. } => Some(target),
            _ => None,
        })
        .expect("reconnect must request the selected session")
}

#[test]
fn consumer_validated_sessions_browse_before_replaying_any_terminal_on_reconnect() {
    let mut plane = ControlPlane::new(ControlOptions {
        session_recovery: phux_client_runtime::control::SessionRecovery::CallerValidated,
        auto_attach_foreign_spawns: false,
        subscribe_agent_sessions: false,
        ..ControlOptions::default()
    });
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let initial: Vec<_> = plane
        .take_outbound()
        .iter()
        .map(|bytes| decode(bytes))
        .collect();
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
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(two_session_snapshot(false))),
        })
        .unwrap();
    let _ = plane.take_outbound();
    assert!(!plane.attach_session(AttachTarget::ById(SessionId::new(1))));
    let _ = plane.take_outbound();
    plane.attach_terminal_preserving_geometry(&ResourceId::local(1));
    let _ = plane.take_outbound();
    for incarnation in [0xAB, 0xCD] {
        plane.connection_lost(None);
        plane.connection_opened();
        let _ = plane.take_outbound();
        plane.feed(replacement_hello(incarnation)).unwrap();
        let frames: Vec<_> = plane
            .take_outbound()
            .iter()
            .map(|bytes| decode(bytes))
            .collect();
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
    assert_eq!(
        reconnect_target(&mut plane, 0xAB),
        AttachTarget::ById(SessionId::new(1))
    );
    assert_eq!(
        reconnect_target(&mut plane, 0xCD),
        AttachTarget::ByName("renamed".to_owned())
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
    assert_eq!(
        reconnect_target(&mut plane, 0xAB),
        AttachTarget::ById(SessionId::new(2))
    );
    assert_eq!(
        reconnect_target(&mut plane, 0xCD),
        AttachTarget::ByName("renamed-beta".to_owned())
    );
}

#[test]
fn selected_session_numbers_are_scoped_to_the_server_incarnation() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"original");
    assert!(!plane.attach_session(AttachTarget::ById(SessionId::new(1))));

    // A dropped socket does not change a session's identity, even if its name
    // changes. A different server may have reused that number for other work.
    assert_eq!(
        reconnect_target(&mut plane, 0xAB),
        AttachTarget::ById(SessionId::new(1))
    );
    assert_eq!(
        reconnect_target(&mut plane, 0xCD),
        AttachTarget::ByName("main".to_owned())
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
