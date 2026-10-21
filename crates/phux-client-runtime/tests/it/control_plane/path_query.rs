//! Host-side path queries: the extended capability, correlation, and bounds.

use super::*;

#[test]
fn path_query_requires_extended_capability_and_correlates_typed_results() {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let _ = plane.take_outbound();
    assert_eq!(plane.path_query("/home".into(), "a".into(), true, None), 0);
    assert!(
        plane.take_outbound().is_empty(),
        "older server must not hang picker"
    );

    plane.connection_opened();
    let _ = plane.take_outbound();
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk { server_caps, .. } = &mut hello {
        *server_caps = server_caps
            .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery]));
    }
    plane.feed(hello).unwrap();
    let _ = plane.take_outbound();
    let id = plane.path_query(
        "/home".into(),
        "a".into(),
        true,
        Some(SatelliteHost::new("peer")),
    );
    assert_ne!(id, 0);
    assert!(
        matches!(&decode(&plane.take_outbound()[0]), FrameKind::PathQuery {
        request_id, root, query, recursive: true, host: Some(host)
    } if *request_id == id && root == "/home" && query == "a" && host.as_str() == "peer")
    );
    plane
        .feed(FrameKind::PathResults {
            request_id: id,
            result: Ok(PathResults {
                root: "/home".into(),
                parent: Some("/".into()),
                rows: vec![PathRow {
                    path: "/home/a b".into(),
                    kind: PathKind::Symlink,
                }],
                status: PathStatus::Warming,
            }),
        })
        .unwrap();
    let answers = plane.take_path_answers();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].request_id, id);
    assert_eq!(answers[0].rows[0].kind, PathMatchKind::Symlink);
    assert_eq!(answers[0].rows[0].path, "/home/a b");
    assert_eq!(answers[0].status, Some(PathSearchStatus::Warming));
    assert_eq!(answers[0].failure, None);
    plane
        .feed(FrameKind::PathResults {
            request_id: id,
            result: Err(phux_protocol::wire::frame::PathQueryError {
                root: "/home".into(),
                code: phux_protocol::wire::frame::PathErrorCode::NotFound,
                message: "late".into(),
            }),
        })
        .unwrap();
    assert!(
        plane.take_path_answers().is_empty(),
        "duplicate must not satisfy a new query"
    );
}

#[test]
fn path_queries_are_bounded_and_connection_scoped() {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    let _ = plane.take_outbound();
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk { server_caps, .. } = &mut hello {
        *server_caps = server_caps
            .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery]));
    }
    plane.feed(hello).unwrap();
    let _ = plane.take_outbound();
    let ids: Vec<_> = (0..MAX_PATH_ANSWERS)
        .map(|_| plane.path_query("/tmp".into(), String::new(), false, None))
        .collect();
    assert!(ids.iter().all(|id| *id != 0));
    let before = plane.take_outbound();
    assert_eq!(before.len(), MAX_PATH_ANSWERS);
    assert_eq!(
        plane.path_query("/tmp".into(), String::new(), false, None),
        0
    );
    assert!(plane.take_outbound().is_empty());
    plane
        .feed(FrameKind::Error {
            request_id: Some(ids[0]),
            code: ErrorCode::InvalidCommand,
            message: "refused".into(),
        })
        .unwrap();
    plane.connection_lost(None);
    let answers = plane.take_path_answers();
    assert_eq!(answers.len(), MAX_PATH_ANSWERS);
    assert_eq!(
        answers
            .iter()
            .find(|answer| answer.request_id == ids[0])
            .unwrap()
            .failure,
        Some(PathFailure::Other)
    );
    assert_eq!(
        answers
            .iter()
            .filter(|answer| answer.failure == Some(PathFailure::Unanswered))
            .count(),
        MAX_PATH_ANSWERS - 1
    );
    assert_eq!(
        plane.path_query("/tmp".into(), String::new(), false, None),
        0
    );
    // A late reply from the old socket cannot resolve any new correlation.
    plane.connection_opened();
    plane
        .feed(FrameKind::PathResults {
            request_id: ids[1],
            result: Ok(PathResults {
                root: "/tmp".into(),
                parent: Some("/".into()),
                rows: Vec::new(),
                status: PathStatus::Complete,
            }),
        })
        .unwrap_err(); // no server frame before HELLO_OK
}
