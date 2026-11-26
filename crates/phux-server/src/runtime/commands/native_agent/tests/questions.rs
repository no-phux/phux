use super::*;
use phux_protocol::native_agent::questions::{
    CurrentQuestion, CurrentQuestions, QuestionOperationId, QuestionReceipt, QuestionRef,
    QuestionTarget, RespondQuestion,
};

async fn reference(fx: &Fixture) -> QuestionRef {
    let result: CurrentQuestions = decode(
        fx.request(
            fx.owner,
            NativeRequest::CurrentQuestions {
                target: QuestionTarget::Handle {
                    resource: fx.agent.clone(),
                },
            },
        )
        .await,
    );
    let [CurrentQuestion::Provider { reference, .. }] = result.questions.as_slice() else {
        panic!("expected exactly one SDK callback");
    };
    reference.clone()
}

#[tokio::test(flavor = "current_thread")]
async fn changed_question_invalidates_a_reserved_response_at_dispatch() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fx = Fixture::new();
            let generation = fx.ready().await;
            fx.ask(generation, 2, "permission").await;
            let old = reference(&fx).await;
            let invalid = RespondQuestion {
                operation_id: QuestionOperationId::new(799).unwrap(),
                question: old.clone(),
                response: serde_json::json!({"option_id":"deny-once","cancelled":true}),
            };
            let refused: QuestionReceipt = decode(
                fx.request(fx.owner, NativeRequest::RespondQuestion { answer: invalid })
                    .await,
            );
            assert_eq!(refused.disposition, NativeDisposition::Refused);
            let operation_id = QuestionOperationId::new(800).unwrap();
            let answer = RespondQuestion {
                operation_id,
                question: old.clone(),
                response: serde_json::json!({"option_id":"deny-once"}),
            };
            let request = NativeRequest::RespondQuestion { answer };
            let queued: QuestionReceipt = decode(fx.request(fx.owner, request).await);
            assert_eq!(queued.awaiting_dispatch, Some(true));

            let mut view = Fixture::view();
            let mut replacement = question("permission");
            replacement.title.push_str(" changed");
            view.questions.push(replacement);
            fx.published(generation, 3, NativeUpdate::View { view })
                .await;
            assert_ne!(old, reference(&fx).await);

            fx.state.with_mut(|s| refuse_undeliverable(s, &fx.agent));
            let refused: QuestionReceipt = decode(
                fx.request(
                    fx.owner,
                    NativeRequest::QuestionReceipt {
                        question: old,
                        operation_id,
                    },
                )
                .await,
            );
            assert_eq!(refused.disposition, NativeDisposition::Refused);
            assert_eq!(refused.awaiting_dispatch, Some(false));
            assert!(
                fx.state
                    .with(|s| s.native_agents().bindings[&fx.agent].pending.is_empty())
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn automation_and_wrong_authority_cannot_reserve_an_exact_callback() {
    crate::runtime::test_support::local(async {
        let fx = Fixture::new();
        let generation = fx.ready().await;
        fx.ask(generation, 2, "permission").await;
        let mut answer = RespondQuestion {
            operation_id: QuestionOperationId::new(900).unwrap(),
            question: reference(&fx).await,
            response: serde_json::json!({"option_id":"deny-once"}),
        };
        let auto = fx
            .request(
                fx.host,
                NativeRequest::RespondQuestion {
                    answer: answer.clone(),
                },
            )
            .await;
        assert_eq!(
            crate::runtime::test_support::error_code(&auto),
            Some(ErrorCode::PermissionDenied)
        );

        answer.question.authority = phux_protocol::ids::AuthorityId::new([0xB1; 16]).unwrap();
        let other = fx
            .request(fx.owner, NativeRequest::RespondQuestion { answer })
            .await;
        assert_eq!(
            crate::runtime::test_support::error_code(&other),
            Some(ErrorCode::PreconditionFailed)
        );
        assert!(
            fx.state
                .with(|s| s.native_agents().bindings[&fx.agent].operations.is_empty())
        );
    })
    .await;
}
