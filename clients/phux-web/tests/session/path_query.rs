//! Host-side path queries: negotiation, stale replies, and the paste they emit.

use super::*;

fn path_reply(request_id: u32, path: &str) -> FrameKind {
    FrameKind::PathResults {
        request_id,
        result: Ok(PathResults {
            root: "/work".to_owned(),
            parent: Some("/".to_owned()),
            rows: vec![PathRow {
                path: path.to_owned(),
                kind: PathKind::File,
            }],
            status: PathStatus::Complete,
        }),
    }
}

#[wasm_bindgen_test]
async fn satellite_pane_queries_its_own_host_not_the_hub() {
    let mut session = path_session_with_panes(
        true,
        vec![ResourceId::satellite(SatelliteHost::new("build"), 101)],
    )
    .await;
    let request = session.path_query_frame("~", "src", true).expect("query");
    let (frame, _) = FrameKind::decode(&request).expect("decode");
    let FrameKind::PathQuery { host, .. } = frame else {
        panic!("not a query")
    };
    assert_eq!(host, Some(SatelliteHost::new("build")));
}

#[wasm_bindgen_test]
async fn path_query_negotiation_and_stale_replies() {
    let mut old = path_session(false, false).await;
    assert!(!old.path_query_supported());
    assert!(old.path_query_frame("~", "src", true).is_none());
    let mut session = path_session(true, false).await;
    let first = session
        .path_query_frame("~", "src", true)
        .expect("negotiated query");
    let (first, _) = FrameKind::decode(&first).unwrap();
    let FrameKind::PathQuery {
        request_id: old_id,
        host: None,
        recursive: true,
        ..
    } = first
    else {
        panic!("wrong query")
    };
    let second = session.path_query_frame("/work", "", false).unwrap();
    let (second, _) = FrameKind::decode(&second).unwrap();
    let FrameKind::PathQuery {
        request_id: new_id,
        host: None,
        recursive: false,
        ..
    } = second
    else {
        panic!("wrong browse")
    };
    assert_ne!(old_id, new_id);
    session.on_frame(path_reply(old_id, "/work/stale"));
    assert!(session.path_pending());
    assert!(session.path_results().is_none());
    session.on_frame(path_reply(new_id, "/work/new"));
    assert_eq!(session.path_results().unwrap().rows[0].path, "/work/new");
    session.cancel_path_query();
    session.on_frame(path_reply(new_id, "/work/new"));
    assert!(session.path_results().is_none());
}

#[wasm_bindgen_test]
async fn path_selection_keeps_pane_and_emits_only_editable_paste() {
    let mut session = path_session(true, false).await;
    let query = session.path_query_frame("/work", "a", true).unwrap();
    let (FrameKind::PathQuery { request_id, .. }, _) = FrameKind::decode(&query).unwrap() else {
        panic!("query")
    };
    session.on_frame(path_reply(request_id, "/work/a b'$(echo nope).txt"));
    let paste = session.paste_path_row(0).expect("eligible paste");
    let (frame, rest) = FrameKind::decode(&paste).unwrap();
    assert!(rest.is_empty());
    let FrameKind::InputPaste { terminal_id, event } = frame else {
        panic!("paste only, never Enter")
    };
    assert_eq!(terminal_id, ResourceId::local(101));
    assert_eq!(event.data, b"'/work/a b'\\''$(echo nope).txt'");
    assert_eq!(
        event.trust,
        phux_protocol::input::paste::PasteTrust::Untrusted
    );
    assert!(
        session.key_frame(key()).is_some(),
        "selection retains input lease"
    );
    assert!(
        session.paste_path_row(0).is_none(),
        "selection cannot replay"
    );
    let query = session.path_query_frame("/work", "a", true).unwrap();
    let (FrameKind::PathQuery { request_id, .. }, _) = FrameKind::decode(&query).unwrap() else {
        panic!("query")
    };
    session.on_frame(path_reply(request_id, "/work/unsafe\ncommand"));
    assert!(
        session.paste_path_row(0).is_none(),
        "control characters cannot become terminal input"
    );
}

#[wasm_bindgen_test]
async fn focus_switch_discards_query_and_cannot_paste_into_next_pane() {
    let mut session = path_session(true, true).await;
    let query = session.path_query_frame("/work", "a", true).unwrap();
    let (FrameKind::PathQuery { request_id, .. }, _) = FrameKind::decode(&query).unwrap() else {
        panic!("query")
    };
    session.on_frame(FrameKind::ResourceClosed {
        terminal_id: ResourceId::local(101),
        exit_status: None,
        reason: phux_protocol::wire::frame::CloseReason::ParentClosed,
        signal: None,
    });
    session.on_frame(path_reply(request_id, "/work/a"));
    assert!(session.path_results().is_none());
    assert!(session.paste_path_row(0).is_none());
    assert!(
        session.key_frame(key()).is_some(),
        "next pane remains usable"
    );
}
