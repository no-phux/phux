use super::*;

#[tokio::test(flavor = "current_thread")]
async fn exact_resource_activation_and_operation_identity_are_required() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            fx.ready().await;
            let wrong_resource = fx
                .request(
                    fx.owner,
                    NativeRequest::Command {
                        agent_session_id: fx.parent_wire.clone(),
                        command: NativeCommand {
                            operation_id: 1,
                            activation: "activation-A".into(),
                            intent: prompt("do not execute"),
                        },
                    },
                )
                .await;
            assert_eq!(
                decode::<NativeCommandResult>(wrong_resource).disposition,
                NativeDisposition::Refused
            );
            let stale = fx
                .request(
                    fx.owner,
                    NativeRequest::Command {
                        agent_session_id: fx.agent.clone(),
                        command: NativeCommand {
                            operation_id: 99,
                            activation: "other-activation".into(),
                            intent: prompt("do not execute"),
                        },
                    },
                )
                .await;
            assert_eq!(
                decode::<NativeCommandResult>(stale).disposition,
                NativeDisposition::Refused
            );
            assert_eq!(
                fx.command(fx.owner, 99, prompt("changed activation and payload"))
                    .await
                    .disposition,
                NativeDisposition::Refused
            );
            assert_eq!(
                fx.command(fx.owner, 1, prompt("reserved input"))
                    .await
                    .disposition,
                NativeDisposition::Queued
            );
            assert_eq!(
                fx.command(fx.owner, 1, prompt("different input"))
                    .await
                    .disposition,
                NativeDisposition::Refused
            );
            assert_eq!(
                fx.command(fx.owner, 1, prompt("reserved input"))
                    .await
                    .disposition,
                NativeDisposition::Queued
            );
            assert_eq!(
                fx.command(
                    fx.owner,
                    2,
                    NativeIntent::Interrupt {
                        turn_id: "stale-turn".into()
                    }
                )
                .await
                .disposition,
                NativeDisposition::Refused
            );
            assert_eq!(
                fx.state
                    .with(|s| s.native_agents().bindings[&fx.agent].pending.len()),
                1
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn concurrent_viewers_get_exactly_one_question_responder() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            decode::<NativePublicationReceipt>(
                fx.publish(
                    generation,
                    2,
                    NativeUpdate::QuestionOpened {
                        question: question("confirm"),
                    },
                )
                .await,
            );
            assert_eq!(
                fx.asked_inventory(),
                (Some("question-A".into()), true, Some(b"1".to_vec()))
            );
            // Codex nonblocking questions can outlive a completed model turn.
            // Only the provider's explicit question closure withdraws the request.
            decode::<NativePublicationReceipt>(
                fx.publish(
                    generation,
                    3,
                    NativeUpdate::State {
                        state: NativeState::Done,
                        turn_id: None,
                    },
                )
                .await,
            );
            assert_eq!(
                fx.asked_inventory(),
                (Some("question-A".into()), true, Some(b"1".to_vec()))
            );
            let second = fx.state.with_mut(|s| {
                let client = s.new_client_id();
                s.set_connection_grant(client, ConnectionGrant::owner());
                client
            });
            let answer = NativeIntent::Answer {
                question_id: "question-A".into(),
                response: serde_json::json!({"confirmed":false}),
            };
            let (a, b) = tokio::join!(
                fx.command(fx.owner, 10, answer.clone()),
                fx.command(second, 11, answer)
            );
            assert_eq!(
                [a.disposition, b.disposition]
                    .iter()
                    .filter(|disposition| **disposition == NativeDisposition::Queued)
                    .count(),
                1
            );
            assert_eq!(
                [a.disposition, b.disposition]
                    .iter()
                    .filter(|disposition| **disposition == NativeDisposition::Refused)
                    .count(),
                1
            );
            decode::<NativePublicationReceipt>(
                fx.publish(
                    generation,
                    4,
                    NativeUpdate::QuestionClosed {
                        id: "question-A".into(),
                        reason: "provider withdrew question".into(),
                    },
                )
                .await,
            );
            assert_eq!(fx.asked_inventory(), (None, false, None));
            assert_eq!(
                fx.command(
                    second,
                    12,
                    NativeIntent::Answer {
                        question_id: "question-A".into(),
                        response: serde_json::json!({"confirmed":true})
                    }
                )
                .await
                .disposition,
                NativeDisposition::Refused
            );
            fx.state.with_mut(|s| refuse_undeliverable(s, &fx.agent));
            assert!(
                fx.state
                    .with(|s| s.native_agents().bindings[&fx.agent].pending.is_empty())
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn human_permission_and_parent_input_lease_gate_native_commands() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            decode::<NativePublicationReceipt>(
                fx.publish(
                    generation,
                    2,
                    NativeUpdate::QuestionOpened {
                        question: question("permission"),
                    },
                )
                .await,
            );
            assert_eq!(
                fx.command(
                    fx.host,
                    20,
                    NativeIntent::Answer {
                        question_id: "question-A".into(),
                        response: serde_json::json!({"option_id":"deny-once"})
                    }
                )
                .await
                .disposition,
                NativeDisposition::Refused
            );
            let other = fx.state.with_mut(|s| {
                let client = s.new_client_id();
                s.set_connection_grant(client, ConnectionGrant::owner());
                s.set_input_lease(fx.parent, client);
                client
            });
            assert_eq!(
                fx.command(fx.owner, 21, prompt("lease-blocked"))
                    .await
                    .disposition,
                NativeDisposition::Refused
            );
            fx.state.with_mut(|s| {
                s.release_input_lease(fx.parent, other);
            });
            assert_eq!(
                fx.command(
                    fx.owner,
                    22,
                    NativeIntent::Answer {
                        question_id: "question-A".into(),
                        response: serde_json::json!({"cancelled":true})
                    }
                )
                .await
                .disposition,
                NativeDisposition::Queued
            );
            let raw = crate::runtime::commands::handle_route_input(
                &fx.state,
                fx.owner,
                &fx.parent_wire,
                phux_protocol::input::InputEvent::Paste(phux_protocol::input::paste::PasteEvent {
                    trust: phux_protocol::input::paste::PasteTrust::Trusted,
                    data: b"must not reach PTY".to_vec(),
                }),
            );
            assert!(matches!(
                raw,
                CommandResult::Error {
                    code: ErrorCode::InputNotWritten,
                    ..
                }
            ));
        })
        .await;
}
