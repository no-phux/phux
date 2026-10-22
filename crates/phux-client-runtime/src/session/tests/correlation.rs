//! Native-command gating and correlated replies, refusals, and disconnects.

use super::*;

#[test]
fn native_commands_without_negotiated_support_never_write() {
    block_on(async {
        let (mut client, server) = pair();
        let mut server = Session::from_stream(server);
        let peer = tokio::task::spawn_local(async move {
            let FrameKind::Hello { client_caps, .. } = server.recv().await.expect("HELLO") else {
                panic!("the first frame is HELLO");
            };
            server
                .send(&hello_ok(
                    &client_caps,
                    phux_protocol::caps::ServerCapabilities::new(),
                ))
                .await
                .expect("answer");
            server
        });
        client
            .negotiate("probe".to_owned(), control_caps())
            .await
            .expect("negotiate");
        let mut server = peer.await.expect("peer");
        let request = phux_protocol::native_agent::NativeRequestPayload::new(
            phux_protocol::native_agent::NativeRequest::Register {
                agent_session_id: ResourceId::local(1),
                activation: "activation".into(),
                binding_proof: "proof".into(),
            },
        )
        .expect("payload");
        let error = tokio::time::timeout(
            WEDGE_TIMEOUT,
            client.request(7, Command::NativeAgent { request }),
        )
        .await
        .expect("local refusal")
        .expect_err("unsupported native command");
        assert!(
            matches!(error, SessionError::Protocol(message) if message.contains("NATIVE_AGENT"))
        );
        assert!(server.try_recv().expect("nothing written").is_none());
    });
}

#[test]
fn pre_ack_bootstrap_is_returned_instead_of_discarded() {
    let mut script = bootstrap();
    script.push(ack(7));
    let reply = request_against(script);
    assert!(matches!(reply.result(), CommandResult::Ok));
    assert!(matches!(
        reply.interleaved(),
        [
            FrameKind::BootstrapBegin { .. },
            FrameKind::BootstrapChunk { .. },
            FrameKind::BootstrapReady { .. }
        ]
    ));
}

/// A hub pushes one uncorrelated ERROR per unreachable satellite before the
/// `GET_STATE` ack; swallowing it hides a partial fleet view.
#[test]
fn satellite_degradation_error_is_not_swallowed_by_get_state() {
    let reply = request_against(vec![
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: "no satellite route to build-box".to_owned(),
        },
        ack(7),
    ]);
    assert!(matches!(reply.result(), CommandResult::Ok));
    match reply.interleaved() {
        [FrameKind::Error { message, .. }] => {
            assert_eq!(message, "no satellite route to build-box");
        }
        other => panic!("degradation notice must survive, got {other:?}"),
    }
}

/// proto.md §9: an ERROR correlated to the request is its answer.
#[test]
fn correlated_error_answers_the_request_instead_of_hanging_forever() {
    let reply = request_against(vec![FrameKind::Error {
        request_id: Some(7),
        code: ErrorCode::InvalidCommand,
        message: "command not supported by this server".to_owned(),
    }]);
    match reply.result() {
        CommandResult::Error { code, message } => {
            assert_eq!(*code, ErrorCode::InvalidCommand);
            assert_eq!(message, "command not supported by this server");
        }
        other => panic!("a correlated ERROR is this command's answer, got {other:?}"),
    }
    assert!(reply.interleaved().is_empty());
}

/// Pipelined requests share a session: another request's ack is kept.
#[test]
fn another_requests_ack_is_kept_not_consumed() {
    let reply = request_against(vec![ack(99), ack(7)]);
    assert!(
        matches!(
            reply.interleaved(),
            [FrameKind::CommandResult { request_id: 99, .. }]
        ),
        "got {:?}",
        reply.interleaved()
    );
}

#[test]
fn a_server_that_closes_before_answering_is_a_disconnect() {
    block_on(async {
        let mut client = scripted(Vec::new());
        let reply = tokio::time::timeout(
            WEDGE_TIMEOUT,
            client.request(
                7,
                Command::GetState {
                    scope: StateScope::Server,
                },
            ),
        )
        .await
        .expect("resolves");
        assert!(
            matches!(reply, Err(SessionError::Disconnected)),
            "{reply:?}"
        );
    });
}
