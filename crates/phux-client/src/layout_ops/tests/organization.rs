use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::ids::{ResourceKey, SessionKey};
use phux_protocol::wire::frame::{
    ArrangementDoc as WireDoc, ArrangementGroup, ArrangementNode, Command, EntryRef, EntryTarget,
    SessionLifetime,
};

use super::*;
use crate::testkit::{OrgFixture, serve_one};

fn member(key: ResourceKey, n: u8) -> ArrangementNode {
    ArrangementNode::Leaf(EntryRef {
        entry: [n; 16],
        target: EntryTarget::Member(key),
    })
}

/// A session with two shells, `a` arranged alone in one group.
fn arranged() -> (OrgFixture, SessionKey, ResourceKey, ResourceKey) {
    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    let a = fixture.shell(work);
    let b = fixture.shell(work);
    fixture.arrangement(
        work,
        WireDoc {
            groups: vec![ArrangementGroup {
                id: [7; 16],
                name: Some("main".to_owned()),
                tree: member(a, 1),
            }],
        },
    );
    (fixture, work, a, b)
}

async fn connect(
    fixture: OrgFixture,
) -> (
    Connection,
    tokio::task::JoinHandle<Vec<FrameKind>>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let spec = ScriptSpec::new()
        .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
        .organization(fixture);
    let (socket, server) = serve_one(dir.path(), spec);
    (Connection::connect(&socket).await.unwrap(), server, dir)
}

fn replaces(seen: &[FrameKind]) -> Vec<WireDoc> {
    seen.iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                command: Command::ArrangementReplace { document, .. },
                ..
            } => Some(document.clone()),
            _ => None,
        })
        .collect()
}

/// The read translates keys to live handles; the edit is a
/// compare-and-swap retried after a conflict, keeping the entry id
/// and group identity it read.
#[tokio::test]
async fn mutate_is_a_compare_and_swap_retried_on_conflict() {
    let (fixture, work, a, b) = arranged();
    let session = fixture.live_session(&work).unwrap();
    let (pane_a, pane_b) = (fixture.live(&a).unwrap(), fixture.live(&b).unwrap());
    let (mut conn, server, _dir) = connect(fixture.inject_conflicts(1)).await;
    let mut ops = LayoutOps::new(&mut conn, session, 10);
    assert!(ops.uses_arrangement());
    let read = ops.read().await.unwrap();
    assert_eq!(read.groups[0].id, [7; 16]);
    assert_eq!(read.groups[0].name, "main");
    assert_eq!(
        leaves(read.groups[0].state.tree.as_ref().unwrap()),
        vec![pane_a.clone()]
    );
    let written = ops
        .mutate(LayoutMutation::Split {
            target: pane_a.clone(),
            new_pane: pane_b.clone(),
            dir: SplitDir::Horizontal,
            ratio: 0.5,
        })
        .await
        .unwrap();
    assert_eq!(
        leaves(written.groups[0].state.tree.as_ref().unwrap()),
        vec![pane_a, pane_b]
    );
    drop(ops);
    drop(conn);
    let seen = server.await.unwrap();
    let replaces = replaces(&seen);
    assert_eq!(replaces.len(), 2, "one conflict, then the commit");
    let entries = crate::layout::wire::wire_leaves(&replaces[1].groups[0].tree);
    assert_eq!(entries[0].entry, [1; 16], "the read entry id survives");
    assert_eq!(entries[1].target, EntryTarget::Member(b));
    assert!(
        !seen
            .iter()
            .any(|frame| matches!(frame, FrameKind::SetMetadata { .. })),
        "no legacy metadata write: {seen:?}"
    );
}

/// An arrangement with no groups is seeded by compare-and-swap, never
/// through the legacy key; one with groups is returned unchanged; a
/// projection key never touches the arrangement.
#[tokio::test]
async fn an_empty_arrangement_is_seeded_by_compare_and_swap() {
    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    let a = fixture.shell(work);
    fixture.arrangement(work, WireDoc { groups: Vec::new() });
    let session = fixture.live_session(&work).unwrap();
    let pane = fixture.live(&a).unwrap();
    let (mut conn, server, _dir) = connect(fixture).await;
    let seeded = LayoutOps::new(&mut conn, session, 10)
        .read_or_seed(ArrangementDoc::single(pane.clone()))
        .await
        .unwrap();
    assert_eq!(
        leaves(seeded.groups[0].state.tree.as_ref().unwrap()),
        vec![pane.clone()]
    );
    let again = LayoutOps::new(&mut conn, session, 200)
        .read_or_seed(ArrangementDoc::single(ResourceId::local(999)))
        .await
        .unwrap();
    assert_eq!(
        leaves(again.groups[0].state.tree.as_ref().unwrap()),
        vec![pane],
        "a seeded arrangement is not seeded again"
    );
    let projection = LayoutOps::with_key(
        &mut conn,
        session,
        format!("myapp.layout/v1/{}", session.get()),
        400,
    )
    .unwrap();
    assert!(!projection.uses_arrangement());
    drop(projection);
    drop(conn);
    let seen = server.await.unwrap();
    let replaces = replaces(&seen);
    assert_eq!(replaces.len(), 1, "one seed, no second write");
    assert_eq!(
        crate::layout::wire::wire_leaves(&replaces[0].groups[0].tree)[0].target,
        EntryTarget::Member(a)
    );
    assert!(
        !seen
            .iter()
            .any(|frame| matches!(frame, FrameKind::SetMetadata { .. })),
        "no legacy metadata write: {seen:?}"
    );
}

/// A session the server lists without an arrangement is refused as
/// missing rather than seeded through the legacy key.
#[tokio::test]
async fn a_missing_arrangement_is_an_error_not_a_legacy_write() {
    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    let a = fixture.shell(work);
    let session = fixture.live_session(&work).unwrap();
    let pane = fixture.live(&a).unwrap();
    let (mut conn, server, _dir) = connect(fixture).await;
    let err = LayoutOps::new(&mut conn, session, 10)
        .read_or_seed(ArrangementDoc::single(pane))
        .await
        .unwrap_err();
    assert!(matches!(err, LayoutOpsError::MissingLayout), "{err:?}");
    drop(conn);
    let seen = server.await.unwrap();
    assert!(
        !seen
            .iter()
            .any(|frame| matches!(frame, FrameKind::SetMetadata { .. })),
        "no legacy metadata write: {seen:?}"
    );
}
