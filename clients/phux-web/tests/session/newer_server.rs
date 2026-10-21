//! A newer 1.x server's additions: values this build does not know degrade
//! instead of failing the session, and correlated refusals settle requests.

use super::*;
use phux_protocol::wire::frame::{
    ErrorCode, HistoryRejectionReason, HistoryTombstoneReason, SpawnError, SpawnResult,
};

fn unsupported(request_id: u32) -> FrameKind {
    FrameKind::Error {
        request_id: Some(request_id),
        code: ErrorCode::Unsupported,
        message: "a newer server does not know this".to_owned(),
    }
}

#[wasm_bindgen_test]
async fn a_path_query_answered_unsupported_settles() {
    let mut session = path_session(true, false).await;
    let request = session.path_query_frame("~", "src", true).expect("query");
    let FrameKind::PathQuery { request_id, .. } = decode_one(&request) else {
        panic!("not a query")
    };
    session.on_frame(unsupported(request_id));
    assert!(!session.path_pending());
    assert!(session.path_error().is_some());
    assert!(!session.is_failed());
}

#[wasm_bindgen_test]
async fn a_split_refused_with_an_unknown_tag_or_unsupported_leaves_the_session() {
    let mut session = path_session(false, false).await;
    let request_id = requested_spawn(&mut session, "vertical");
    session.on_frame(FrameKind::ResourceSpawned {
        request_id,
        result: SpawnResult::Err(SpawnError::Unknown(0xFE)),
    });
    assert!(!session.pane_pending());
    // A newer server's refusal does not say whether a pane exists.
    assert!(session.pane_error().unwrap().contains("outcome is unknown"));
    let request_id = requested_spawn(&mut session, "vertical");
    session.on_frame(unsupported(request_id));
    assert!(!session.pane_pending());
    assert!(!session.pane_error().unwrap().contains("outcome is unknown"));
    let request_id = requested_spawn(&mut session, "vertical");
    session.on_frame(FrameKind::Error {
        request_id: Some(request_id),
        code: ErrorCode::UNREADABLE_REPLY,
        message: "unreadable".to_owned(),
    });
    assert!(!session.pane_pending());
    assert!(session.pane_error().unwrap().contains("outcome is unknown"));
    assert!(!session.is_failed());
}

/// An unknown history reason is reduced exactly like a known one (here, an
/// unrequested cursor either way), never as an unsupported-reason failure.
#[wasm_bindgen_test]
async fn unknown_history_reasons_reduce_like_known_ones() {
    async fn after(
        tombstone: HistoryTombstoneReason,
        rejection: HistoryRejectionReason,
    ) -> (bool, Option<String>) {
        let mut session = path_session(false, false).await;
        let terminal_id = ResourceId::local(101);
        let first = session.on_frame(FrameKind::HistoryTombstone {
            terminal_id: terminal_id.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            cursor: Bytes::from_static(b"cursor"),
            reason: tombstone,
        });
        let second = session.on_frame(FrameKind::HistoryRejected {
            terminal_id,
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            cursor: Bytes::from_static(b"cursor"),
            reason: rejection,
            required_bytes: 4096,
            required_rows: 16,
        });
        let fatal = first.fatal.or(second.fatal);
        assert!(
            fatal
                .as_deref()
                .is_none_or(|m| !m.contains("unsupported history")),
            "{fatal:?}"
        );
        (session.is_failed(), fatal)
    }
    let known = after(
        HistoryTombstoneReason::Released,
        HistoryRejectionReason::ZeroLimit,
    )
    .await;
    let unknown = after(
        HistoryTombstoneReason::Unknown,
        HistoryRejectionReason::Unknown,
    )
    .await;
    assert_eq!(known, unknown);
}

/// A frame before `HELLO` or a missed `HELLO` deadline now closes with
/// `MALFORMED_MESSAGE`: a transient fault, so the page keeps redialing.
#[wasm_bindgen_test]
async fn a_malformed_message_close_stays_retryable() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 20, 3);
    session.on_frame(FrameKind::Error {
        request_id: None,
        code: ErrorCode::MalformedMessage,
        message: "HELLO deadline expired".to_owned(),
    });
    assert!(!session.is_version_refused());
}
