use super::*;

#[tokio::test(flavor = "current_thread")]
async fn invalid_declared_custom_answer_does_not_reserve_the_question() {
    tokio::task::LocalSet::new().run_until(async {
        let fx = Fixture::new(); let generation = fx.ready().await;
        let mut ask = question("ask");
        ask.fields = serde_json::json!({"allow_custom_input":false});
        ask.questions.push(NativeQuestionItem { id: "choice".into(), question: "Choose".into(), header: "Choice".into(),
            options: ask.options.clone(), multi: false, recommended: None });
        fx.published(generation, 2, NativeUpdate::QuestionOpened { question: ask }).await;
        let invalid = answer(serde_json::json!({"answers":[{"id":"choice","selected_options":[],"custom_input":"unsupported"}]}));
        assert_eq!(fx.disposition(fx.owner, 100, invalid.clone()).await, NativeDisposition::Refused);
        let corrected = answer(serde_json::json!({"answers":[{"id":"choice","selected_options":["deny-once"]}]}));
        assert_eq!(fx.disposition(fx.owner, 101, corrected.clone()).await, NativeDisposition::Queued);
        assert_eq!(fx.disposition(fx.owner, 100, invalid).await, NativeDisposition::Refused);
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
                fx.ask(generation, 2, "form").await;
                let first = answer(serde_json::json!({"values":{"count":"1"}}));
                assert_eq!(
                    fx.disposition(fx.owner, 110, first.clone()).await,
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
                let corrected = answer(serde_json::json!({"values":{"count":1}}));
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
                        fx.disposition(fx.owner, 110, first).await,
                        NativeDisposition::Refused
                    );
                    assert_eq!(
                        fx.disposition(
                            fx.owner,
                            112,
                            answer(serde_json::json!({"cancelled":true}))
                        )
                        .await,
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
    let completions: &[NativeDisposition] = match case {
        "queued" => &[NativeDisposition::Queued],
        "admitted" => &[NativeDisposition::Admitted],
        "completed" => &[NativeDisposition::Completed],
        "submitted-then-unknown" => &[NativeDisposition::Queued, NativeDisposition::Unknown],
        _ => &[],
    };
    for disposition in completions {
        assert_eq!(
            fx.request(fx.host, fx.completion(generation, 110, *disposition))
                .await,
            CommandResult::Ok,
            "{case}: {disposition:?}"
        );
    }
    match case {
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
            fx.published(
                generation,
                3,
                NativeUpdate::QuestionClosed {
                    id: "question-A".into(),
                    reason: "withdrawn".into(),
                },
            )
            .await;
        }
        "local" | "unknown" | "queued" | "admitted" | "completed" | "submitted-then-unknown" => {}
        _ => panic!("unhandled reservation case: {case}"),
    }
}
