use super::*;

/// Every transcript row queued on `callback`'s sink, in order.
fn transcript_rows(
    callback: &Callback,
) -> Vec<phux_client_core::session::agent_transcript::TranscriptEntry> {
    let backlog = callback.sink.0.backlog.lock();
    backlog
        .rows
        .iter()
        .filter_map(|(output, _)| {
            let crate::session::Output::Update(NativeUpdate::Transcript { entry }) = output else {
                return None;
            };
            phux_client_core::session::agent_transcript::TranscriptEntry::from_data(entry)
        })
        .collect()
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
    let callback = Callback::new(crate::session::tests::sink(), "custom");
    callback
        .chunk(TranscriptRole::Assistant, "historical".into())
        .unwrap();
    callback.prompt_entry(7, "new user").unwrap();
    callback
        .chunk(TranscriptRole::Assistant, "fresh reply".into())
        .unwrap();
    callback.flush_text().unwrap();
    let final_rows: Vec<_> = transcript_rows(&callback)
        .into_iter()
        .filter(|row| row.is_final)
        .collect();
    assert_eq!(
        final_rows
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>(),
        ["historical", "new user", "fresh reply"]
    );
    assert_ne!(final_rows[0].id, final_rows[2].id);
}

#[test]
fn chunks_preserve_row_identity_until_role_changes() {
    let callback = Callback::new(crate::session::tests::sink(), "cursor");
    callback.rows.lock().prompt_user_id = Some("acp-prompt:7".into());
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
    let rows = transcript_rows(&callback);
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
}
