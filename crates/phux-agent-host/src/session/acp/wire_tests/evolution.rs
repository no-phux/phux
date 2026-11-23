//! Producer-boundary regressions, not a promise of arbitrary future ACP support.
use super::*;
use phux_protocol::native_agent::NativeQuestion;

async fn opened_question(peer: &Peer, id: &str) -> NativeQuestion {
    peer.question(id).await;
    wait_for(|| {
        peer.callback
            .sink
            .view()
            .questions
            .iter()
            .any(|question| question.id == id)
    })
    .await;
    peer.callback
        .sink
        .view()
        .questions
        .into_iter()
        .find(|question| question.id == id)
        .unwrap()
}

#[tokio::test]
async fn v1_opaque_permission_ids_and_metadata_round_trip_for_custom_provider() {
    let mut host = spec(None);
    host.provider = "independent-acp".into();
    run_peer(host, async |mut peer| {
        peer.new_session().await;
        let params = json!({
            "sessionId":"session",
            "toolCall":{
                "toolCallId":"source/tool:7", "title":"Choose a workspace",
                "_meta":{"display":{"revision":2,"tags":["new",{"accent":"blue"}]}}
            },
            "options":[
                {"optionId":"opaque/7:alpha", "name":"Workspace alpha", "kind":"allow_once",
                 "_meta":{"display":{"rank":2}}},
                {"optionId":"opaque/9:beta", "name":"Workspace beta", "kind":"reject_once",
                 "_meta":{"display":{"rank":1}}}
            ],
            "_meta":{"provider":{"annotations":{"revision":3,"notes":["descriptive",null]}}}
        });
        peer.send(json!({
            "jsonrpc":"2.0", "id":"opaque-permission",
            "method":"session/request_permission", "params":params
        }))
        .await;
        let question = opened_question(&peer, "acp-question:1").await;
        assert_eq!(question.kind, "permission");
        assert_eq!(question.title, "Choose a workspace");
        assert_eq!(
            question
                .options
                .iter()
                .map(|option| (option.id.as_str(), option.label.as_str()))
                .collect::<Vec<_>>(),
            [
                ("opaque/7:alpha", "Workspace alpha"),
                ("opaque/9:beta", "Workspace beta")
            ]
        );
        assert_eq!(question.fields["_meta"], params["_meta"]);
        assert_eq!(
            question.fields["toolCall"]["_meta"],
            params["toolCall"]["_meta"]
        );
        assert_eq!(question.fields["options"], params["options"]);
        assert_eq!(question.fields["source_question_id"], "source/tool:7");

        // The raw command channel bypasses host admission. Exercise the actual
        // production guard against this SDK-produced question before forwarding.
        let mut command = NativeCommand {
            operation_id: 1,
            activation: "a".into(),
            intent: NativeIntent::Answer {
                question_id: question.id.clone(),
                response: json!({"option_id":"Workspace beta"}),
            },
        };
        assert!(crate::session::command_refusal(&peer.callback.sink, &command, None).is_some());
        assert!(peer.callback.sink.0.responders.lock().is_empty());
        assert!(peer.callback.waiting.lock().contains_key(&question.id));
        assert_eq!(peer.callback.sink.view().questions, vec![question.clone()]);

        command.operation_id = 2;
        command.intent = NativeIntent::Answer {
            question_id: question.id.clone(),
            response: json!({"option_id":"opaque/9:beta"}),
        };
        assert_eq!(
            crate::session::command_refusal(&peer.callback.sink, &command, None),
            None
        );
        assert_eq!(
            peer.callback.sink.0.responders.lock().get(&question.id),
            Some(&2)
        );
        peer.commands.send(command).await.unwrap();
        let answer = peer.recv().await;
        assert_eq!(answer["id"], "opaque-permission");
        assert_eq!(
            answer["result"],
            json!({"outcome":{"outcome":"selected", "optionId":"opaque/9:beta"}})
        );
        assert!(answer.get("error").is_none());
        peer.receipt(2, NativeDisposition::Queued).await;
        assert!(peer.callback.sink.view().questions.is_empty());
        assert!(peer.callback.sink.0.responders.lock().is_empty());
        peer.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn v1_unadvertised_typed_elicitation_is_refused_without_blocking_permission() {
    run_peer(spec(None), async |mut peer| {
        // new_session checks initialize advertises no elicitation capability.
        peer.new_session().await;
        let request = sdk::CreateElicitationRequest::new(
            sdk::ElicitationFormMode::new(
                sdk::ElicitationSessionScope::new("session"),
                sdk::ElicitationSchema::new().string("workspace/name", true),
            ),
            "Name the workspace",
        );
        let params = serde_json::to_value(&request).unwrap();
        let checked: sdk::CreateElicitationRequest =
            serde_json::from_value(params.clone()).unwrap();
        assert_eq!(serde_json::to_value(checked).unwrap(), params);
        assert_eq!(params["mode"], "form");
        assert_eq!(params["sessionId"], "session");
        peer.send(json!({
            "jsonrpc":"2.0", "id":"optional-form",
            "method":"elicitation/create", "params":params
        }))
        .await;
        let answer = peer.recv().await;
        assert_eq!(answer["id"], "optional-form");
        assert_eq!(answer["error"]["code"], -32601);
        assert!(answer.get("result").is_none());
        assert!(peer.callback.waiting.lock().is_empty());
        assert!(peer.callback.sink.view().questions.is_empty());
        assert_eq!(peer.callback.rows.lock().question_serial, 0);

        peer.permission("ordinary-after-elicitation", "session")
            .await;
        opened_question(&peer, "acp-question:1").await;
        peer.answer(1, "acp-question:1", json!({"option_id":"allow-once"}))
            .await;
        let answer = peer.recv().await;
        assert_eq!(answer["id"], "ordinary-after-elicitation");
        assert_eq!(
            answer["result"],
            json!({"outcome":{"outcome":"selected", "optionId":"allow-once"}})
        );
        peer.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn v1_repeated_source_tool_id_gets_fresh_callback_and_refuses_stale_answer() {
    run_peer(spec(None), async |mut peer| {
        peer.new_session().await;
        peer.permission("first-permission", "session").await;
        let first = opened_question(&peer, "acp-question:1").await;
        peer.answer(1, &first.id, json!({"option_id":"allow-once"}))
            .await;
        let answer = peer.recv().await;
        assert_eq!(answer["id"], "first-permission");
        assert_eq!(
            answer["result"],
            json!({"outcome":{"outcome":"selected", "optionId":"allow-once"}})
        );
        peer.permission("second-permission", "session").await;
        let second = opened_question(&peer, "acp-question:2").await;
        assert_eq!(first.fields["source_question_id"], "tool");
        assert_eq!(second.fields["source_question_id"], "tool");
        assert_ne!(first.id, second.id);
        assert_eq!(peer.callback.sink.view().questions, vec![second.clone()]);

        peer.answer(2, &first.id, json!({"option_id":"allow-once"}))
            .await;
        peer.receipt(2, NativeDisposition::Refused).await;
        assert!(peer.callback.waiting.lock().contains_key(&second.id));
        assert_eq!(peer.callback.sink.view().questions, vec![second.clone()]);
        peer.answer(3, &second.id, json!({"option_id":"allow-once"}))
            .await;
        let answer = peer.recv().await;
        assert_eq!(answer["id"], "second-permission");
        assert_eq!(
            answer["result"],
            json!({"outcome":{"outcome":"selected", "optionId":"allow-once"}})
        );
        peer.stop().await;
    })
    .await
    .unwrap();
}
