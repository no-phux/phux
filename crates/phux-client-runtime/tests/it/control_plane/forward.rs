use super::service_http::{ready, sent_request_id};
use super::*;

use phux_client_runtime::control::forward::ForwardError;
use phux_protocol::forward::{ForwardChunk, ForwardOp, MAX_FORWARD_CHUNK_BYTES};

fn plane_with_forward() -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    let _ = plane.take_outbound();
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk { server_caps, .. } = &mut hello {
        *server_caps = server_caps
            .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::ForwardTcp]));
    }
    plane.feed(hello).unwrap();
    let _ = plane.take_outbound();
    plane
}

fn answer(plane: &mut ControlPlane, request_id: u32, result: CommandResult) {
    plane
        .feed(FrameKind::CommandResult { request_id, result })
        .unwrap();
}

#[test]
fn a_server_without_the_bit_is_never_asked() {
    let mut plane = ControlPlane::new(ControlOptions::default());
    assert_eq!(
        plane.forward_open("localhost".to_owned(), 80).unwrap_err(),
        ForwardError::NotConnected
    );
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let _ = plane.take_outbound();
    assert!(!plane.forward_supported());
    assert_eq!(
        plane.forward_open("localhost".to_owned(), 80).unwrap_err(),
        ForwardError::Unsupported
    );
    assert!(plane.take_outbound().is_empty());
}

#[test]
fn open_read_write_and_close_resolve_their_calls() {
    let mut plane = plane_with_forward();
    assert!(plane.forward_supported());
    let open = plane.forward_open("localhost".to_owned(), 3000).unwrap();
    let (request_id, command) = sent_request_id(&mut plane);
    let Command::Forward { op } = command else {
        panic!("expected FORWARD_OPEN, got {command:?}");
    };
    let expected = ForwardOp::Open {
        addr: "localhost".to_owned(),
        port: 3000,
    };
    assert_eq!(op, expected);
    answer(
        &mut plane,
        request_id,
        CommandResult::OkWith(CommandValue::ForwardOpened(9)),
    );
    assert_eq!(ready(open).unwrap(), 9);

    // A read and a write are in flight together, answered in either order.
    let read = plane.forward_read(9, 1024).unwrap();
    let (read_id, _) = sent_request_id(&mut plane);
    let write = plane.forward_write(9, b"ping".to_vec(), true).unwrap();
    let (write_id, command) = sent_request_id(&mut plane);
    let sent = ForwardOp::Write {
        id: 9,
        bytes: b"ping".to_vec(),
        fin: true,
    };
    assert!(matches!(command, Command::Forward { op } if op == sent));
    answer(&mut plane, write_id, CommandResult::Ok);
    assert_eq!(ready(write), Ok(()));
    let chunk = ForwardChunk {
        bytes: b"pong".to_vec(),
        eof: true,
    };
    answer(
        &mut plane,
        read_id,
        CommandResult::OkWith(CommandValue::ForwardChunk(chunk.clone())),
    );
    assert_eq!(ready(read).unwrap(), chunk);

    let close = plane.forward_close(9).unwrap();
    let (request_id, command) = sent_request_id(&mut plane);
    assert!(matches!(
        command,
        Command::Forward {
            op: ForwardOp::Close { id: 9 }
        }
    ));
    let refusal = CommandResult::Error {
        code: ErrorCode::PermissionDenied,
        message: "not on loopback".to_owned(),
    };
    answer(&mut plane, request_id, refusal);
    assert_eq!(
        ready(close).unwrap_err(),
        ForwardError::Refused {
            code: ErrorCode::PermissionDenied,
            message: "not on loopback".to_owned(),
        }
    );
}

#[test]
fn an_oversized_call_sends_nothing_and_a_disconnect_resolves_the_rest() {
    let mut plane = plane_with_forward();
    let over = vec![0; MAX_FORWARD_CHUNK_BYTES + 1];
    assert_eq!(
        plane.forward_write(1, over, false).unwrap_err(),
        ForwardError::Invalid
    );
    assert!(plane.take_outbound().is_empty());

    let read = plane.forward_read(1, 1).unwrap();
    let write = plane.forward_write(1, vec![1], false).unwrap();
    plane.connection_lost(None);
    assert_eq!(ready(read).unwrap_err(), ForwardError::Disconnected);
    assert_eq!(ready(write).unwrap_err(), ForwardError::Disconnected);
    assert_eq!(
        plane.forward_read(1, 1).unwrap_err(),
        ForwardError::NotConnected
    );
}

#[test]
fn a_dropped_open_closes_the_forward_it_was_answered_with() {
    let mut plane = plane_with_forward();
    drop(plane.forward_open("localhost".to_owned(), 80).unwrap());
    let (request_id, _) = sent_request_id(&mut plane);
    answer(
        &mut plane,
        request_id,
        CommandResult::OkWith(CommandValue::ForwardOpened(42)),
    );
    let (close_id, command) = sent_request_id(&mut plane);
    assert!(matches!(
        command,
        Command::Forward {
            op: ForwardOp::Close { id: 42 }
        }
    ));
    answer(&mut plane, close_id, CommandResult::Ok);
    assert!(
        plane.take_outbound().is_empty(),
        "the close's answer resolves nothing"
    );
}
