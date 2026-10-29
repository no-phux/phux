//! Deterministic post-intent SQLite failure and activation-fenced recovery.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::org_store::SqliteOrgStore;
use crate::state::TerminalTarget;
use phux_core::{Actor, ExitPolicy, SessionLifetime, TerminalLaunch};

fn retained() -> (
    ServerState,
    ResourceId,
    tempfile::TempDir,
    rusqlite::Connection,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("org.sqlite3");
    let mut s = ServerState::new();
    s.set_org_store(Box::new(SqliteOrgStore::open(&path).unwrap()));
    s.persist_first_start().unwrap();
    let (_, id) = s
        .create_terminal(
            TerminalTarget::NewSession {
                name: "kept",
                lifetime: SessionLifetime::Persistent,
            },
            TerminalLaunch::default(),
            ExitPolicy::Retain,
            &Actor::owner(),
        )
        .unwrap();
    let db = rusqlite::Connection::open(path).unwrap();
    (s, id, dir, db)
}
fn reject(db: &rusqlite::Connection) {
    db.execute_batch("CREATE TRIGGER reject_report BEFORE UPDATE ON resources WHEN NEW.exec_state <> 'starting' BEGIN SELECT RAISE(FAIL, 'test report refusal'); END;").unwrap();
}
fn recover(db: &rusqlite::Connection) {
    db.execute_batch("DROP TRIGGER reject_report;").unwrap();
}
fn activation(s: &ServerState, id: ResourceId) -> phux_core::Activation {
    s.registry().record(id).unwrap().activation.clone().unwrap()
}

#[test]
fn refused_failed_launch_settles_same_activation_with_diagnostic_after_recovery() {
    let (mut s, id, _dir, db) = retained();
    let before = activation(&s, id);
    let key = s.registry().record(id).unwrap().key;
    reject(&db);
    let diagnostic = phux_core::LaunchDiagnostic {
        stage: phux_core::LaunchStage::Exec,
        errno: Some(libc::ENOENT),
    };
    s.activation_ended_with_diagnostic(id, ExitOutcome::UNKNOWN, true, Some(diagnostic));
    assert_eq!(activation(&s, id), before, "memory remains committed state");
    assert!(s.activation_persistence_pending(key));
    assert_eq!(s.org.pending_reports.len(), 1);
    recover(&db);
    s.retry_activation_reports();
    let after = activation(&s, id);
    assert_eq!((after.id, after.instance), (before.id, before.instance));
    assert_eq!(after.state, ExecState::Failed);
    assert_eq!(after.diagnostic, Some(diagnostic));
    assert!(!s.has_pending_activation_reports());
    let loaded = s.org.store.load().unwrap().unwrap();
    assert_eq!(
        loaded
            .resources
            .iter()
            .find(|r| r.key == key)
            .unwrap()
            .activation
            .as_ref(),
        Some(&after)
    );
    // Retrying a settled report creates neither an activation nor an effect.
    let org = s.org().clone();
    s.retry_activation_reports();
    assert_eq!(s.org(), &org);
}

#[test]
fn refused_start_stays_before_end_and_duplicate_reports_remain_bounded() {
    let (mut s, id, _dir, db) = retained();
    reject(&db);
    for _ in 0..10 {
        s.activation_started(id, None);
        s.activation_ended(id, ExitOutcome::exited(7), false);
    }
    assert_eq!(s.org.pending_reports.len(), 1);
    assert_eq!(activation(&s, id).state, ExecState::Starting);
    recover(&db);
    s.retry_activation_reports();
    assert_eq!(activation(&s, id).state, ExecState::Completed);
    assert_eq!(
        activation(&s, id).outcome,
        Some(Outcome::Exit(ExitOutcome::exited(7)))
    );
    assert!(!s.has_pending_activation_reports());
}

#[test]
fn queued_reports_cannot_touch_relaunch_or_removed_records() {
    let (mut s, id, _dir, db) = retained();
    reject(&db);
    s.activation_ended(id, ExitOutcome::UNKNOWN, true);
    let mut old = s.org.pending_reports.pop_front().unwrap();
    let key = old.resource;
    recover(&db);
    s.persist_activation_report(&mut old).unwrap();
    let expected = s.registry().record(id).unwrap().revision;
    s.commit(
        Op::ResourceRelaunch {
            resource: key,
            expected,
            launch: None,
        },
        &Actor::owner(),
    )
    .unwrap();
    // Simulate a delayed queued copy of the old report arriving after relaunch.
    s.org.pending_reports.push_back(old);
    let before = activation(&s, id);
    s.retry_activation_reports();
    assert_eq!(activation(&s, id), before);
    assert!(!s.has_pending_activation_reports());
    reject(&db);
    s.activation_ended(id, ExitOutcome::UNKNOWN, true);
    let old = s.org.pending_reports.pop_front().unwrap();
    recover(&db);
    s.activation_ended(id, ExitOutcome::UNKNOWN, true);
    s.commit(Op::ResourceRemove { resource: key }, &Actor::owner())
        .unwrap();
    s.org.pending_reports.push_back(old);
    s.retry_activation_reports();
    assert!(s.org().resource(&key).is_none());
    assert!(!s.has_pending_activation_reports());
}

#[test]
fn report_from_another_instance_is_discarded_even_with_same_activation_id() {
    let (mut s, id, _dir, db) = retained();
    reject(&db);
    s.activation_ended(id, ExitOutcome::UNKNOWN, true);
    s.org.pending_reports.front_mut().unwrap().instance = InstanceId::from_bytes([77; 16]);
    recover(&db);
    let before = activation(&s, id);
    s.retry_activation_reports();
    assert_eq!(activation(&s, id), before);
    assert!(!s.has_pending_activation_reports());
}

#[test]
fn a_report_for_a_different_activation_id_is_discarded() {
    let (mut s, id, _dir, db) = retained();
    reject(&db);
    s.activation_ended(id, ExitOutcome::UNKNOWN, true);
    s.org.pending_reports.front_mut().unwrap().id = phux_core::ActivationId::FIRST.next();
    recover(&db);
    let before = activation(&s, id);
    s.retry_activation_reports();
    assert_eq!(activation(&s, id), before);
    assert!(!s.has_pending_activation_reports());
}

#[test]
fn a_commit_prunes_a_report_whose_activation_was_replaced() {
    let (mut s, id, _dir, _db) = retained();
    s.activation_ended(id, ExitOutcome::UNKNOWN, true);
    assert_eq!(activation(&s, id).state, ExecState::Failed);
    let record = s.registry().record(id).unwrap().clone();
    let activation_id = record.activation.as_ref().unwrap().id;
    let instance = record.activation.as_ref().unwrap().instance;
    s.org.pending_reports.push_back(PendingActivation {
        resource: record.key,
        id: activation_id,
        instance,
        start: None,
        end: Some((ExitOutcome::exited(9), false, None)),
    });
    s.commit(
        Op::ResourceRename {
            resource: record.key,
            name: Some("renamed".to_owned()),
        },
        &Actor::owner(),
    )
    .unwrap();
    assert!(
        s.org.pending_reports.is_empty(),
        "the commit must drop evidence that no longer matches the activation"
    );
    assert_eq!(activation(&s, id).state, ExecState::Failed);
}

#[test]
fn a_start_report_does_not_apply_once_the_activation_is_running() {
    let (mut s, id, _dir, db) = retained();
    s.activation_started(id, None);
    assert_eq!(activation(&s, id).state, ExecState::Running);
    reject(&db);
    s.activation_started(id, None);
    recover(&db);
    s.retry_activation_reports();
    assert_eq!(activation(&s, id).state, ExecState::Running);
    assert!(!s.has_pending_activation_reports());
}

#[test]
fn normal_start_end_and_remove_policy_still_commit_in_order() {
    let (mut s, id, _dir, _db) = retained();
    s.activation_started(id, None);
    assert_eq!(activation(&s, id).state, ExecState::Running);
    s.activation_ended(id, ExitOutcome::exited(0), false);
    assert_eq!(activation(&s, id).state, ExecState::Completed);
    assert!(!s.has_pending_activation_reports());
}
