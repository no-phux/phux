use super::super::*;
use crate::org_store::{OrgStore as _, SqliteOrgStore};

/// The live tree is valid but a different connection has removed its stored
/// member. The candidate's blob-only probe used to miss this fatal resume.
#[tokio::test(flavor = "current_thread")]
async fn invalid_saved_membership_refuses_upgrade_before_touching_live_resources() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.sqlite3");
    let state = SharedState::new();
    let (_, terminal) = state.with_mut(|s| s.seed_session("kept"));
    let before = state.with(|s| s.org().clone());
    {
        let mut store = SqliteOrgStore::open(&path).unwrap();
        store.import(&before, None, phux_core::Stamp(0)).unwrap();
    }
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("DELETE FROM resources", []).unwrap();
    drop(db);
    // Open after the external write: this proves invariant validation, not
    // just detection of a commit through a second connection.
    state.with_mut(|s| {
        s.set_org_store(Box::new(SqliteOrgStore::open(&path).unwrap()));
    });
    let result = prepare_upgrade_inner(&state).await;
    assert!(
        matches!(result, Err(UpgradeError::Validation(ref why)) if why.contains("does not point back"))
    );
    state.with(|s| {
        assert_eq!(s.org(), &before);
        assert!(s.registry().record(terminal).is_some());
    });
    let stored = SqliteOrgStore::open(&path)
        .unwrap()
        .load()
        .unwrap()
        .unwrap();
    assert!(
        stored.resources.is_empty(),
        "preflight must not repair or relaunch"
    );
    assert_eq!(stored.sessions.len(), before.sessions().count());
}

#[tokio::test(flavor = "current_thread")]
async fn a_second_writer_refuses_upgrade_even_when_the_blob_is_valid() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.sqlite3");
    let state = SharedState::new();
    state.with_mut(|s| {
        s.set_org_store(Box::new(SqliteOrgStore::open(&path).unwrap()));
        s.persist_first_start().unwrap();
    });
    let mut other = SqliteOrgStore::open(&path).unwrap();
    other
        .commit(&phux_core::Transition::default(), 1, phux_core::Stamp(1))
        .unwrap();
    assert!(matches!(
        prepare_upgrade_inner(&state).await,
        Err(UpgradeError::Validation(why)) if why.contains("another connection")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn an_external_commit_after_preparation_refuses_exec_and_lifts_the_cut() {
    use super::support::*;
    crate::runtime::test_support::local(async {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.sqlite3");
        let state = SharedState::new();
        let (_, _, append) = state.with_mut(old_image_with_agent_sessions);
        state.with_mut(|s| {
            let mut store = SqliteOrgStore::open(&path).unwrap();
            store.import(s.org(), None, phux_core::Stamp(0)).unwrap();
            s.set_org_store(Box::new(store));
        });
        let (_, seal) = capture_blob(&state).await;
        let (plan, listener, flags) = unexecutable_plan(seal);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute("DELETE FROM resources", []).unwrap();
        let error = plan.exec(&state).await;
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("another connection"));
        assert_eq!(fd_flags(listener.as_raw_fd()), flags);
        let accepted = append_records(&append, "{\"type\":\"stop\"}").await;
        assert_eq!(
            accepted.first_seq, 1,
            "refused exec must resume the live producer"
        );
    })
    .await;
}
