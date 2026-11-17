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

fn plane_with_agent() -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    let _ = plane.take_outbound();
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk { server_caps, .. } = &mut hello {
        *server_caps = server_caps
            .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::ForwardAgent]));
    }
    plane.feed(hello).unwrap();
    let _ = plane.take_outbound();
    plane
}

/// The two forwarding bits gate their own calls: a server with only TCP
/// forwarding is never sent an ssh-agent offer, and the reverse.
#[test]
fn each_forwarding_bit_gates_only_its_own_calls() {
    let mut tcp = plane_with_forward();
    assert!(!tcp.forward_agent_supported());
    assert_eq!(tcp.agent_offer().unwrap_err(), ForwardError::Unsupported);
    assert_eq!(
        tcp.agent_reply(1, vec![5]).unwrap_err(),
        ForwardError::Unsupported
    );
    assert!(tcp.take_outbound().is_empty());

    let mut agent = plane_with_agent();
    assert!(agent.forward_agent_supported() && !agent.forward_supported());
    assert_eq!(
        agent.forward_open("localhost".to_owned(), 80).unwrap_err(),
        ForwardError::Unsupported
    );
    assert!(agent.take_outbound().is_empty());
}

#[test]
fn an_offer_is_sent_and_a_request_surfaces_to_be_answered() {
    use phux_protocol::wire::frame::AgentEvent;
    let mut plane = plane_with_agent();
    let offer = plane.agent_offer().unwrap();
    let (request_id, command) = sent_request_id(&mut plane);
    assert!(matches!(
        command,
        Command::Forward {
            op: ForwardOp::AgentOffer
        }
    ));
    answer(&mut plane, request_id, CommandResult::Ok);
    assert_eq!(ready(offer), Ok(()));

    let _ = plane.take_events();
    let asked = FrameKind::Event {
        terminal: None,
        event: AgentEvent::SshAgentRequest {
            id: 7,
            message: vec![11],
        },
        stamp: None,
    };
    plane.feed(asked.clone()).unwrap();
    let events = plane.take_events();
    assert!(
        matches!(events.as_slice(), [Event::Frame(frame)] if **frame == asked),
        "{events:?}"
    );
    let read: Vec<_> = events
        .into_iter()
        .filter_map(phux_client_runtime::control::forward::agent_request)
        .collect();
    assert_eq!(read, [(7, vec![11])]);

    let reply = plane.agent_reply(7, vec![12, 0, 0, 0, 0]).unwrap();
    let (request_id, command) = sent_request_id(&mut plane);
    let sent = ForwardOp::AgentReply {
        id: 7,
        message: vec![12, 0, 0, 0, 0],
    };
    assert!(matches!(command, Command::Forward { op } if op == sent));
    let late = CommandResult::Error {
        code: ErrorCode::NotFound,
        message: "gone".to_owned(),
    };
    answer(&mut plane, request_id, late);
    assert!(matches!(
        ready(reply).unwrap_err(),
        ForwardError::Refused {
            code: ErrorCode::NotFound,
            ..
        }
    ));

    // An over-long reply is never sent.
    let over = vec![5; phux_protocol::forward::MAX_AGENT_MESSAGE_BYTES + 1];
    assert_eq!(
        plane.agent_reply(8, over).unwrap_err(),
        ForwardError::Invalid
    );
    assert!(plane.take_outbound().is_empty());
}
