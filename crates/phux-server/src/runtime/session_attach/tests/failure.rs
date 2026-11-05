//! Aggregate rollback and a resource's natural close during capture.

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn oversized_document_rolls_back_every_kind_before_attached() {
    LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let (session, parent) = parent(&state, true);
            let (agent, mut cuts) = agent_stub(&state, parent, session);
            let (tree, handle) = tree(&state, parent, session);
            let payload = format!("\"{}\"", "x".repeat(staging::MAX_STAGED_BOOTSTRAP_BYTES));
            publish::<TreeDocument>(&handle, Bytes::from(payload)).await;
            let mut connection = Connection::start(&state, true).await;
            connection.attach(1).await;
            assert!(matches!(
                connection.next().await,
                FrameKind::Error {
                    code: ErrorCode::CodecUnavailable,
                    ..
                }
            ));
            assert!(matches!(
                connection.next().await,
                FrameKind::Detached { .. }
            ));
            state.with(|s| {
                assert!(!s.attached().contains_key(&connection.client));
                for core in [parent, agent, tree] {
                    assert!(
                        !s.subscribers_for_resource(core)
                            .contains(&connection.client)
                    );
                    assert!(s.input_lease_holder(core).is_none());
                }
            });
            assert!(
                cuts.try_recv().is_err(),
                "rollback never starts a deferred agent cut"
            );
            connection.task.await.unwrap().unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_explicit_agent_cut_refuses_without_stopping_documents() {
    LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let (session, parent) = parent(&state, false);
            let (_, handle) = tree(&state, parent, session);
            let (agent, mut cut) = agent_stub(&state, parent, session);
            let wire = state.with_mut(|s| s.intern_resource_wire(agent));
            let mut connection = Connection::start(&state, false).await;
            connection.attach(1).await;
            prefix(&mut connection, 2, 1).await;
            assert!(
                cut.try_recv().is_err(),
                "session attach must not request an agent cut"
            );
            connection
                .send(FrameKind::Command {
                    request_id: 70,
                    command: phux_protocol::wire::frame::Command::attach_agent_session(
                        wire, None, None,
                    ),
                })
                .await;
            let last_capture = tokio::time::timeout(std::time::Duration::from_secs(5), cut.recv())
                .await
                .unwrap()
                .unwrap();
            last_capture
                .reply
                .send(agent_cut(vec![Bytes::from_static(b"not-json\n")]))
                .unwrap();
            assert!(matches!(
                connection.next().await,
                FrameKind::CommandResult {
                    request_id: 70,
                    result: phux_protocol::wire::frame::CommandResult::Error {
                        code: ErrorCode::CodecUnavailable,
                        ..
                    }
                }
            ));
            assert!(state.with(|s| s.attached().contains_key(&connection.client)));
            publish::<TreeDocument>(&handle, Bytes::from_static(b"{}")).await;
            assert!(matches!(
                connection.next().await,
                FrameKind::ResourceOutput { seq: 1, .. }
            ));
            connection.finish().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn resource_closed_mid_capture_satisfies_the_barrier_without_stopping_documents() {
    LocalSet::new().run_until(async {
        let state = SharedState::new();
        let (session, parent) = parent(&state, false);
        let (_, handle) = tree(&state, parent, session);
        let (closed, parked, actor) = parked_tree(&state, parent, session);
        let wire = state.with_mut(|s| s.intern_resource_wire(closed));
        let mut connection = Connection::start(&state, false).await;
        connection.attach(1).await;
        wait_for_capture(&parked).await;
        drop(actor);
        assert!(matches!(connection.next().await, FrameKind::Attached { .. }));
        assert!(matches!(connection.next().await, FrameKind::BootstrapBegin { profile: BootstrapStreamProfile::AccessibilityTreeJsonV1, .. }));
        assert!(matches!(connection.next().await, FrameKind::BootstrapReady { .. }));
        assert!(matches!(connection.next().await, FrameKind::ResourceClosed { terminal_id, .. } if terminal_id == wire));
        assert!(matches!(connection.next().await, FrameKind::AttachReady { attach_id: 1 }));
        publish::<TreeDocument>(&handle, Bytes::from_static(b"{}")).await;
        assert!(matches!(connection.next().await, FrameKind::ResourceOutput { seq: 1, .. }));
        connection.finish().await;
    }).await;
}
