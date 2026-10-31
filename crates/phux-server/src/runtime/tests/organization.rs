use super::*;
use crate::org_store::SqliteOrgStore;
use crate::state::TerminalTarget;
use phux_core::{Actor, ExecState, ExitOutcome, ExitPolicy, Op, SessionLifetime, TerminalLaunch};

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

const REJECT_REPORT: &str = "CREATE TRIGGER reject_report BEFORE UPDATE ON resources \
     WHEN NEW.exec_state <> 'starting' BEGIN SELECT RAISE(FAIL, 'test report refusal'); END;";

fn paused<T>(body: impl FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = T>>>) -> T {
    let runtime = Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    let local = LocalSet::new();
    runtime.block_on(local.run_until(body()))
}

#[test]
fn a_refused_end_report_backs_off_instead_of_livelocking() {
    paused(|| {
        Box::pin(async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("org.sqlite3");
            let state = SharedState::new();
            let id = state.with_mut(|server| {
                server.set_org_store(Box::new(SqliteOrgStore::open(&path).unwrap()));
                server.persist_first_start().unwrap();
                server
                    .create_terminal(
                        TerminalTarget::NewSession {
                            name: "kept",
                            lifetime: SessionLifetime::Persistent,
                        },
                        TerminalLaunch::default(),
                        ExitPolicy::Retain,
                        &Actor::owner(),
                    )
                    .unwrap()
                    .1
            });
            rusqlite::Connection::open(&path)
                .unwrap()
                .execute_batch(REJECT_REPORT)
                .unwrap();
            crate::state::ServerState::reset_executor_persist_attempts();
            state.with_mut(|server| server.activation_ended(id, ExitOutcome::UNKNOWN, true));
            let token = CancellationToken::new();
            super::super::services::spawn_activation_report_retry(&state, &token);
            tokio::task::yield_now().await;
            let mut elapsed = Duration::ZERO;
            while elapsed < Duration::from_secs(10) {
                tokio::time::advance(Duration::from_millis(20)).await;
                tokio::task::yield_now().await;
                elapsed += Duration::from_millis(20);
            }
            let attempts = crate::state::ServerState::executor_persist_attempts();
            // 20ms doubling to a 2s cap over 10s is 12 offers. No cap stops at
            // 10; a wake that resets the delay livelocks well past 13.
            token.cancel();
            tokio::task::yield_now().await;
            assert_eq!(
                attempts, 12,
                "persistence attempts {attempts} left the 20ms-to-2s backoff"
            );
        })
    });
}

#[test]
fn a_commit_wakes_a_backing_off_report_retry() {
    paused(|| {
        Box::pin(async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("org.sqlite3");
            let state = SharedState::new();
            let (session, id) = state.with_mut(|server| {
                server.set_org_store(Box::new(SqliteOrgStore::open(&path).unwrap()));
                server.persist_first_start().unwrap();
                server
                    .create_terminal(
                        TerminalTarget::NewSession {
                            name: "kept",
                            lifetime: SessionLifetime::Persistent,
                        },
                        TerminalLaunch::default(),
                        ExitPolicy::Retain,
                        &Actor::owner(),
                    )
                    .unwrap()
            });
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch(REJECT_REPORT).unwrap();
            state.with_mut(|server| server.activation_ended(id, ExitOutcome::UNKNOWN, true));
            assert_eq!(
                state.with(|server| server
                    .registry()
                    .record(id)
                    .unwrap()
                    .activation
                    .clone()
                    .unwrap()
                    .state),
                ExecState::Starting
            );
            let token = CancellationToken::new();
            super::super::services::spawn_activation_report_retry(&state, &token);
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(3)).await;
            db.execute_batch("DROP TRIGGER reject_report;").unwrap();
            state.with_mut(|server| {
                let key = server.registry().session_key(session).unwrap();
                server
                    .commit(
                        Op::SessionRename {
                            session: key,
                            name: "kept-2".to_owned(),
                        },
                        &Actor::owner(),
                    )
                    .unwrap();
            });
            tokio::task::yield_now().await;
            let state_now = state.with(|server| {
                server
                    .registry()
                    .record(id)
                    .unwrap()
                    .activation
                    .clone()
                    .unwrap()
                    .state
            });
            token.cancel();
            tokio::task::yield_now().await;
            assert_eq!(state_now, ExecState::Failed);
        })
    });
}
