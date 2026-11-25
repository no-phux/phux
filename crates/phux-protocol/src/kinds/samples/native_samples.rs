// All public and guardian request shapes carried by NATIVE_AGENT.
fn native_command_samples() -> [(Command, &'static Rule); 7] {
    use crate::native_agent::{NativeCommand, NativeIntent, NativeRequest, SpawnNativeAgent};
    [
        (NativeRequest::ListIntegrations {}, &C_NATIVE_LIST),
        (NativeRequest::PrepareRelaunch {
            operation: operation(), resource: resource_key(), expected: crate::ids::Revision::new(1),
        }, &C_PREPARE_RELAUNCH),
        (NativeRequest::ApproveRelaunch {
            approval: crate::native_agent::relaunch::RelaunchApproval {
                instance: [1; 16],
                operation: operation(), resource: resource_key(), expected: crate::ids::Revision::new(1),
                activation: serde_json::Value::Null,
                recipe: serde_json::json!({"terminal":{"argv":["sh"]}}),
                execution: serde_json::json!({"terminal":{"argv":["sh"]}}),
            },
        }, &C_APPROVE_RELAUNCH),
        (
            NativeRequest::LaunchIntegration {
                integration: "opencode".into(),
                session: session_key(),
                cwd: "/workspace".into(),
            },
            &C_NATIVE_LAUNCH,
        ),
        (
            NativeRequest::Spawn {
                spec: SpawnNativeAgent {
                    session: "session".into(),
                    backend: "acp".into(),
                    provider: "omp".into(),
                    command: vec!["omp".into()],
                    cwd: None,
                    env: std::collections::BTreeMap::new(),
                    native_id: None,
                    guardian: None,
                    guardian_key: None,
                    guardian_expected: None,
                },
            },
            &C_NATIVE_SPAWN,
        ),
        (
            NativeRequest::Command {
                agent_session_id: terminal(),
                command: NativeCommand {
                    operation_id: 1,
                    activation: "activation".into(),
                    intent: NativeIntent::Prompt {
                        text: "hello".into(),
                    },
                },
            },
            &C_NATIVE_COMMAND,
        ),
        (
            NativeRequest::Register {
                agent_session_id: terminal(),
                activation: "activation".into(),
                binding_proof: "proof".into(),
            },
            &C_NATIVE_CONTROLLER,
        ),
    ]
    .map(native_command_sample)
}

fn question_samples() -> [(Command, &'static Rule); 4] {
    use crate::ids::{AuthorityId, ResourceKey};
    use crate::native_agent::questions::{
        QuestionOperationId, QuestionRef, QuestionTarget, RespondQuestion,
    };
    use crate::native_agent::NativeRequest;

    let authority = AuthorityId::new([1; 16]).unwrap();
    let resource = ResourceKey::new([2; 16]).unwrap();
    let question = QuestionRef {
        authority,
        conversation: resource,
        activation: "activation".into(),
        id: "callback".into(),
        revision: "1".repeat(64),
    };
    let operation_id = QuestionOperationId::new(1).unwrap();
    [
        (
            NativeRequest::CurrentQuestions {
                target: QuestionTarget::Handle { resource: terminal() },
            },
            &C_CURRENT_QUESTIONS,
        ),
        (
            NativeRequest::CurrentQuestions {
                target: QuestionTarget::Key { authority, resource },
            },
            &C_CURRENT_QUESTIONS_KEYED,
        ),
        (
            NativeRequest::RespondQuestion {
                answer: RespondQuestion {
                    operation_id,
                    question: question.clone(),
                    response: serde_json::json!({"confirmed":false}),
                },
            },
            &C_RESPOND_QUESTION,
        ),
        (NativeRequest::QuestionReceipt { operation_id, question }, &C_QUESTION_RECEIPT),
    ]
    .map(native_command_sample)
}

fn native_command_sample(
    (request, rule): (crate::native_agent::NativeRequest, &'static Rule),
) -> (Command, &'static Rule) {
    (
        Command::NativeAgent {
            request: crate::native_agent::NativeRequestPayload::new(request).unwrap(),
        },
        rule,
    )
}
