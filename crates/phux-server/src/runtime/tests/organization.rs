use super::*;

/// A store written before every session had an arrangement gains the
/// missing empty ones at startup, and an arrangement it already held
/// stays the one its session names.
#[test]
fn open_org_store_backfills_sessions_that_have_no_arrangement() {
    use crate::org_store::{LIVE_IMPORT_MIGRATION, OrgStore as _, SqliteOrgStore};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.sqlite3");
    let (org, arrangement_key) = org_with_one_arrangement();
    {
        let mut store = SqliteOrgStore::open(&path).unwrap();
        store.import(&org, None, phux_core::Stamp(1_000)).unwrap();
        assert_eq!(store.count("arrangements").unwrap(), 1);
        assert!(!store.has_migration(LIVE_IMPORT_MIGRATION).unwrap());
    }

    let state = SharedState::new();
    open_org_store(&state, &crate::OrgStoreLocation::Path(path.clone())).unwrap();
    state.with(|server| {
        assert_eq!(
            server.session_by_name("arranged").unwrap().arrangement,
            Some(arrangement_key)
        );
        assert!(
            server
                .session_by_name("bare")
                .unwrap()
                .arrangement
                .is_some()
        );
        assert!(
            server
                .session_by_name(phux_core::INTAKE_NAME)
                .unwrap()
                .arrangement
                .is_some()
        );
        assert!(server.org().check_invariants().is_ok());
    });
    drop(state);

    let mut store = SqliteOrgStore::open(&path).unwrap();
    let stored = store.load().unwrap().expect("backfilled");
    assert_eq!(stored.arrangements.len(), 3);
    let arranged = stored
        .sessions
        .iter()
        .find(|session| session.name == "arranged")
        .unwrap();
    assert_eq!(arranged.arrangement, Some(arrangement_key));
    let bare = stored
        .sessions
        .iter()
        .find(|session| session.name == "bare")
        .unwrap();
    let backfill = stored
        .arrangements
        .iter()
        .find(|arrangement| Some(arrangement.key) == bare.arrangement)
        .unwrap();
    assert!(backfill.groups.is_empty());
    assert_eq!(backfill.revision, phux_core::Revision::FIRST);
    assert!(!store.has_migration(LIVE_IMPORT_MIGRATION).unwrap());
}
