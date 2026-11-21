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
