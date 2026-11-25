#![allow(clippy::unwrap_used, reason = "tests")]

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn rebootstrap_selects_attached_identity_without_reusable_name() {
    let (a, b) = tokio::net::UnixStream::pair().unwrap();
    let mut conn = Connection::from_stream(a);
    let mut peer = Connection::from_stream(b);
    let id = SessionId::new(41);
    let attach_id = restart_attach(&mut conn, Some(id), &mut HeadlessCompletion::default())
        .await
        .unwrap();
    let frame = peer.recv().await.unwrap();
    assert!(matches!(frame, FrameKind::Attach {
        attach_id: sent, target: AttachTarget::ById(selected), viewport, ..
    } if sent == attach_id && selected == id && viewport.cols == 0 && viewport.rows == 0));
}

#[tokio::test(flavor = "current_thread")]
async fn rebootstrap_without_attached_identity_does_not_select_or_create() {
    let (a, b) = tokio::net::UnixStream::pair().unwrap();
    let mut conn = Connection::from_stream(a);
    let mut peer = Connection::from_stream(b);
    let result = restart_attach(&mut conn, None, &mut HeadlessCompletion::default()).await;
    assert!(matches!(result, Err(AttachError::Protocol(_))));
    drop(conn);
    assert!(
        peer.recv().await.is_err(),
        "no fallback name attach or creation"
    );
}
