//! Credential values must not survive in journal replay or metadata.
use super::*;

#[test]
fn secret_values_never_appear_in_journal_replay_or_surviving_metadata() {
    run_local(async {
        let server = Server::start(300, None);
        let mut requester = server.connect("requester").await;
        let mut provider = server.connect("provider").await;
        let mut approver = server.connect("approver").await;
        let mut observer = server.observer().await;
        assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        begin(&mut requester).await;
        let id = opened(&mut observer).await;
        decide(&mut approver, id, true).await;
        let _ = claim(&mut provider).await;
        let canary = "NEVER-PERSIST-CREDENTIAL-CANARY";
        assert_eq!(
            complete(&mut provider, id, Some(vec![canary.into()])).await,
            CommandResult::Ok
        );
        let result = answer(&mut requester).await;
        assert!(!format!("{result:?}").contains(canary));
        let mut replay = server.connect("replayer").await;
        send_frame(
            &mut replay,
            &FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: Some(0),
            },
        )
        .await;
        send_frame(
            &mut replay,
            &FrameKind::Command {
                request_id: 50,
                command: Command::GetState {
                    scope: StateScope::Server,
                },
            },
        )
        .await;
        let mut saw_opening = false;
        let mut saw_decision = false;
        loop {
            let (_, frame) = phux_server_testkit::recv_typed(&mut replay).await;
            let mut encoded = bytes::BytesMut::new();
            frame.encode(&mut encoded);
            assert!(
                !encoded
                    .windows(canary.len())
                    .any(|part| part == canary.as_bytes())
            );
            assert!(!format!("{frame:?}").contains(canary));
            match frame {
                FrameKind::Event {
                    event: AgentEvent::ApprovalRequested { id: got },
                    ..
                } if got == id => saw_opening = true,
                FrameKind::Event {
                    event: AgentEvent::ApprovalDecided { id: got, .. },
                    ..
                } if got == id => saw_decision = true,
                FrameKind::CommandResult { request_id: 50, .. } => break,
                _ => {}
            }
        }
        assert!(
            saw_opening && saw_decision,
            "the journal actually replayed the credential exchange"
        );
        send_frame(
            &mut replay,
            &FrameKind::ListMetadata {
                request_id: 51,
                scope: Scope::Global,
            },
        )
        .await;
        let keys = recv_until(&mut replay, |_, frame| match frame {
            FrameKind::MetadataKeys {
                request_id: 51,
                keys,
            } => Some(keys),
            _ => None,
        })
        .await;
        for key in keys {
            if let Some(value) = metadata(&mut replay, &key).await {
                assert!(
                    !value
                        .windows(canary.len())
                        .any(|part| part == canary.as_bytes())
                );
            }
        }
        server.stop().await;
    });
}
