use phux_protocol::ids::WindowId;
use phux_protocol::wire::frame::{ErrorCode, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, WindowInfo};

use super::*;
use crate::layout::{Group, LayoutState};
use crate::testkit::{ScriptSpec, connect_one, serve_one};

fn tid(id: u32) -> ResourceId {
    ResourceId::local(id)
}

fn split(left: u32, right: u32) -> LayoutNode {
    LayoutNode::Split {
        dir: SplitDir::Horizontal,
        ratio: 0.5,
        left: Box::new(LayoutNode::Leaf(tid(left))),
        right: Box::new(LayoutNode::Leaf(tid(right))),
    }
}

fn workspace(name: &str, tree: LayoutNode, focus: u32) -> ArrangementDoc {
    ArrangementDoc {
        groups: vec![Group::new(
            name.to_owned(),
            LayoutState {
                tree: Some(tree),
                focus: Some(tid(focus)),
            },
        )],
        active: 0,
    }
}

fn snapshot(source_in_destination: bool, source_session_present: bool) -> SessionSnapshot {
    let source_session = SessionId::new(1);
    let destination_session = SessionId::new(2);
    let source_window = WindowId::new(10);
    let destination_window = WindowId::new(20);
    let mut sessions = vec![SessionInfo::new(destination_session, "dest")];
    let mut windows = vec![WindowInfo::new(
        destination_window,
        destination_session,
        "target",
    )];
    if source_session_present {
        sessions.insert(0, SessionInfo::new(source_session, "source"));
        windows.insert(0, WindowInfo::new(source_window, source_session, "origin"));
    }
    SessionSnapshot::new(destination_session, destination_window, tid(3))
        .with_sessions(sessions)
        .with_windows(windows)
        .with_resources(vec![
            ResourceInfo::new(
                tid(1),
                if source_in_destination {
                    destination_window
                } else {
                    source_window
                },
                80,
                24,
            ),
            ResourceInfo::new(tid(2), source_window, 80, 24),
            ResourceInfo::new(tid(3), destination_window, 80, 24),
        ])
}

/// A move-capable server answering `states` in order, `result` to the
/// move, with source `origin/{1,2}` and destination `target/3` layouts
/// stored under `keys` (source, destination).
fn cross_spec(
    states: impl IntoIterator<Item = SessionSnapshot>,
    result: MoveResult,
    (source_key, destination_key): (&str, &str),
    source: LayoutNode,
) -> ScriptSpec {
    let group = Scope::Group(DEFAULT_LAYOUT_GROUP_ID);
    ScriptSpec::new()
        .states(states)
        .move_result(result)
        .stored_metadata(
            group.clone(),
            source_key,
            workspace("origin", source, 1).encode_cbor().unwrap(),
        )
        .stored_metadata(
            group,
            destination_key,
            workspace("target", LayoutNode::Leaf(tid(3)), 3)
                .encode_cbor()
                .unwrap(),
        )
}

/// Named envelopes: the only cross-session moves that write layout
/// metadata (the default layout is the arrangement record).
fn projection_keys() -> (String, String) {
    ("src.layout/v1/1".to_owned(), "dst.layout/v1/2".to_owned())
}

fn spawned_or_moved(seen: &[FrameKind]) -> Vec<(ResourceId, ResourceId)> {
    seen.iter()
        .filter_map(|frame| match frame {
            FrameKind::MoveResource {
                terminal,
                owner_terminal,
                ..
            } => Some((terminal.clone(), owner_terminal.clone())),
            FrameKind::SpawnResource { .. } => panic!("a move must never spawn"),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn same_session_move_changes_layout_without_recreating_the_terminal() {
    let session = SessionId::new(1);
    let window = WindowId::new(10);
    let snapshot = SessionSnapshot::new(session, window, tid(1))
        .with_sessions(vec![SessionInfo::new(session, "work")])
        .with_windows(vec![WindowInfo::new(window, session, "main")])
        .with_resources(vec![
            ResourceInfo::new(tid(1), window, 80, 24),
            ResourceInfo::new(tid(2), window, 80, 24),
            ResourceInfo::new(tid(3), window, 80, 24),
        ]);
    let initial = workspace("main", split(1, 2), 2);
    let key = "app.layout/v1/1".to_owned();
    let spec = ScriptSpec::new().state(snapshot).stored_metadata(
        Scope::Group(DEFAULT_LAYOUT_GROUP_ID),
        &key,
        initial.encode_cbor().unwrap(),
    );
    let (_dir, mut conn, server) = connect_one(spec).await;

    let outcome = move_pane(&mut conn, tid(1), tid(2), SplitDir::Vertical, 0.6, &[key])
        .await
        .unwrap();
    assert!(!outcome.cross_session);
    assert_eq!(
        outcome.destination_workspace.groups[0].state.focus,
        Some(tid(1)),
        "focus follows the moved Terminal identity"
    );
    drop(conn);
    assert!(spawned_or_moved(&server.await.unwrap()).is_empty());
}

#[tokio::test]
async fn cross_session_move_reparents_the_same_id_and_cleans_the_source_layout() {
    let (source_key, destination_key) = projection_keys();
    let spec = cross_spec(
        [snapshot(false, true), snapshot(true, true)],
        MoveResult::Ok(tid(1)),
        (&source_key, &destination_key),
        split(1, 2),
    );
    let (_dir, mut conn, server) = connect_one(spec).await;

    // Destination-first: keys match by embedded session id, not position.
    let keys = [destination_key.clone(), source_key.clone()];
    let outcome = move_pane(&mut conn, tid(1), tid(3), SplitDir::Horizontal, 0.5, &keys)
        .await
        .unwrap();
    assert!(outcome.cross_session);
    assert_eq!(
        leaves(
            outcome.destination_workspace.groups[0]
                .state
                .tree
                .as_ref()
                .unwrap()
        ),
        vec![tid(3), tid(1)]
    );
    assert_eq!(
        outcome.destination_workspace.groups[0].state.focus,
        Some(tid(1))
    );
    drop(conn);
    let seen = server.await.unwrap();
    assert_eq!(
        spawned_or_moved(&seen),
        vec![(tid(1), tid(3))],
        "the existing identity is reparented once"
    );
    let source_write = seen
        .iter()
        .find_map(|frame| match frame {
            FrameKind::SetMetadata { key, value, .. } if key == &source_key => {
                ArrangementDoc::decode_cbor(value).ok()
            }
            _ => None,
        })
        .expect("source cleanup write");
    assert_eq!(
        leaves(source_write.groups[0].state.tree.as_ref().unwrap()),
        vec![tid(2)]
    );
    assert!(
        !seen.iter().any(|frame| matches!(
            frame,
            FrameKind::SetMetadata { key, .. } if key == &crate::layout::layout_key(SessionId::new(2))
        )),
        "the default destination key must never be touched"
    );
}

#[tokio::test]
async fn last_pane_move_reaps_source_layout_instead_of_recreating_a_pane() {
    let (source_key, destination_key) = projection_keys();
    let spec = cross_spec(
        [snapshot(false, true), snapshot(true, false)],
        MoveResult::Ok(tid(1)),
        (&source_key, &destination_key),
        LayoutNode::Leaf(tid(1)),
    );
    let (_dir, mut conn, server) = connect_one(spec).await;
    let keys = [source_key.clone(), destination_key];
    move_pane(&mut conn, tid(1), tid(3), SplitDir::Horizontal, 0.5, &keys)
        .await
        .unwrap();
    drop(conn);
    let seen = server.await.unwrap();
    assert!(
        seen.iter().any(
            |frame| matches!(frame, FrameKind::DeleteMetadata { key, .. } if key == &source_key)
        )
    );
    spawned_or_moved(&seen);
}

#[tokio::test]
async fn destination_layout_failure_rolls_ownership_back_and_reports_failure() {
    let spec = ScriptSpec::new()
        .states([
            snapshot(false, true),
            snapshot(true, true),
            snapshot(true, true),
            snapshot(false, true),
        ])
        .move_result(MoveResult::Ok(tid(1)))
        .refuse_metadata(ErrorCode::PermissionDenied, "layout blocked");
    let (_dir, mut conn, server) = connect_one(spec).await;
    let keys: [String; 2] = projection_keys().into();
    let error = move_pane(&mut conn, tid(1), tid(3), SplitDir::Horizontal, 0.5, &keys)
        .await
        .unwrap_err();
    assert!(
        matches!(error, PaneMoveError::DestinationLayout { rollback, .. } if rollback.contains("moved back"))
    );
    drop(conn);
    assert_eq!(
        spawned_or_moved(&server.await.unwrap()),
        vec![(tid(1), tid(3)), (tid(1), tid(2))]
    );
}

/// Naming only one side's projection is refused before any write.
#[tokio::test]
async fn cross_session_move_with_one_projection_key_is_refused() {
    let spec = ScriptSpec::new().states([snapshot(false, true)]);
    let (_dir, mut conn, server) = connect_one(spec).await;
    let error = move_pane(
        &mut conn,
        tid(1),
        tid(3),
        SplitDir::Horizontal,
        0.5,
        &["myapp.layout/v1/1".to_owned()],
    )
    .await
    .unwrap_err();
    assert!(matches!(error, PaneMoveError::ProjectionArityMismatch));
    drop(conn);
    let seen = server.await.unwrap();
    assert!(
        seen.iter()
            .all(|frame| !matches!(frame, FrameKind::SetMetadata { .. })),
        "an arity refusal must happen before any layout write"
    );
}

#[test]
fn single_session_projection_arity_and_validation() {
    let session = SessionId::new(1);
    assert_eq!(
        resolve_single_session_projection(&[], session).unwrap(),
        None
    );
    assert_eq!(
        resolve_single_session_projection(&["myapp.layout/v1/1".to_owned()], session).unwrap(),
        Some("myapp.layout/v1/1".to_owned())
    );
    assert!(matches!(
        resolve_single_session_projection(
            &["a.layout/v1/1".to_owned(), "b.layout/v1/1".to_owned()],
            session
        ),
        Err(PaneMoveError::ProjectionArityTooMany)
    ));
    assert!(matches!(
        resolve_single_session_projection(&["myapp.layout/v1/2".to_owned()], session),
        Err(PaneMoveError::Layout(LayoutOpsError::InvalidProjectionKey(
            _
        )))
    ));
}

#[test]
fn cross_session_projection_resolution_matches_by_embedded_session_id() {
    let source = SessionId::new(1);
    let destination = SessionId::new(2);
    assert!(matches!(
        resolve_cross_session_projection(&[], source, destination),
        Err(PaneMoveError::ProjectionArityMismatch)
    ));
    assert!(matches!(
        resolve_cross_session_projection(&["a.layout/v1/1".to_owned()], source, destination),
        Err(PaneMoveError::ProjectionArityMismatch)
    ));
    assert!(matches!(
        resolve_cross_session_projection(
            &["a.layout/v1/1".to_owned(), "b.layout/v1/1".to_owned()],
            source,
            destination
        ),
        Err(PaneMoveError::ProjectionArityMismatch)
    ));
    let (resolved_source, resolved_destination) = resolve_cross_session_projection(
        &["b.layout/v1/2".to_owned(), "a.layout/v1/1".to_owned()],
        source,
        destination,
    )
    .unwrap();
    assert_eq!(resolved_source, "a.layout/v1/1");
    assert_eq!(resolved_destination, "b.layout/v1/2");
}

#[tokio::test]
async fn a_scoped_denial_refuses_the_move_instead_of_wedging_it() {
    // An out-of-scope MOVE_RESOURCE is refused with the move's own reply
    // (workload-auth §7); the move must end on it.
    let (source_key, destination_key) = projection_keys();
    let spec = cross_spec(
        [snapshot(false, true), snapshot(true, true)],
        MoveResult::Err(MoveError::MoveFailed("permission denied".to_owned())),
        (&source_key, &destination_key),
        split(1, 2),
    );
    let (_dir, mut conn, server) = connect_one(spec).await;
    let keys = [source_key, destination_key];

    let refused = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        move_pane(&mut conn, tid(1), tid(3), SplitDir::Horizontal, 0.5, &keys),
    )
    .await
    .expect("a denied move must return; a timeout here is the wedge itself");
    match refused {
        Err(PaneMoveError::MoveRefused(message)) => assert_eq!(message, "permission denied"),
        Err(other) => panic!("expected the move's own refusal, got {other}"),
        Ok(_) => panic!("a denied move must not succeed"),
    }
    drop(conn);
    server.await.unwrap();
}

/// A default-layout cross-session move is a membership move plus a
/// compare-and-swap of the destination arrangement: no `MOVE_RESOURCE`,
/// no source cleanup write (the server prunes the source arrangement).
#[tokio::test]
async fn a_cross_session_move_rides_resource_move_and_the_destination_arrangement() {
    use phux_protocol::wire::frame::{
        ArrangementDoc as WireDoc, ArrangementGroup, ArrangementNode, EntryRef, EntryTarget,
        SessionLifetime,
    };
    let mut fixture = crate::testkit::OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    let play = fixture.session("play", SessionLifetime::Persistent);
    let a = fixture.shell(work);
    let b = fixture.shell(play);
    fixture.arrangement(
        play,
        WireDoc {
            groups: vec![ArrangementGroup::new(
                [3; 16],
                None,
                ArrangementNode::Leaf(EntryRef {
                    entry: [4; 16],
                    target: EntryTarget::Member(b),
                }),
            )],
            stacks: false,
        },
    );
    let (pane_a, pane_b) = (fixture.live(&a).unwrap(), fixture.live(&b).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let spec = ScriptSpec::new().organization(fixture);
    let (socket, server) = serve_one(dir.path(), spec);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let outcome = move_pane(
        &mut conn,
        pane_a.clone(),
        pane_b.clone(),
        SplitDir::Horizontal,
        0.5,
        &[],
    )
    .await
    .unwrap();
    assert!(outcome.cross_session);
    assert_eq!(outcome.destination_session_name, "play");
    assert_eq!(
        leaves(
            outcome.destination_workspace.groups[0]
                .state
                .tree
                .as_ref()
                .unwrap()
        ),
        vec![pane_b, pane_a]
    );
    drop(conn);
    let seen = server.await.unwrap();
    let count = |pick: fn(&FrameKind) -> bool| seen.iter().filter(|f| pick(f)).count();
    assert_eq!(
        count(|f| matches!(
            f,
            FrameKind::Command {
                command: Command::ResourceMove { .. },
                ..
            }
        )),
        1
    );
    assert_eq!(
        count(|f| matches!(
            f,
            FrameKind::Command {
                command: Command::ArrangementReplace { .. },
                ..
            }
        )),
        1
    );
    assert_eq!(count(|f| matches!(f, FrameKind::MoveResource { .. })), 0);
    assert_eq!(count(|f| matches!(f, FrameKind::SetMetadata { .. })), 0);
}
