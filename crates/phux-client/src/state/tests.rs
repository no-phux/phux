use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{Command, ErrorCode, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

use super::{Connection, get_state};
use crate::testkit::{ScriptSpec, serve_one};

#[tokio::test]
async fn get_state_skips_unrelated_command_results() {
    let dir = tempfile::tempdir().unwrap();
    let expected = SessionSnapshot::new(SessionId::new(7), WindowId::new(8), ResourceId::local(9));
    // A foreign COMMAND_RESULT ahead of this ack must be skipped.
    let spec = ScriptSpec::new().foreign_ack(99).state(expected.clone());
    let (socket, server) = serve_one(dir.path(), spec);

    let view = get_state(&socket).await.unwrap();
    assert!(
        view.is_complete(),
        "a server that pushed no uncorrelated ERROR is not degraded"
    );
    assert_eq!(*view.snapshot(), expected);
    let seen = server.await.unwrap();
    let Some(FrameKind::Hello {
        client_name,
        client_caps,
        ..
    }) = seen.first()
    else {
        panic!("HELLO must be first, got {seen:?}");
    };
    assert!(client_name.starts_with("phux-client/"));
    assert_eq!(
        client_caps.layers,
        phux_protocol::caps::LayerSet::with(&[phux_protocol::caps::Layer::L3]),
        "control commands advertise the metadata tier used by the shared connection API"
    );
    assert!(
        matches!(
            seen.get(1),
            Some(FrameKind::Command {
                request_id: 0,
                command: Command::GetState { .. }
            })
        ),
        "GET_STATE must follow negotiation on request id 0; got {seen:?}",
    );
    assert_eq!(seen.len(), 2, "exactly one HELLO and one GET_STATE");
}

#[tokio::test]
async fn get_state_hands_back_the_degradation_with_the_snapshot() {
    // A hub's degradation notice ahead of the ack rides with the view.
    let dir = tempfile::tempdir().unwrap();
    let expected = SessionSnapshot::new(SessionId::new(1), WindowId::new(2), ResourceId::local(3));
    let spec = ScriptSpec::new()
        .degradation_notice("satellite build-box is unreachable: link is down")
        .state(expected.clone());
    let (socket, server) = serve_one(dir.path(), spec);

    let view = get_state(&socket).await.unwrap();
    assert!(!view.is_complete(), "one dead satellite is a partial view");
    assert_eq!(*view.snapshot(), expected, "degradation is not failure");
    let (snapshot, degradation) = view.into_parts();
    assert_eq!(snapshot, expected);
    assert_eq!(
        degradation.notices(),
        ["satellite build-box is unreachable: link is down".to_owned()]
    );
    server.await.unwrap();
}

#[tokio::test]
async fn probe_hello_returns_the_negotiated_version_triple() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, server) = serve_one(dir.path(), ScriptSpec::new());

    let mut conn = Connection::connect(&socket).await.unwrap();
    let triple = super::probe_hello(&mut conn).await.unwrap();
    assert_eq!(
        triple,
        (
            phux_protocol::PROTOCOL_VERSION.major,
            phux_protocol::PROTOCOL_VERSION.minor,
            phux_protocol::PROTOCOL_VERSION.patch,
        )
    );
    drop(conn);
    let seen = server.await.unwrap();
    assert!(
        matches!(seen.first(), Some(FrameKind::Hello { .. })),
        "the probe's first frame must be HELLO; got {:?}",
        seen.first()
    );
    assert_eq!(
        seen.iter()
            .filter(|frame| matches!(frame, FrameKind::Hello { .. }))
            .count(),
        1,
        "probing an already-negotiated connection must not emit a second HELLO",
    );
}

#[tokio::test]
async fn peer_pid_names_the_process_behind_the_socket() {
    // The listener is this process, so the peer pid is ours.
    let dir = tempfile::tempdir().unwrap();
    let (socket, server) = serve_one(dir.path(), ScriptSpec::new());

    let conn = Connection::connect(&socket).await.unwrap();
    assert_eq!(
        conn.peer_pid(),
        Some(i32::try_from(std::process::id()).unwrap()),
        "a UDS connection's peer pid must be the listening process"
    );
    drop(conn);
    server.await.unwrap();
}

#[tokio::test]
async fn the_agent_index_is_complete_only_when_every_pane_answered() {
    // Two panes, one record: a definite absence keeps the index complete.
    let dir = tempfile::tempdir().unwrap();
    let spec = ScriptSpec::new().metadata(|scope, key| {
        (key == crate::agent_meta::RESOURCE_AGENT_KEY
            && matches!(
                scope,
                phux_protocol::wire::frame::Scope::Resource(id) if *id == ResourceId::local(1)
            ))
        .then(|| br#"{"name":"reviewer","kind":"claude","state":"working"}"#.to_vec())
    });
    let (socket, server) = serve_one(dir.path(), spec);
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 80, 24),
            ResourceInfo::new(ResourceId::local(2), WindowId::new(1), 80, 24),
        ]);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let index = super::fetch_agent_index(&mut conn, &snapshot).await;
    assert!(index.is_complete());
    assert!(index.get(&ResourceId::local(2)).is_none());
    assert_eq!(index.get(&ResourceId::local(1)).unwrap().name, "reviewer");
    drop(conn);
    server.await.unwrap();

    // A refused read is not an absence: the index is partial.
    let dir = tempfile::tempdir().unwrap();
    let spec = ScriptSpec::new().refuse_metadata(ErrorCode::PermissionDenied, "policy refused");
    let (socket, server) = serve_one(dir.path(), spec);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let index = super::fetch_agent_index(&mut conn, &snapshot).await;
    assert!(
        !index.is_complete(),
        "a refusal must not read as a complete absence"
    );
    drop(conn);
    server.await.unwrap();
}

#[test]
fn degradation_notices_ignores_correlated_errors() {
    // A correlated ERROR is some command's answer, not a notice.
    let frames = vec![
        FrameKind::Error {
            request_id: Some(4),
            code: phux_protocol::wire::frame::ErrorCode::TerminalNotFound,
            message: "someone else's refusal".to_owned(),
        },
        FrameKind::Error {
            request_id: None,
            code: phux_protocol::wire::frame::ErrorCode::UnsupportedSatelliteRoute,
            message: "satellite unreachable".to_owned(),
        },
    ];
    assert_eq!(
        super::degradation_notices(&frames),
        vec!["satellite unreachable".to_owned()]
    );
}

/// Long enough that a loaded machine cannot trip it, short enough that a
/// genuine wedge fails this test instead of hanging the run.
const WEDGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[tokio::test]
async fn refused_tag_read_ends_the_index_instead_of_wedging_it() {
    // Regression: a correlated ERROR on one tag read once wedged every
    // `#tag` selector forever. The timeout is the assertion.
    let dir = tempfile::tempdir().unwrap();
    let spec = ScriptSpec::new().refuse_metadata(ErrorCode::PermissionDenied, "policy refused");
    let (socket, server) = serve_one(dir.path(), spec);

    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 80, 24),
            ResourceInfo::new(ResourceId::local(2), WindowId::new(1), 80, 24),
        ]);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let index = tokio::time::timeout(WEDGE_TIMEOUT, super::fetch_tag_index(&mut conn, &snapshot))
        .await
        .expect("a refused read must end the index; a timeout here is the wedge itself");

    assert!(index.is_empty(), "got {index:?}");
    drop(conn);
    server.await.unwrap();
}
