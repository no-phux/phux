//! A newer 1.x server's additions, fed as raw bytes: an unknown frame type
//! or enum value is skipped without dropping the connection, and a
//! correlated reply this build cannot fully read still settles its request.

use bytes::Bytes;
use phux_protocol::wire::frame::{
    TYPE_COMMAND_RESULT, TYPE_METADATA_CHANGED, TYPE_RESOURCE_SPAWNED,
};

use super::*;

/// A frame of `type_byte` with the given `(field id, value)` TLV fields.
fn raw(type_byte: u8, fields: &[(u8, &[u8])]) -> Vec<u8> {
    let mut body = vec![type_byte];
    for (id, value) in fields {
        // Every id and length here is under 128: one varint byte each.
        body.extend([*id, 2, u8::try_from(value.len()).unwrap()]);
        body.extend_from_slice(value);
    }
    let mut frame = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    frame.extend(body);
    frame
}

fn wire(frame: &FrameKind) -> Vec<u8> {
    let mut bytes = bytes::BytesMut::new();
    frame.encode(&mut bytes);
    bytes.to_vec()
}

fn bells(plane: &mut ControlPlane) -> usize {
    plane
        .take_events()
        .iter()
        .filter(|event| matches!(event, Event::Bell { .. }))
        .count()
}

#[test]
fn unknown_frame_kinds_and_enum_values_are_skipped_on_every_feed_path() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"home");
    let _ = plane.take_events();
    let unknown_kind = raw(0x6E, &[]);
    let unknown_scope = raw(TYPE_METADATA_CHANGED, &[(1, &[0xFE]), (2, b"key")]);
    let bell = wire(&FrameKind::Bell {
        terminal_id: terminal(),
    });

    plane.feed_bytes(&unknown_kind).expect("unknown frame kind");
    plane.feed_bytes(&unknown_scope).expect("unknown scope tag");
    plane
        .feed_bytes_batch(&[unknown_kind.clone(), bell.clone()])
        .expect("batch with an unknown frame");
    assert_eq!(
        bells(&mut plane),
        1,
        "the frame after the skipped one lands"
    );
    plane
        .feed_shared_batch(&[Bytes::from(unknown_scope), Bytes::from(bell)])
        .expect("shared batch with an unknown frame");
    assert_eq!(bells(&mut plane), 1);
    assert_eq!(plane.status(), Status::Attached);

    // Malformed bytes still end the connection.
    assert!(matches!(
        plane.feed_bytes(&raw(TYPE_RESOURCE_SPAWNED, &[(1, &[0, 0])])),
        Err(ControlError::Protocol(_))
    ));
}

#[test]
fn an_unknown_spawn_refusal_settles_the_spawn() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"home");
    let _ = plane.take_outbound();
    let _ = plane.take_events();
    let request_id = plane.spawn_terminal(SpawnRequest::default());
    let _ = plane.take_outbound();
    plane
        .feed_bytes(&raw(
            TYPE_RESOURCE_SPAWNED,
            &[(1, &request_id.to_be_bytes()), (2, &[0x01, 0xFE])],
        ))
        .expect("an unknown spawn refusal decodes");
    assert!(plane.take_events().iter().any(|event| matches!(
        event,
        Event::TerminalSpawned { request_id: id, terminal_id: None, error: Some(_) }
            if *id == request_id
    )));
}

fn acknowledged_plane() -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    plane.take_outbound();
    plane
        .feed(hello_ok_with(&[ServerFeature::AcknowledgedInput]))
        .unwrap();
    plane.take_outbound();
    plane
}

fn apply_input_request(plane: &mut ControlPlane) -> (u64, u32) {
    let delivery = plane.apply_line(&terminal(), "command");
    let request = plane
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Command {
                request_id,
                command: Command::ApplyInput { .. },
            } => Some(request_id),
            _ => None,
        })
        .expect("APPLY_INPUT attempt");
    (delivery, request)
}

fn outcome(plane: &mut ControlPlane, delivery: u64) -> DeliveryOutcome {
    plane
        .take_events()
        .into_iter()
        .find_map(|event| match event {
            Event::InputDelivery {
                delivery_id,
                outcome,
                ..
            } if delivery_id == delivery => Some(outcome),
            _ => None,
        })
        .expect("the delivery settles")
}

/// The server answered with a value this build cannot read, so the input
/// may have landed: unknown, never a safe-to-retype refusal.
#[test]
fn an_unreadable_input_reply_settles_the_delivery_as_unknown() {
    let mut plane = acknowledged_plane();
    let (delivery, request) = apply_input_request(&mut plane);
    plane
        .feed_bytes(&raw(
            TYPE_COMMAND_RESULT,
            &[(1, &request.to_be_bytes()), (2, &[0x01, 0xEE, 1, 2])],
        ))
        .expect("an unknown value tag settles the request");
    assert_eq!(outcome(&mut plane, delivery), DeliveryOutcome::Unknown);
    assert!(plane.delivery_fenced(&terminal()));
}

/// `UNSUPPORTED` promises nothing happened, so a first attempt is refused.
#[test]
fn an_unsupported_input_reply_refuses_the_delivery() {
    let mut plane = acknowledged_plane();
    let (delivery, request) = apply_input_request(&mut plane);
    plane
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::Error {
                code: ErrorCode::Unsupported,
                message: "unknown enum value".into(),
            },
        })
        .unwrap();
    assert_eq!(outcome(&mut plane, delivery), DeliveryOutcome::Refused);
    assert!(!plane.delivery_fenced(&terminal()));
}

/// A spawn reply whose id has a shape this build does not know settles the
/// spawn as failed instead of leaving it pending.
#[test]
fn an_unreadable_spawn_reply_settles_the_spawn() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"home");
    let _ = plane.take_outbound();
    let _ = plane.take_events();
    let request_id = plane.spawn_terminal(SpawnRequest::default());
    let _ = plane.take_outbound();
    plane
        .feed_bytes(&raw(
            TYPE_RESOURCE_SPAWNED,
            &[(1, &request_id.to_be_bytes()), (2, &[0x00, 0xFF, 1])],
        ))
        .expect("an unreadable reply settles, not fails");
    assert!(plane.take_events().iter().any(|event| matches!(
        event,
        Event::TerminalSpawned { request_id: id, terminal_id: None, error: Some(_) }
            if *id == request_id
    )));
    assert_eq!(plane.status(), Status::Attached);
}

fn unsupported(request_id: u32) -> FrameKind {
    FrameKind::Error {
        request_id: Some(request_id),
        code: ErrorCode::Unsupported,
        message: "a newer server does not know this".into(),
    }
}

/// A newer server refuses a request frame it cannot read with a correlated
/// `ERROR UNSUPPORTED`: the spawn settles as refused, the session stays.
#[test]
fn a_spawn_answered_unsupported_settles_as_refused() {
    let (mut plane, attach_id) = negotiated();
    attach(&mut plane, attach_id, b"home");
    let _ = plane.take_outbound();
    let _ = plane.take_events();
    let request_id = plane.spawn_terminal(SpawnRequest::default());
    let _ = plane.take_outbound();
    plane
        .feed(unsupported(request_id))
        .expect("a correlated UNSUPPORTED is not fatal");
    assert!(plane.take_events().iter().any(|event| matches!(
        event,
        Event::TerminalSpawned { request_id: id, terminal_id: None, error: Some(_) }
            if *id == request_id
    )));
    assert_eq!(plane.status(), Status::Attached);
}

#[test]
fn a_directory_listing_answered_unsupported_settles_as_failed() {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane
        .feed(hello_ok_with(&[ServerFeature::ListDirectory]))
        .unwrap();
    let _ = plane.take_outbound();
    let request_id = plane.list_directory("/home".into());
    assert_ne!(request_id, 0);
    plane
        .feed(unsupported(request_id))
        .expect("a correlated UNSUPPORTED is not fatal");
    let listings = plane.take_directory_listings();
    assert_eq!(listings.len(), 1);
    assert_eq!(listings[0].request_id, request_id);
    assert_eq!(listings[0].path, "/home");
    assert!(listings[0].error.is_some());
}

/// A correlated `ERROR` for an acknowledged input settles it in the input
/// journal, so the terminal's next input goes out instead of waiting out
/// the retry horizon.
#[test]
fn a_correlated_error_settles_acknowledged_input() {
    for (code, expected) in [
        (ErrorCode::Unsupported, DeliveryOutcome::Refused),
        (ErrorCode::UNREADABLE_REPLY, DeliveryOutcome::Unknown),
    ] {
        let mut plane = acknowledged_plane();
        let (delivery, request) = apply_input_request(&mut plane);
        plane
            .feed(FrameKind::Error {
                request_id: Some(request),
                code,
                message: "a newer server".into(),
            })
            .expect("a correlated error is not fatal");
        assert_eq!(outcome(&mut plane, delivery), expected, "{code:?}");
        if expected == DeliveryOutcome::Refused {
            let (_, next) = apply_input_request(&mut plane);
            assert_ne!(next, request, "the terminal accepts its next input");
        }
    }
}

fn reads_topology(plane: &mut ControlPlane) -> bool {
    plane.take_outbound().iter().any(|bytes| {
        matches!(
            decode(bytes),
            FrameKind::Command {
                command: Command::GetState { .. },
                ..
            }
        )
    })
}

/// An unreadable spawn answer leaves the outcome unknown: the spawn settles
/// saying so and the topology is re-read to list a pane that may exist. A
/// definite refusal admitted nothing and reads nothing.
#[test]
fn an_unreadable_spawn_answer_is_an_unknown_outcome_not_a_refusal() {
    for (frame, uncertain) in [
        (unsupported(0), false),
        (
            FrameKind::Error {
                request_id: Some(0),
                code: ErrorCode::UNREADABLE_REPLY,
                message: "unreadable".into(),
            },
            true,
        ),
        (
            FrameKind::ResourceSpawned {
                request_id: 0,
                result: SpawnResult::Err(phux_protocol::wire::frame::SpawnError::Unknown(0xFE)),
            },
            true,
        ),
    ] {
        let (mut plane, attach_id) = negotiated();
        attach(&mut plane, attach_id, b"home");
        let _ = plane.take_outbound();
        let _ = plane.take_events();
        let request_id = plane.spawn_terminal(SpawnRequest::default());
        let _ = plane.take_outbound();
        let frame = match frame {
            FrameKind::Error { code, message, .. } => FrameKind::Error {
                request_id: Some(request_id),
                code,
                message,
            },
            FrameKind::ResourceSpawned { result, .. } => {
                FrameKind::ResourceSpawned { request_id, result }
            }
            other => other,
        };
        plane.feed(frame).expect("settles");
        let events = plane.take_events();
        let error = events
            .iter()
            .find_map(|event| match event {
                Event::TerminalSpawned {
                    request_id: id,
                    terminal_id: None,
                    error: Some(error),
                } if *id == request_id => Some(error.clone()),
                _ => None,
            })
            .expect("the spawn settles");
        assert_eq!(error.contains("outcome is unknown"), uncertain, "{error}");
        assert_eq!(reads_topology(&mut plane), uncertain, "{error}");
    }
}

/// An `ATTACHED` this build cannot read (here a focused resource of an
/// unknown id shape) ends the connection visibly: skipping it would leave
/// the attach waiting forever.
#[test]
fn an_unreadable_attached_fails_the_attach_instead_of_hanging() {
    let (mut plane, attach_id) = negotiated();
    let marker = 0x0BAD_F00D_u32;
    let snapshot = SessionSnapshot::new(
        SessionId::new(1),
        WindowId::new(1),
        ResourceId::local(marker),
    );
    let mut bytes = wire(&FrameKind::Attached {
        attach_id,
        snapshot,
        initial_client_id: ClientId::new(2),
    });
    let id = marker.to_be_bytes();
    let at = bytes
        .windows(id.len())
        .position(|window| window == id)
        .expect("the focused resource id");
    assert_eq!(bytes[at - 1], 0x00, "local id tag");
    bytes[at - 1] = 0xEE;
    assert!(matches!(
        plane.feed_bytes(&bytes),
        Err(ControlError::Protocol(_))
    ));
}
