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

/// docs/protocol.md: an ERROR correlated to the request is its answer.
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

#[test]
fn ordinary_archived_relaunch_to_an_old_peer_is_refused_without_writing() {
    block_on(async {
        let (mut client, server) = pair();
        let mut server = Session::from_stream(server);
        let peer = tokio::task::spawn_local(async move {
            let FrameKind::Hello { client_caps, .. } = server.recv().await.unwrap() else { panic!("HELLO"); };
            server.send(&hello_ok(&client_caps, phux_protocol::caps::ServerCapabilities::new()
                .with_features_ext(phux_protocol::caps::ServerFeatureExtSet::all()))).await.unwrap();
            server
        });
        client.negotiate("archive-policy-probe".into(), control_caps()).await.unwrap();
        let mut server = peer.await.unwrap();
        let command = Command::ResourceRelaunch {
            operation: phux_protocol::ids::OperationId::new([1;16]).unwrap(),
            resource: phux_protocol::ids::ResourceKey::new([2;16]).unwrap(),
            expected: phux_protocol::ids::Revision::new(1),
        };
        let (result, _) = client.request(7, command).await.unwrap().into_parts();
        assert!(matches!(result, CommandResult::Error { code: ErrorCode::Unsupported, .. }));
        use phux_protocol::native_agent::{NativeRequest, NativeRequestPayload, SpawnNativeAgent};
        let mut spec = SpawnNativeAgent {
            session: "archive".into(), backend: "acp".into(), provider: "fixture".into(),
            command: vec!["explicit-program".into()], cwd: Some("/".into()), env: Default::default(),
            native_id: None, guardian: Some(ResourceId::local(1)), guardian_key: None, guardian_expected: None,
        };
        for request in [
            NativeRequest::PrepareRelaunch {
                operation: phux_protocol::ids::OperationId::new([3;16]).unwrap(),
                resource: phux_protocol::ids::ResourceKey::new([2;16]).unwrap(),
                expected: phux_protocol::ids::Revision::new(1),
            },
            NativeRequest::Spawn { spec: spec.clone() },
        ] {
            let command = Command::NativeAgent { request: NativeRequestPayload::new(request).unwrap() };
            let (result, _) = client.request(8, command).await.unwrap().into_parts();
            assert!(matches!(result, CommandResult::Error { code: ErrorCode::Unsupported, .. }));
            assert!(server.try_recv().unwrap().is_none(), "neither owner review nor archived guardian intent reaches an old peer");
        }
        spec.guardian = None;
        let fresh = Command::NativeAgent { request: NativeRequestPayload::new(NativeRequest::Spawn { spec }).unwrap() };
        let (fresh_result, ()) = tokio::join!(client.request(9, fresh), async {
            let FrameKind::Command { request_id: 9, .. } =
                tokio::time::timeout(WEDGE_TIMEOUT, server.recv()).await.unwrap().unwrap().unwrap() else { panic!("fresh request"); };
            server.send(&FrameKind::CommandResult { request_id: 9, result: CommandResult::Ok }).await.unwrap();
        });
        assert!(matches!(fresh_result.unwrap().into_parts().0, CommandResult::Ok),
            "fresh explicitly requested native launch retains its old-peer contract");
        assert!(server.try_recv().unwrap().is_none(), "an old peer receives no executable archived intent");
    });
}
