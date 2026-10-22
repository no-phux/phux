use phux_protocol::caps::ServerFeatureExtSet;
use phux_protocol::wire::frame::{ExecState, LaunchMode, LaunchSpec, OrgFilter, SessionLifetime};

use super::*;
use crate::testkit::{OrgFixture, ScriptSpec, serve_one};
use phux_protocol::ResourceId;
use phux_protocol::ids::{ResourceKey, ResourceKind, SessionId, SessionKey};

fn assert_send<T: Send>(_: &T) {}

/// The MCP server runs these on a multi-threaded runtime: every
/// organization future must be `Send`. Compiled, never called.
#[allow(dead_code, reason = "a compile-time check")]
fn organization_futures_are_send(conn: &mut Connection, key: ResourceKey, to: SessionKey) {
    assert_send(&remove_resource(conn, key));
    assert_send(&move_many(conn, &[], to));
    assert_send(&restore(conn, None));
    assert_send(&crate::kill::kill_session(conn, to, "work"));
    assert_send(&crate::pane_move::move_pane(
        conn,
        ResourceId::local(1),
        ResourceId::local(2),
        crate::layout::SplitDir::Horizontal,
        0.5,
        &[],
    ));
    assert_send(&crate::session::rename_checked(
        conn,
        "a",
        "b",
        &mut Vec::new(),
    ));
    assert_send(
        &crate::layout_ops::LayoutOps::new(conn, SessionId::new(1), 1).mutate(
            crate::layout_ops::LayoutMutation::Close {
                target: ResourceId::local(1),
            },
        ),
    );
}

fn org_spec(fixture: OrgFixture) -> ScriptSpec {
    ScriptSpec::new()
        .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
        .organization(fixture)
}

async fn connect(
    spec: ScriptSpec,
) -> (
    Connection,
    tokio::task::JoinHandle<Vec<phux_protocol::wire::frame::FrameKind>>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().expect("temp dir");
    let (socket, server) = serve_one(dir.path(), spec);
    let conn = Connection::connect(&socket).await.expect("connect");
    (conn, server, dir)
}

#[tokio::test]
async fn every_helper_refuses_without_the_bit_and_sends_nothing() {
    let (mut conn, server, _dir) = connect(ScriptSpec::new()).await;
    let err = fetch(&mut conn, OrgFilter::All, 1).await.unwrap_err();
    assert!(matches!(err, OrgError::Unsupported));
    drop(conn);
    let seen = server.await.unwrap();
    assert!(
        !seen
            .iter()
            .any(|frame| matches!(frame, phux_protocol::wire::frame::FrameKind::Command { .. })),
        "{seen:?}"
    );
}

#[tokio::test]
async fn fetch_reads_every_page_and_resolves_key_prefixes() {
    let mut fixture = OrgFixture::new().page_size(1);
    let work = fixture.session("work", SessionLifetime::Persistent);
    let a = fixture.resource(work, Some(ExecState::Running));
    let _b = fixture.resource(work, Some(ExecState::Completed));
    let (mut conn, server, _dir) = connect(org_spec(fixture)).await;
    let view = fetch(&mut conn, OrgFilter::All, 1).await.unwrap();
    assert_eq!(view.sessions.len(), 1);
    assert_eq!(view.resources.len(), 2);
    assert_eq!(view.members(&work).count(), 2);
    let text = a.to_string();
    assert!(matches!(
        view.resolve_key_prefix(&format!("^{}", &text[..8])).unwrap(),
        KeyMatch::Resource(record) if record.key == a
    ));
    assert!(matches!(
        view.resolve_key_prefix("^ab"),
        Err(OrgError::NotFound(_))
    ));
    drop(conn);
    let seen = server.await.unwrap();
    let pages = seen
        .iter()
        .filter(|f| {
            matches!(
                f,
                phux_protocol::wire::frame::FrameKind::Command {
                    command: Command::GetOrganization { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(pages, 3, "one page per record: {seen:?}");
}

#[tokio::test]
async fn a_conflict_is_retried_against_fresh_revisions() {
    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    let gone = fixture.resource(work, Some(ExecState::Completed));
    let fixture = fixture.inject_conflicts(2);
    let (mut conn, server, _dir) = connect(org_spec(fixture)).await;
    relaunch(&mut conn, gone).await.unwrap();
    drop(conn);
    let seen = server.await.unwrap();
    let removes = seen
        .iter()
        .filter(|f| {
            matches!(
                f,
                phux_protocol::wire::frame::FrameKind::Command {
                    command: Command::ResourceRelaunch { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(removes, 3, "two conflicts, then the commit");
}

#[tokio::test]
async fn a_deferred_create_is_dormant_until_restore_starts_it() {
    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    let (mut conn, server, _dir) = connect(org_spec(fixture)).await;
    let committed = create_resource(
        &mut conn,
        work,
        ResourceKind::Terminal,
        Some(LaunchSpec::new(vec!["sh".to_owned()])),
        LaunchMode::Deferred,
    )
    .await
    .unwrap();
    let key = committed.resource.expect("a record was created");
    assert!(committed.target.is_none(), "nothing launched");
    let view = fetch(&mut conn, OrgFilter::All, 1).await.unwrap();
    assert!(view.resource(&key).unwrap().activation.is_none());
    let restorable: Vec<ResourceKey> = view
        .restorable(Some(&work))
        .into_iter()
        .map(|record| record.key)
        .collect();
    assert_eq!(restorable, vec![key]);
    let restored = restore(&mut conn, Some(work)).await.unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].0, key);
    assert!(restored[0].1.is_ok());
    drop(conn);
    let seen = server.await.unwrap();
    assert!(
        seen.iter().any(|f| matches!(
            f,
            phux_protocol::wire::frame::FrameKind::Command {
                command: Command::ResourceCreate {
                    launch_mode: LaunchMode::Deferred,
                    ..
                },
                ..
            }
        )),
        "{seen:?}"
    );
}

#[tokio::test]
async fn rm_relaunch_restore_move_bulk_undo_and_inbox() {
    let mut fixture = OrgFixture::new();
    let work = fixture.session("work", SessionLifetime::Persistent);
    let play = fixture.session("play", SessionLifetime::Persistent);
    let empty = fixture.session("empty", SessionLifetime::Persistent);
    let inbox = fixture.inbox_session("inbox");
    let running = fixture.resource(work, Some(ExecState::Running));
    let restarted = fixture.ended_by_restart(work);
    let done = fixture.resource(work, Some(ExecState::Completed));
    let spare = fixture.resource(inbox, None);
    let (mut conn, server, _dir) = connect(org_spec(fixture)).await;

    assert!(matches!(
        remove_resource(&mut conn, running).await,
        Err(OrgError::Refused(Refusal::InvalidArgument { .. }))
    ));
    remove_resource(&mut conn, done).await.unwrap();
    delete_session(&mut conn, empty).await.unwrap();
    assert!(matches!(
        delete_session(&mut conn, work).await,
        Err(OrgError::Refused(Refusal::NotEmpty { .. }))
    ));
    let restored = restore(&mut conn, Some(work)).await.unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].0, restarted);
    assert!(restored[0].1.is_ok());

    let moved = move_resource(&mut conn, running, play).await.unwrap();
    let back = undo(&mut conn, &moved).await.unwrap();
    assert_ne!(back.operation, moved.operation);
    let chunks = move_many(&mut conn, &[running, restarted], play)
        .await
        .unwrap();
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].result.is_ok());
    let view = fetch(&mut conn, OrgFilter::Session(play), 1).await.unwrap();
    assert_eq!(view.members(&play).count(), 2);

    inbox_resolve(&mut conn, inbox).await.unwrap();
    let _ = spare;
    drop(conn);
    server.await.unwrap();
}
