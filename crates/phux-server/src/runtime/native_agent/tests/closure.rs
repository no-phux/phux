use super::*;

async fn seal(fx: &Fixture, value: bool) {
    let bootstrap = fx.state.with(|s| {
        s.resource_handle(s.resource_from_wire(&fx.agent).unwrap())
            .unwrap()
            .agent_session()
            .unwrap()
            .bootstrap
            .clone()
    });
    let (reply, rx) = oneshot::channel();
    bootstrap
        .send(phux_agent_session::session::BootstrapRequest {
            reply,
            after_seq: None,
            seal: Some(value),
        })
        .await
        .unwrap();
    rx.await.unwrap();
}
async fn close_request(fx: &Fixture, generation: u64) -> CommandResult {
    fx.request(
        fx.host,
        NativeRequest::Close {
            agent_session_id: fx.agent.clone(),
            activation: "activation-A".into(),
            binding_proof: "private-test-proof".into(),
            generation,
        },
    )
    .await
}
async fn accept_without_ack(
    fx: &Fixture,
    id: u64,
    update: &NativeUpdate,
) -> (NativeView, NativePublicationReceipt) {
    let hash = fingerprint(update).unwrap();
    let view = fx.state.with_mut(|s| {
        reserve_publication(
            s.native_agents_mut().bindings.get_mut(&fx.agent).unwrap(),
            id,
            &hash,
            update,
        )
        .unwrap()
    });
    let accepted = super::super::super::resource_commands::append_native_publication(
        &fx.state,
        &fx.agent,
        encode_publication(update, &view).unwrap(),
        phux_agent_session::session::NativeAppendIdentity {
            id,
            fingerprint: hash,
        },
    )
    .await
    .unwrap();
    (
        view,
        NativePublicationReceipt {
            publication_id: id,
            first_seq: accepted.first_seq,
            last_seq: accepted.last_seq,
        },
    )
}

#[tokio::test(flavor = "current_thread")]
async fn late_accepted_publication_never_reopens_closed_source() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for source_exit in [false, true] {
                let fx = Fixture::new();
                let generation = fx.ready().await;
                let update = NativeUpdate::QuestionOpened {
                    question: question("input"),
                };
                let (view, receipt) = accept_without_ack(&fx, 2, &update).await;
                if source_exit {
                    source_exited(&fx.state, fx.parent).await;
                } else {
                    assert_eq!(close_request(&fx, generation).await, CommandResult::Ok);
                }
                let reply: NativePublicationReceipt = decode(acknowledge_publication(
                    &fx.state, &fx.agent, view, generation, &receipt, &update,
                ));
                assert_eq!(reply, receipt);
                fx.state.with(|s| {
                    let binding = &s.native_agents().bindings[&fx.agent];
                    assert!(binding.closed && binding.closure_committed);
                    assert!(!binding.view.connected);
                    assert!(binding.view.questions.is_empty());
                    assert!(binding.view.turn_id.is_none());
                });
                assert!(!fx.state.with(|s| s.agent_is_asked(fx.parent)));
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_duplicate_ack_and_old_generation_do_not_roll_back_projection() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            let update = NativeUpdate::State {
                state: NativeState::Working,
                turn_id: Some("older".into()),
            };
            let (older, receipt) = accept_without_ack(&fx, 2, &update).await;
            decode::<NativePublicationReceipt>(acknowledge_publication(
                &fx.state,
                &fx.agent,
                older.clone(),
                generation,
                &receipt,
                &update,
            ));
            decode::<NativePublicationReceipt>(
                fx.publish(
                    generation,
                    3,
                    NativeUpdate::State {
                        state: NativeState::Working,
                        turn_id: Some("newer".into()),
                    },
                )
                .await,
            );
            decode::<NativePublicationReceipt>(acknowledge_publication(
                &fx.state, &fx.agent, older, generation, &receipt, &update,
            ));
            assert_eq!(
                fx.state
                    .with(|s| s.native_agents().bindings[&fx.agent].view.turn_id.clone()),
                Some("newer".into())
            );
            let (stale, receipt) = accept_without_ack(&fx, 4, &update).await;
            assert!(fx.register().await > generation);
            decode::<NativePublicationReceipt>(acknowledge_publication(
                &fx.state, &fx.agent, stale, generation, &receipt, &update,
            ));
            assert_eq!(
                fx.state
                    .with(|s| s.native_agents().bindings[&fx.agent].view.turn_id.clone()),
                Some("newer".into())
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn sealed_closure_is_fenced_retryable_and_committed_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for source_exit in [false, true] {
                let fx = Fixture::new();
                let generation = fx.ready().await;
                seal(&fx, true).await;
                if source_exit {
                    source_exited(&fx.state, fx.parent).await;
                } else {
                    assert!(matches!(
                        close_request(&fx, generation).await,
                        CommandResult::Error {
                            code: ErrorCode::Overflow,
                            ..
                        }
                    ));
                }
                fx.state.with(|s| {
                    let binding = &s.native_agents().bindings[&fx.agent];
                    assert!(binding.closed && !binding.closure_committed);
                    assert!(!binding.view.connected);
                });
                assert_eq!(
                    fx.command(fx.owner, 99, prompt("never restart"))
                        .await
                        .disposition,
                    NativeDisposition::Refused
                );
                seal(&fx, false).await;
                if source_exit {
                    source_exited(&fx.state, fx.parent).await;
                    source_exited(&fx.state, fx.parent).await;
                } else {
                    assert_eq!(fx.register().await, generation);
                    assert_eq!(close_request(&fx, generation).await, CommandResult::Ok);
                    assert_eq!(close_request(&fx, generation).await, CommandResult::Ok);
                }
                assert!(
                    fx.state
                        .with(|s| s.native_agents().bindings[&fx.agent].closure_committed)
                );
                let cut = fx.cut().await;
                assert!(cut.ended);
                assert_eq!(
                    cut.records
                        .iter()
                        .filter(|record| std::str::from_utf8(record)
                            .unwrap()
                            .contains("\"type\":\"session_end\""))
                        .count(),
                    1
                );
            }
        })
        .await;
}
