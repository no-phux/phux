use super::*;

#[tokio::test(flavor = "current_thread")]
async fn invalid_declared_custom_answer_does_not_reserve_the_question() {
    tokio::task::LocalSet::new().run_until(async {
        let fx = Fixture::new(); let generation = fx.ready().await;
        let mut ask = question("ask");
        ask.fields = serde_json::json!({"allow_custom_input":false});
        ask.questions.push(NativeQuestionItem { id: "choice".into(), question: "Choose".into(), header: "Choice".into(),
            options: ask.options.clone(), multi: false, recommended: None });
        decode::<NativePublicationReceipt>(fx.publish(generation, 2, NativeUpdate::QuestionOpened { question: ask }).await);
        let invalid = NativeIntent::Answer { question_id: "question-A".into(),
            response: serde_json::json!({"answers":[{"id":"choice","selected_options":[],"custom_input":"unsupported"}]}) };
        assert_eq!(fx.command(fx.owner, 100, invalid.clone()).await.disposition, NativeDisposition::Refused);
        let corrected = NativeIntent::Answer { question_id: "question-A".into(),
            response: serde_json::json!({"answers":[{"id":"choice","selected_options":["deny-once"]}]}) };
        assert_eq!(fx.command(fx.owner, 101, corrected.clone()).await.disposition, NativeDisposition::Queued);
        assert_eq!(fx.command(fx.owner, 100, invalid).await.disposition, NativeDisposition::Refused);
        assert_eq!(fx.receive(generation).await.intent, corrected);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_answer_refusal_releases_only_its_exact_live_reservation() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for case in [
                "local",
                "unknown",
                "queued",
                "admitted",
                "completed",
                "other-operation",
                "other-activation",
                "closed-question",
                "submitted-then-unknown",
            ] {
                let fx = Fixture::new();
                let generation = fx.ready().await;
                decode::<NativePublicationReceipt>(
                    fx.publish(
                        generation,
                        2,
                        NativeUpdate::QuestionOpened {
                            question: question("form"),
                        },
                    )
                    .await,
                );
                let answer = NativeIntent::Answer {
                    question_id: "question-A".into(),
                    response: serde_json::json!({"values":{"count":"1"}}),
                };
                assert_eq!(
                    fx.command(fx.owner, 110, answer.clone()).await.disposition,
                    NativeDisposition::Queued
                );
                fx.receive(generation).await;
                let complete = |disposition| fx.completion(generation, 110, disposition);
                prepare_answer_reservation_case(&fx, generation, case).await;
                if case != "unknown" {
                    let receipt = fx
                        .request(fx.host, complete(NativeDisposition::Refused))
                        .await;
                    if case == "completed" {
                        assert!(matches!(receipt, CommandResult::Error { .. }));
                    } else {
                        assert_eq!(receipt, CommandResult::Ok);
                    }
                }
                let corrected = NativeIntent::Answer {
                    question_id: "question-A".into(),
                    response: serde_json::json!({"values":{"count":1}}),
                };
                let result = fx.command(fx.owner, 111, corrected).await;
                assert_eq!(
                    result.disposition,
                    if case == "local" {
                        NativeDisposition::Queued
                    } else {
                        NativeDisposition::Refused
                    },
                    "{case}"
                );
                if case == "local" {
                    // Replaying the old final refusal cannot remove the new responder.
                    assert_eq!(
                        fx.request(fx.host, complete(NativeDisposition::Refused))
                            .await,
                        CommandResult::Ok
                    );
                    assert_eq!(
                        fx.command(fx.owner, 110, answer).await.disposition,
                        NativeDisposition::Refused
                    );
                    assert_eq!(
                        fx.command(
                            fx.owner,
                            112,
                            NativeIntent::Answer {
                                question_id: "question-A".into(),
                                response: serde_json::json!({"cancelled":true})
                            }
                        )
                        .await
                        .disposition,
                        NativeDisposition::Refused
                    );
                    assert_eq!(
                        fx.state
                            .with(|s| s.native_agents().bindings[&fx.agent].answered["question-A"]),
                        111
                    );
                } else {
                    assert!(fx.state.with(|s| {
                        s.native_agents().bindings[&fx.agent]
                            .answered
                            .contains_key("question-A")
                    }));
                }
            }
        })
        .await;
}

async fn prepare_answer_reservation_case(fx: &Fixture, generation: u64, case: &str) {
    let complete = |disposition| fx.completion(generation, 110, disposition);
    match case {
        "queued" => assert_eq!(
            fx.request(fx.host, complete(NativeDisposition::Queued))
                .await,
            CommandResult::Ok
        ),
        "admitted" => assert_eq!(
            fx.request(fx.host, complete(NativeDisposition::Admitted))
                .await,
            CommandResult::Ok
        ),
        "completed" => assert_eq!(
            fx.request(fx.host, complete(NativeDisposition::Completed))
                .await,
            CommandResult::Ok
        ),
        "submitted-then-unknown" => {
            assert_eq!(
                fx.request(fx.host, complete(NativeDisposition::Queued))
                    .await,
                CommandResult::Ok
            );
            assert_eq!(
                fx.request(fx.host, complete(NativeDisposition::Unknown))
                    .await,
                CommandResult::Ok
            );
        }
        "other-operation" => fx.state.with_mut(|s| {
            s.native_agents_mut()
                .bindings
                .get_mut(&fx.agent)
                .unwrap()
                .answered
                .insert("question-A".into(), 999);
        }),
        "other-activation" => fx.state.with_mut(|s| {
            s.native_agents_mut()
                .bindings
                .get_mut(&fx.agent)
                .unwrap()
                .operations
                .get_mut(&110)
                .unwrap()
                .command
                .activation = "older-activation".into();
        }),
        "closed-question" => {
            decode::<NativePublicationReceipt>(
                fx.publish(
                    generation,
                    3,
                    NativeUpdate::QuestionClosed {
                        id: "question-A".into(),
                        reason: "withdrawn".into(),
                    },
                )
                .await,
            );
        }
        "local" | "unknown" => {}
        _ => panic!("unhandled reservation case: {case}"),
    }
}
