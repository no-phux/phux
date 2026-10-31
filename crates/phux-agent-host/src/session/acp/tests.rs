use super::*;
use phux_protocol::native_agent::{NativeOption, NativeQuestion};
use serde_json::json;

#[tokio::test]
async fn unsupported_client_execution_callbacks_fail_explicitly() {
    let callback = Callback {
        sink: crate::session::tests::sink(),
        provider: "custom".into(),
        waiting: Mutex::new(BTreeMap::new()),
        rows: Mutex::new(Rows::default()),
    };
    let session = sdk::SessionId::new("session");
    let read = callback
        .request(sdk::AgentRequest::ReadTextFileRequest(
            sdk::ReadTextFileRequest::new(session.clone(), "/never-read"),
        ))
        .await
        .unwrap_err();
    assert_eq!(read.code, sdk::ErrorCode::MethodNotFound);
    let terminal = callback
        .request(sdk::AgentRequest::CreateTerminalRequest(
            sdk::CreateTerminalRequest::new(session, "never-executed"),
        ))
        .await
        .unwrap_err();
    assert_eq!(terminal.code, sdk::ErrorCode::MethodNotFound);
    let params = serde_json::value::to_raw_value(&json!({})).unwrap();
    let extension = callback
        .ext_method(sdk::ExtRequest::new("unknown", params.into()))
        .await
        .unwrap_err();
    assert_eq!(extension.code, sdk::ErrorCode::MethodNotFound);
}

#[test]
fn ended_prompt_releases_turn_and_callbacks_without_disconnect() {
    let sink = crate::session::tests::sink();
    let mut view = sink.view();
    view.connected = true;
    view.turn_id = Some("acp:7".into());
    sink.update(NativeUpdate::View { view }).unwrap();
    let (sender, mut receiver) = oneshot::channel();
    let callback = Callback {
        sink: sink.clone(),
        provider: "cursor".into(),
        waiting: Mutex::new(BTreeMap::from([("permission".into(), sender)])),
        rows: Mutex::new(Rows {
            prompt_user_id: Some("acp-prompt:7".into()),
            ..Rows::default()
        }),
    };
    callback.finish_prompt(7).unwrap();
    assert!(sink.view().connected);
    assert!(sink.view().turn_id.is_none());
    assert!(callback.rows.lock().prompt_user_id.is_none());
    assert_eq!(receiver.try_recv().unwrap()["cancelled"], true);
    assert!(!sink.begin_prompt("acp:8".into()).unwrap());
    assert_eq!(sink.view().turn_id.as_deref(), Some("acp:8"));
}
#[test]
fn loaded_history_and_prompt_without_user_echo_have_distinct_rows() {
    let callback = Callback {
        sink: crate::session::tests::sink(),
        provider: "custom".into(),
        waiting: Mutex::new(BTreeMap::new()),
        rows: Mutex::new(Rows::default()),
    };
    callback
        .chunk(TranscriptRole::Assistant, "historical".into())
        .unwrap();
    callback.prompt_entry(7, "new user").unwrap();
    callback
        .chunk(TranscriptRole::Assistant, "fresh reply".into())
        .unwrap();
    callback.flush_text().unwrap();
    let backlog = callback.sink.0.backlog.lock();
    let final_rows: Vec<_> = backlog
        .rows
        .iter()
        .filter_map(|(output, _)| {
            let crate::session::Output::Update(NativeUpdate::Transcript { entry }) = output else {
                return None;
            };
            let row =
                phux_client_core::session::agent_transcript::TranscriptEntry::from_data(entry)?;
            row.is_final.then_some(row)
        })
        .collect();
    assert_eq!(
        final_rows
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>(),
        ["historical", "new user", "fresh reply"]
    );
    assert_ne!(final_rows[0].id, final_rows[2].id);
    drop(backlog);
}

#[test]
fn chunks_preserve_row_identity_until_role_changes() {
    let callback = Callback {
        sink: crate::session::tests::sink(),
        provider: "cursor".into(),
        waiting: Mutex::new(BTreeMap::new()),
        rows: Mutex::new(Rows {
            prompt_user_id: Some("acp-prompt:7".into()),
            ..Rows::default()
        }),
    };
    callback
        .chunk(TranscriptRole::User, "first".into())
        .unwrap();
    callback
        .chunk(TranscriptRole::User, " second".into())
        .unwrap();
    callback
        .chunk(TranscriptRole::Assistant, "reply".into())
        .unwrap();
    callback.flush_text().unwrap();
    let backlog = callback.sink.0.backlog.lock();
    let rows: Vec<_> = backlog
        .rows
        .iter()
        .filter_map(|(output, _)| {
            let crate::session::Output::Update(NativeUpdate::Transcript { entry }) = output else {
                return None;
            };
            phux_client_core::session::agent_transcript::TranscriptEntry::from_data(entry)
        })
        .collect();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].id, "acp-prompt:7");
    assert_eq!(rows[1].id, rows[0].id);
    assert_eq!(rows[1].text, "first second");
    assert!(!rows[1].is_final);
    assert_eq!(rows[2].id, rows[0].id);
    assert_eq!(rows[2].text, "first second");
    assert!(rows[2].is_final);
    assert_ne!(rows[3].id, rows[0].id);
    assert_eq!(rows[3].role, TranscriptRole::Assistant);
    assert_eq!(rows[4].id, rows[3].id);
    assert_eq!(rows[4].text, "reply");
    assert!(rows[4].is_final);
    drop(backlog);
}

#[tokio::test]
async fn question_wait_requires_matching_explicit_answer() {
    let callback = Callback {
        sink: crate::session::tests::sink(),
        provider: "cursor".into(),
        waiting: Mutex::new(BTreeMap::new()),
        rows: Mutex::new(Rows::default()),
    };
    let question = NativeQuestion {
        id: "permission".into(),
        kind: "permission".into(),
        title: "Tool permission".into(),
        message: String::new(),
        options: vec![NativeOption {
            id: "allow-once".into(),
            label: "Allow once".into(),
            description: None,
        }],
        questions: Vec::new(),
        fields: json!({}),
    };
    let wait = callback.ask(question);
    let (answer, result) = tokio::join!(wait, async {
        tokio::task::yield_now().await;
        let refused = callback.startup_command(NativeCommand {
            operation_id: 1,
            activation: "a".into(),
            intent: NativeIntent::Answer {
                question_id: "other-permission".into(),
                response: json!({"option_id":"allow-once"}),
            },
        });
        assert_eq!(refused.disposition, NativeDisposition::Refused);
        assert!(callback.waiting.lock().contains_key("acp-question:1"));
        callback.startup_command(NativeCommand {
            operation_id: 2,
            activation: "a".into(),
            intent: NativeIntent::Answer {
                question_id: "acp-question:1".into(),
                response: json!({"option_id":"allow-once"}),
            },
        })
    });
    assert_eq!(answer.unwrap(), json!({"option_id":"allow-once"}));
    assert_eq!(result.operation_id, 2);
    assert_eq!(result.disposition, NativeDisposition::Queued);
    assert!(callback.waiting.lock().is_empty());
    assert!(callback.sink.view().questions.is_empty());
}
