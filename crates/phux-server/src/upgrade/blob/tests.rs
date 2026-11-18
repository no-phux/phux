use super::*;

#[test]
fn rejects_unknown_version() {
    let json = format!(
        r#"{{"version": {}, "listener_fd": 3, "counters": {{"next_session_wire_id": 1,
        "next_terminal_wire_id": 1, "next_window_wire_id": 1, "next_touch_timestamp": 1}},
        "sessions": [], "windows": [], "panes": []}}"#,
        BLOB_VERSION + 1
    );
    match StateBlob::from_bytes(json.as_bytes()) {
        Err(BlobError::Version { found, expected }) => {
            assert_eq!(found, BLOB_VERSION + 1);
            assert_eq!(expected, BLOB_VERSION);
        }
        other => panic!("expected version error, got {other:?}"),
    }
}

#[test]
fn deserialize_tolerates_missing_additive_fields() {
    // A minimal blob omitting every `#[serde(default)]` field still loads,
    // proving older writers stay readable.
    let json = r#"{
        "version": 1,
        "listener_fd": 3,
        "counters": {
            "next_session_wire_id": 1,
            "next_terminal_wire_id": 1,
            "next_window_wire_id": 1,
            "next_touch_timestamp": 1
        },
        "sessions": [{
            "wire_id": 1,
            "name": "s",
            "window_wire_ids": [1],
            "active_window": null
        }],
        "windows": [{
            "wire_id": 1,
            "session_wire_id": 1,
            "pane_wire_ids": [1],
            "active_pane": null
        }],
        "panes": [{
            "wire_id": 1,
            "window_wire_id": 1,
            "cols": 80,
            "rows": 24,
            "cwd": "/tmp",
            "term": "xterm-256color",
            "child_pid": 9,
            "master_fd": 10,
            "vt_replay_bytes": []
        }]
    }"#;
    let blob = StateBlob::from_bytes(json.as_bytes()).expect("tolerant decode");
    assert_eq!(blob.sessions[0].root, None);
    assert_eq!(blob.windows[0].layout, None);
    assert_eq!(blob.panes[0].scrollback_bytes, Vec::<u8>::new());
    assert_eq!(blob.panes[0].cell_px, None);
    assert_eq!(
        blob.counters.server_instance, None,
        "ADR-0109: pre-token image"
    );
    assert!(
        blob.agent_sessions.is_empty(),
        "an image that predates carried agent sessions carries none"
    );
}

#[test]
fn deserialize_ignores_unknown_future_fields() {
    // A newer writer's extra field must not break an older reader.
    let json = r#"{
        "version": 1,
        "listener_fd": 3,
        "counters": {
            "next_session_wire_id": 1,
            "next_terminal_wire_id": 1,
            "next_window_wire_id": 1,
            "next_touch_timestamp": 1
        },
        "sessions": [],
        "windows": [],
        "panes": [],
        "some_future_field": {"nested": [1, 2, 3]}
    }"#;
    let blob = StateBlob::from_bytes(json.as_bytes()).expect("forward-tolerant decode");
    assert!(blob.sessions.is_empty());
}
