//! Relayed directory listings and path queries.

use super::*;

pub(super) fn listing_request(
    path: &str,
) -> (RelayRequest, oneshot::Receiver<DirectoryListingResult>) {
    let (reply, rx) = oneshot::channel();
    (
        RelayRequest::ListDirectory {
            path: path.to_owned(),
            reply,
        },
        rx,
    )
}

pub(super) fn refusal_message(result: DirectoryListingResult) -> String {
    let refusal = result.expect_err("expected a refusal");
    assert_eq!(refusal.code, DirectoryErrorCode::Other);
    refusal.message
}

#[test]
fn a_satellite_without_list_directory_is_refused_before_the_link() {
    let mut session = negotiated(&[]);
    let (request, mut rx) = listing_request("/srv");
    assert!(session.handle_request_checked(request).is_none());
    let message = refusal_message(rx.try_recv().expect("resolved at once"));
    assert!(
        message.contains("devbox") && message.contains("LIST_DIRECTORY"),
        "{message}"
    );
}

#[test]
fn a_correlated_error_and_teardown_both_refuse_pending_listings() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (first, mut first_rx) = listing_request("/a");
    let FrameKind::ListDirectory { request_id, .. } = decode(&session.handle_request(first)) else {
        panic!("expected LIST_DIRECTORY");
    };
    session.feed(&FrameKind::Error {
        request_id: Some(request_id),
        code: ErrorCode::InvalidCommand,
        message: "nope".to_owned(),
    });
    let message = refusal_message(first_rx.try_recv().expect("resolved"));
    assert!(message.contains("refused the listing"), "{message}");

    let (second, mut second_rx) = listing_request("/b");
    let _ = session.handle_request(second);
    session.teardown("link lost");
    let refusal = second_rx
        .try_recv()
        .expect("resolved")
        .expect_err("refused");
    assert_eq!(refusal.path, "/b");
    assert!(
        refusal.message.contains("unreachable: link lost"),
        "{}",
        refusal.message
    );
}

// --- relayed PATH_QUERY ----------------------------------------

pub(super) fn path_request(root: &str) -> (RelayRequest, oneshot::Receiver<PathQueryResult>) {
    let (reply, rx) = oneshot::channel();
    (
        RelayRequest::PathQuery {
            root: root.to_owned(),
            query: "src".to_owned(),
            recursive: true,
            reply,
        },
        rx,
    )
}

pub(super) fn path_session() -> RelaySession {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    session.set_satellite_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery]));
    session
}

pub(super) fn path_error(result: PathQueryResult, root: &str) -> String {
    let refusal = result.expect_err("expected path refusal");
    assert_eq!(refusal.code, PathErrorCode::Other);
    assert_eq!(refusal.root, root);
    assert!(refusal.message.contains("devbox"));
    refusal.message
}

#[test]
fn older_satellite_refuses_path_query_before_forwarding() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (request, mut rx) = path_request("~/work");
    assert!(session.handle_request_checked(request).is_none());
    assert!(path_error(rx.try_recv().unwrap(), "~/work").contains("PATH_QUERY"));
    assert_eq!(session.pending_request_count(), 0);
}

#[test]
fn path_query_correlated_error_and_disconnect_are_typed_refusals() {
    let mut session = path_session();
    let (request, mut rx) = path_request("/first");
    let FrameKind::PathQuery { request_id, .. } = decode(&session.handle_request(request)) else {
        panic!("expected PATH_QUERY");
    };
    session.feed(&FrameKind::Error {
        request_id: Some(request_id),
        code: ErrorCode::InvalidCommand,
        message: "denied".to_owned(),
    });
    assert!(path_error(rx.try_recv().unwrap(), "/first").contains("denied"));

    let (request, mut rx) = path_request("/second");
    let _ = session.handle_request(request);
    session.teardown("link lost");
    assert!(path_error(rx.try_recv().unwrap(), "/second").contains("link lost"));
}

#[test]
fn satellite_path_refusal_preserves_its_specific_error_code() {
    let mut session = path_session();
    let (request, mut rx) = path_request("/private");
    let FrameKind::PathQuery { request_id, .. } = decode(&session.handle_request(request)) else {
        panic!("expected PATH_QUERY");
    };
    let refusal = PathQueryError {
        root: "/private".to_owned(),
        code: PathErrorCode::PermissionDenied,
        message: "access denied".to_owned(),
    };
    session.feed(&FrameKind::PathResults {
        request_id,
        result: Err(refusal.clone()),
    });
    assert_eq!(rx.try_recv().unwrap(), Err(refusal));
}

#[test]
fn abandoned_listings_and_path_queries_are_pruned_and_late_results_dropped() {
    let mut session = path_session();
    let (listing, listing_rx) = listing_request("/srv");
    let _ = session.handle_request(listing);
    let (request, rx) = path_request("/tmp");
    let FrameKind::PathQuery { request_id, .. } = decode(&session.handle_request(request)) else {
        panic!("expected PATH_QUERY");
    };
    assert_eq!(session.prune_abandoned(), 0);
    drop((listing_rx, rx));
    assert_eq!(session.prune_abandoned(), 2);
    session.feed(&FrameKind::PathResults {
        request_id,
        result: Err(path_refusal("/tmp", "late")),
    });
}

#[tokio::test]
async fn disconnected_listings_and_path_queries_fail_fast_naming_the_host() {
    let (request, rx) = listing_request("/srv");
    fail_fast(request, &host(), "backoff");
    let message = refusal_message(rx.await.expect("resolved"));
    assert!(
        message.contains("devbox is unreachable: backoff"),
        "{message}"
    );
    let (request, rx) = path_request("/tmp");
    fail_fast(request, &host(), "backoff");
    assert!(path_error(rx.await.unwrap(), "/tmp").contains("backoff"));
}

/// A link that stays up but never answers is refused at the deadline,
/// naming the host and the path it was asked for.
#[tokio::test(start_paused = true)]
async fn listings_and_path_queries_time_out_against_a_silent_satellite() {
    let (handle, _mailbox) = RelayHandle::new(host());
    let refusal = handle
        .list_directory("/srv".to_owned())
        .await
        .expect_err("expected a refusal");
    assert_eq!(
        (refusal.code, refusal.path.as_str()),
        (DirectoryErrorCode::Other, "/srv")
    );
    let expected = "devbox did not answer the listing within 10s";
    assert!(refusal.message.contains(expected), "{}", refusal.message);
    let result = handle
        .path_query("~/work".to_owned(), "src".to_owned(), true)
        .await;
    assert!(path_error(result, "~/work").contains("within 10s"));
}

#[tokio::test]
async fn saturated_and_closed_path_query_mailboxes_refuse() {
    let (handle, mut mailbox) = RelayHandle::new(host());
    for _ in 0..RELAY_MAILBOX {
        handle.forward(FrameKind::Ping { nonce: 1 });
    }
    let query = || handle.path_query("/tmp".to_owned(), String::new(), false);
    assert!(path_error(query().await, "/tmp").contains("saturated"));
    mailbox.requests.close();
    while mailbox.requests.try_recv().is_ok() {}
    drop(mailbox);
    assert!(path_error(query().await, "/tmp").contains("down"));
}
