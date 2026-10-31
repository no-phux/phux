//! Layout validation and the store-versus-blob merge.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

use super::validation::*;

#[test]
fn layout_leaf_must_name_a_pane_in_the_blob() {
    let mut win = window(2, 1, vec![3]);
    win.layout = Some(LayoutBlob::Split {
        dir: SplitDirBlob::Horizontal,
        ratio: 0.5,
        left: Box::new(LayoutBlob::Leaf(3)),
        right: Box::new(LayoutBlob::Leaf(99)),
    });
    let handoff = blob(
        vec![session(1, vec![2])],
        vec![win],
        vec![no_pty_pane(3, 2)],
    );
    match validate_upgrade_blob(&handoff) {
        Err(RebuildError::DanglingRef { kind, id }) => {
            assert_eq!(kind, "pane");
            assert_eq!(id, 99);
        }
        other => panic!("expected dangling layout pane, got {other:?}"),
    }
}

/// A pane actor failure installs none of the rebuilt sessions.
#[tokio::test(flavor = "current_thread")]
async fn pane_actor_failure_does_not_install_a_partial_tree() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut state = ServerState::new();
            let _ = state.seed_empty_session("already-here");

            let mut bad = no_pty_pane(4, 2);
            // libghostty refuses a zero-sized grid; topology is otherwise
            // complete, so this is a mid-rebuild actor failure.
            bad.cols = 0;
            bad.rows = 0;
            let handoff = blob(
                vec![session(1, vec![2])],
                vec![window(2, 1, vec![3, 4])],
                vec![no_pty_pane(3, 2), bad],
            );
            validate_upgrade_blob(&handoff).expect("topology is complete");
            match state.rebuild_from_blob(&handoff, |_, _| ()) {
                Err(RebuildError::Actor(_)) => {}
                other => panic!("expected actor rebuild failure, got {other:?}"),
            }
            assert_eq!(
                session_names(&state),
                vec!["already-here".to_owned(), "intake".to_owned()],
                "a mid-rebuild actor failure must not commit the partial tree"
            );
        })
        .await;
}

/// #1207: a hot-swap upgrade used to drop every Group-scope key, so
/// each session's `phux.tui.layout/v1/<id>` vanished and attached TUIs
/// fell back to one pane. Group and Global entries must cross; Terminal
/// entries are left for their writers.
#[tokio::test(flavor = "current_thread")]
async fn upgrade_carries_group_and_global_metadata() {
    use phux_protocol::ids::GroupId;
    use phux_protocol::wire::frame::Scope;

    let local = tokio::task::LocalSet::new();
    Box::pin(local.run_until(async {
        let mut state = ServerState::new();
        let sid = state.seed_empty_session("work").expect("seeded");
        let wire = state.idspace.intern_session(sid);
        let layout = format!("phux.tui.layout/v1/{}", wire.get());
        let group = Scope::Group(GroupId::new(1));
        let _ = state.metadata.set(&group, &layout, b"tree".to_vec());
        let _ = state
            .metadata
            .set(&Scope::Global, "phux.session.project/v1", b"p".to_vec());
        let terminal = Scope::Resource(WireResourceId::local(9));
        let _ = state
            .metadata
            .set(&terminal, "phux.agent/v1", b"a".to_vec());

        let blob = state.build_upgrade_blob(7).await;
        let bytes = blob.to_bytes().expect("serialize");
        let blob = StateBlob::from_bytes(&bytes).expect("deserialize");
        let mut fresh = ServerState::new();
        fresh.rebuild_from_blob(&blob, |_, _| ()).expect("rebuild");

        assert_eq!(fresh.metadata.get(&group, &layout), Some(b"tree".to_vec()));
        assert_eq!(
            fresh
                .metadata
                .get(&Scope::Global, "phux.session.project/v1"),
            Some(b"p".to_vec())
        );
        assert_eq!(fresh.metadata.get(&terminal, "phux.agent/v1"), None);
    }))
    .await;
}

/// A pre-record blob (no carried organization) imported through a
/// [`SqliteOrgStore`](crate::org_store::SqliteOrgStore) is the file's
/// contents, and the file records the `import-live-registry/v1` receipt.
#[tokio::test(flavor = "current_thread")]
async fn a_pre_record_import_is_written_to_sqlite_with_the_migration_receipt() {
    use crate::org_store::{LIVE_IMPORT_MIGRATION, OrgStore as _, SqliteOrgStore};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.sqlite3");
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut state = ServerState::new();
            let _ = state.seed_empty_session("already-here");
            state.set_org_store(Box::new(SqliteOrgStore::open(&path).unwrap()));
            let handoff = blob(
                vec![session(1, vec![2])],
                vec![window(2, 1, vec![3])],
                vec![no_pty_pane(3, 2)],
            );
            state
                .rebuild_from_blob(&handoff, |_, _| ())
                .expect("import");
            assert_eq!(
                session_names(&state),
                vec![
                    "already-here".to_owned(),
                    phux_core::INTAKE_NAME.to_owned(),
                    "main".to_owned()
                ]
            );
            drop(state);

            let mut store = SqliteOrgStore::open(&path).unwrap();
            assert_eq!(LIVE_IMPORT_MIGRATION, "import-live-registry/v1");
            assert!(store.has_migration(LIVE_IMPORT_MIGRATION).unwrap());
            let stored = store.load().unwrap().expect("the import wrote the file");
            let mut names: Vec<_> = stored
                .sessions
                .iter()
                .map(|session| session.name.as_str())
                .collect();
            names.sort_unstable();
            assert_eq!(names, ["already-here", phux_core::INTAKE_NAME, "main"]);
            let main = stored
                .sessions
                .iter()
                .find(|session| session.name == "main")
                .expect("imported session");
            assert!(main.awaiting_organization);
            assert_eq!(main.members.len(), 1);
            assert_eq!(stored.resources.len(), 1);
            assert!(
                stored
                    .arrangements
                    .iter()
                    .any(|arrangement| Some(arrangement.key) == main.arrangement)
            );
        })
        .await;
}

/// Snapshot `source`'s records into `path`, then rename the seeded
/// session in memory and carry one extra session the file does not have.
/// Returns the hot-upgrade blob, the store's key, and the store's revision.
#[allow(
    clippy::future_not_send,
    reason = "test helper calls build_upgrade_blob on the current-thread LocalSet"
)]
async fn blob_that_disagrees_with_the_store(
    path: &std::path::Path,
) -> (StateBlob, phux_core::SessionKey, phux_core::Revision) {
    use crate::org_store::{OrgStore as _, SqliteOrgStore};

    let mut source = ServerState::new();
    source.seed_empty_session("kept").expect("seed");
    let kept = source.session_by_name("kept").expect("kept").clone();
    let mut store = SqliteOrgStore::open(path).unwrap();
    store
        .import(source.org(), None, crate::state::now_stamp())
        .unwrap();
    drop(store);
    assert_eq!(
        source.rename_session("kept", "carried"),
        crate::state::RenameOutcome::Renamed
    );
    let mut blob = source.build_upgrade_blob(7).await;
    let template = blob
        .org
        .as_ref()
        .expect("a hot upgrade carries records")
        .sessions
        .iter()
        .find(|session| session.key == kept.key)
        .expect("carried copy")
        .clone();
    assert_eq!(template.name, "carried");
    assert_ne!(template.revision, kept.revision);
    let arrived_key = phux_core::SessionKey::from_bytes([0xA1; 16]);
    assert!(
        blob.org
            .as_ref()
            .unwrap()
            .sessions
            .iter()
            .all(|session| session.key != arrived_key)
    );
    let mut arrived = template;
    arrived.key = arrived_key;
    arrived.name = "arrived".to_owned();
    arrived.members.clear();
    arrived.arrangement = None;
    arrived.recent = None;
    arrived.lifetime = phux_core::SessionLifetime::Persistent;
    blob.org.as_mut().unwrap().sessions.push(arrived);
    (blob, kept.key, kept.revision)
}

/// Install the organization at `path` as a store this process already loaded.
fn install_loaded_sqlite(state: &mut ServerState, path: &std::path::Path) {
    use crate::org_store::{OrgStore as _, SqliteOrgStore};

    let mut store = SqliteOrgStore::open(path).unwrap();
    let stored = store.load().unwrap().expect("populated");
    let org = phux_core::Org::restore(
        stored.authority,
        state.instance_id(),
        stored.sessions,
        stored.resources,
        stored.arrangements,
        stored.receipts,
    )
    .unwrap();
    state.install_org(Box::new(store), org, true);
}

/// A hot upgrade against a populated SQLite store keeps the store's
/// record where the carried copy disagrees, and still adopts a carried
/// session the store does not have.
#[tokio::test(flavor = "current_thread")]
async fn a_populated_sqlite_store_wins_a_hot_upgrade_merge() {
    use crate::org_store::{OrgStore as _, SqliteOrgStore};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.sqlite3");
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (blob, kept_key, kept_revision) = blob_that_disagrees_with_the_store(&path).await;
            let mut resumed = ServerState::new();
            install_loaded_sqlite(&mut resumed, &path);
            resumed.rebuild_from_blob(&blob, |_, _| ()).expect("merge");
            let kept = resumed.session_by_name("kept").expect("store name");
            assert_eq!(kept.key, kept_key);
            assert_eq!(kept.revision, kept_revision);
            assert!(resumed.session_by_name("carried").is_none());
            let arrived = resumed
                .session_by_name("arrived")
                .expect("a carried session the store lacks is added")
                .clone();
            let arrangement = arrived
                .arrangement
                .expect("a carried session without an arrangement gets one");
            drop(resumed);

            let mut store = SqliteOrgStore::open(&path).unwrap();
            let stored = store.load().unwrap().expect("store");
            let row = stored
                .sessions
                .iter()
                .find(|session| session.key == kept_key)
                .expect("store row");
            assert_eq!(row.name, "kept");
            assert_eq!(row.revision, kept_revision);
            assert!(
                stored
                    .sessions
                    .iter()
                    .any(|session| session.key == arrived.key
                        && session.arrangement == Some(arrangement)),
                "the store learns the added session and its arrangement"
            );
            assert!(
                stored
                    .arrangements
                    .iter()
                    .any(|stored| stored.key == arrangement)
            );
            assert!(
                stored
                    .sessions
                    .iter()
                    .all(|session| session.name != "carried")
            );
        })
        .await;
}
