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
