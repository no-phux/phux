use super::*;

fn sample() -> StateBlob {
    StateBlob {
        version: BLOB_VERSION,
        listener_fd: 7,
        counters: Counters {
            next_session_wire_id: 3,
            next_resource_wire_id: 5,
            next_window_wire_id: 4,
            next_touch_timestamp: 42,
            server_instance: Some([7; 16]),
        },
        sessions: vec![SessionBlob {
            wire_id: 1,
            name: "main".to_owned(),
            window_wire_ids: vec![1, 2],
            active_window: Some(1),
            created_at_unix_nanos: 1_700_000_000_000_000_000,
            last_touched: Some(7),
            root: Some(PathBuf::from("/home/u/proj")),
            keep_empty: true,
            key: None,
        }],
        windows: vec![WindowBlob {
            wire_id: 1,
            session_wire_id: 1,
            pane_wire_ids: vec![1, 2],
            active_resource: Some(2),
            layout: Some(LayoutBlob::Split {
                dir: SplitDirBlob::Horizontal,
                ratio: 0.5,
                left: Box::new(LayoutBlob::Leaf(1)),
                right: Box::new(LayoutBlob::Leaf(2)),
            }),
            last_cwd: Some(PathBuf::from("/home/u/proj/src")),
        }],
        panes: vec![
            PaneBlob {
                wire_id: 1,
                window_wire_id: 1,
                cols: 80,
                rows: 24,
                cell_px: Some((9, 18)),
                cwd: PathBuf::from("/home/u/proj"),
                title: Some("vim".to_owned()),
                osc_title: Some("nvim README.md".to_owned()),
                term: "xterm-256color".to_owned(),
                child_pid: Some(4321),
                master_fd: Some(11),
                vt_replay_bytes: b"\x1b[2J\x1b[Hhello".to_vec(),
                scrollback_bytes: b"old line\r\n".to_vec(),
                retained_exit: None,
                retain_secs: None,
                key: None,
            },
            PaneBlob {
                wire_id: 2,
                window_wire_id: 1,
                cols: 80,
                rows: 24,
                cell_px: None,
                cwd: PathBuf::from("/home/u/proj/src"),
                title: None,
                osc_title: None,
                term: "xterm-256color".to_owned(),
                child_pid: Some(4322),
                master_fd: Some(12),
                vt_replay_bytes: vec![],
                scrollback_bytes: vec![],
                retained_exit: None,
                retain_secs: None,
                key: None,
            },
        ],
        agent_sessions: vec![AgentSessionBlob {
            wire_id: 3,
            parent_wire_id: 2,
            provider: "claude".to_owned(),
            native_id: Some("abc".to_owned()),
            state: Some("working".to_owned()),
            base_seq: 9,
            dropped: 4,
            ended: false,
            records: vec!["{\"seq\":9,\"ts_ms\":1,\"type\":\"prompt\",\"data\":{}}\n".to_owned()],
            key: None,
            native_publications: Vec::new(),
            native_publication_high_water: 0,
        }],
        metadata: vec![MetadataBlob {
            group: Some(1),
            key: "phux.tui.layout/v1/1".to_owned(),
            value: vec![0xa1, 0x01],
        }],
        org: None,
        delegations: Vec::new(),
        native_agents: Vec::new(),
    }
}

#[test]
fn round_trips_through_bytes() {
    let blob = sample();
    let bytes = blob.to_bytes().expect("serialize");
    let back = StateBlob::from_bytes(&bytes).expect("deserialize");
    assert_eq!(blob, back);
}

#[test]
fn rejects_unknown_version() {
    let mut blob = sample();
    blob.version = BLOB_VERSION + 1;
    let bytes = blob.to_bytes().expect("serialize");
    match StateBlob::from_bytes(&bytes) {
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
            "next_resource_wire_id": 1,
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
            "next_resource_wire_id": 1,
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
