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
