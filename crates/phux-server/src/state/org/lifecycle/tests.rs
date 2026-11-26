//! Deterministic post-intent SQLite failure and activation-fenced recovery.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::org_store::{OrgStore, SqliteOrgStore};
use crate::state::TerminalTarget;
use phux_core::{Actor, ExitPolicy, SessionLifetime, TerminalLaunch};

mod retirement;

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
fn change_trigger(s: &mut ServerState, db: &rusqlite::Connection, sql: &str) {
    use crate::org_store::OrgStore as _;
    db.execute_batch(sql).unwrap();
    // Reopening is valid only because the test changed schema, not records.
    // Prove that before replacing the owner; never refresh a stale baseline.
    let mut store = SqliteOrgStore::open(std::path::Path::new(db.path().unwrap())).unwrap();
    let stored = store.load().unwrap().unwrap();
    let org = phux_core::Org::restore(
        stored.authority,
        s.instance_id(),
        stored.sessions,
        stored.resources,
        stored.arrangements,
        stored.receipts,
    )
    .unwrap();
    assert_eq!(&org, s.org());
    s.set_org_store(Box::new(store));
}
fn reject(s: &mut ServerState, db: &rusqlite::Connection) {
    change_trigger(
        s,
        db,
        "CREATE TRIGGER reject_report BEFORE UPDATE ON resources WHEN NEW.exec_state <> 'starting' BEGIN SELECT RAISE(FAIL, 'test report refusal'); END;",
    );
}
fn recover(s: &mut ServerState, db: &rusqlite::Connection) {
    change_trigger(s, db, "DROP TRIGGER reject_report;");
}
fn activation(s: &ServerState, id: ResourceId) -> phux_core::Activation {
    s.registry().record(id).unwrap().activation.clone().unwrap()
}

#[test]
fn refused_failed_launch_settles_same_activation_with_diagnostic_after_recovery() {
    let (mut s, id, _dir, db) = retained();
    let before = activation(&s, id);
    let key = s.registry().record(id).unwrap().key;
    reject(&mut s, &db);
    let diagnostic = phux_core::LaunchDiagnostic {
        stage: phux_core::LaunchStage::Exec,
        errno: Some(libc::ENOENT),
    };
    s.activation_ended_with_diagnostic(
        s.activation_token(id).unwrap(),
        ExitOutcome::UNKNOWN,
        true,
        Some(diagnostic),
    );
    assert_eq!(activation(&s, id), before, "memory remains committed state");
    assert!(s.activation_persistence_pending(key));
    assert_eq!(s.org.pending_reports.len(), 1);
    recover(&mut s, &db);
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
    reject(&mut s, &db);
    for _ in 0..10 {
        s.activation_started(s.activation_token(id).unwrap(), None);
        s.activation_ended(
            s.activation_token(id).unwrap(),
            ExitOutcome::exited(7),
            false,
        );
    }
    assert_eq!(s.org.pending_reports.len(), 1);
    assert_eq!(activation(&s, id).state, ExecState::Starting);
    recover(&mut s, &db);
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
    reject(&mut s, &db);
    s.activation_ended(s.activation_token(id).unwrap(), ExitOutcome::UNKNOWN, true);
    let mut old = s.org.pending_reports.pop_front().unwrap();
    let key = old.resource;
    recover(&mut s, &db);
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
    reject(&mut s, &db);
    s.activation_ended(s.activation_token(id).unwrap(), ExitOutcome::UNKNOWN, true);
    let old = s.org.pending_reports.pop_front().unwrap();
    recover(&mut s, &db);
    s.activation_ended(s.activation_token(id).unwrap(), ExitOutcome::UNKNOWN, true);
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
    reject(&mut s, &db);
    s.activation_ended(s.activation_token(id).unwrap(), ExitOutcome::UNKNOWN, true);
    s.org
        .pending_reports
        .front_mut()
        .unwrap()
        .execution
        .as_mut()
        .unwrap()
        .1 = InstanceId::from_bytes([77; 16]);
    recover(&mut s, &db);
    let before = activation(&s, id);
    s.retry_activation_reports();
    assert_eq!(activation(&s, id), before);
    assert!(!s.has_pending_activation_reports());
}

#[test]
fn a_report_for_a_different_activation_id_is_discarded() {
    let (mut s, id, _dir, db) = retained();
    reject(&mut s, &db);
    s.activation_ended(s.activation_token(id).unwrap(), ExitOutcome::UNKNOWN, true);
    let key = s.registry().record(id).unwrap().key;
    assert!(s.activation_persistence_pending(key));
    s.org
        .pending_reports
        .front_mut()
        .unwrap()
        .execution
        .as_mut()
        .unwrap()
        .0 = phux_core::ActivationId::FIRST.next();
    assert!(
        !s.activation_persistence_pending(key),
        "evidence for another activation is not this execution's pending write"
    );
    recover(&mut s, &db);
    let before = activation(&s, id);
    s.retry_activation_reports();
    assert_eq!(activation(&s, id), before);
    assert!(!s.has_pending_activation_reports());
}

#[test]
fn a_commit_prunes_a_report_whose_activation_was_replaced() {
    let (mut s, id, _dir, _db) = retained();
    s.activation_ended(s.activation_token(id).unwrap(), ExitOutcome::UNKNOWN, true);
    assert_eq!(activation(&s, id).state, ExecState::Failed);
    let record = s.registry().record(id).unwrap().clone();
    let activation_id = record.activation.as_ref().unwrap().id;
    let instance = record.activation.as_ref().unwrap().instance;
    s.org.pending_reports.push_back(PendingActivation {
        resource: record.key,
        execution: Some((activation_id, instance)),
        start: None,
        end: Some((Outcome::Exit(ExitOutcome::exited(9)), false, None)),
        lost: false,
        retire: false,
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
    s.activation_started(s.activation_token(id).unwrap(), None);
    assert_eq!(activation(&s, id).state, ExecState::Running);
    reject(&mut s, &db);
    s.activation_started(s.activation_token(id).unwrap(), None);
    assert_eq!(activation(&s, id).state, ExecState::Running);
    assert!(
        !s.has_pending_activation_reports(),
        "a start for an activation that already left Starting is dropped without a store write"
    );
}

#[test]
fn a_late_producer_cannot_start_or_end_a_replacement_activation() {
    let (mut s, id, _dir, _db) = retained();
    let producer = s.activation_token(id).unwrap();
    s.activation_ended(producer, ExitOutcome::UNKNOWN, true);
    let record = s.registry().record(id).unwrap().clone();
    s.commit(
        Op::ResourceRelaunch {
            resource: record.key,
            expected: record.revision,
            launch: None,
        },
        &Actor::owner(),
    )
    .unwrap();
    let replacement = activation(&s, id);
    assert_ne!(replacement.id, producer.activation);
    s.activation_started(producer, None);
    s.activation_ended_with_diagnostic(
        producer,
        ExitOutcome::exited(9),
        true,
        Some(phux_core::LaunchDiagnostic {
            stage: phux_core::LaunchStage::Exec,
            errno: Some(libc::ENOENT),
        }),
    );
    let wrong_instance = ActivationToken {
        instance: InstanceId::from_bytes([77; 16]),
        ..s.activation_token(id).unwrap()
    };
    s.activation_started(wrong_instance, None);
    s.activation_ended(wrong_instance, ExitOutcome::UNKNOWN, true);
    assert_eq!(activation(&s, id), replacement);
    assert!(!s.has_pending_activation_reports());
}

#[test]
fn retries_are_bounded_and_fair_across_more_than_one_pass() {
    let (mut s, first, _dir, db) = retained();
    let session = s.registry().record(first).unwrap().session;
    let mut producers = vec![s.activation_token(first).unwrap()];
    for _ in 1..70 {
        let (_, id) = s
            .create_terminal(
                TerminalTarget::Session(s.registry().session_id(&session).unwrap()),
                TerminalLaunch::default(),
                ExitPolicy::Retain,
                &Actor::owner(),
            )
            .unwrap();
        producers.push(s.activation_token(id).unwrap());
    }
    reject(&mut s, &db);
    for producer in &producers {
        s.activation_ended(*producer, ExitOutcome::exited(7), false);
    }
    assert_eq!(s.org.pending_reports.len(), 70);
    recover(&mut s, &db);
    s.retry_activation_reports();
    assert_eq!(s.org.pending_reports.len(), 6);
    s.retry_activation_reports();
    assert!(!s.has_pending_activation_reports());
    for producer in producers {
        let ended = s
            .org()
            .resource(&producer.resource)
            .unwrap()
            .activation
            .as_ref()
            .unwrap();
        assert_eq!(ended.id, producer.activation);
        assert_eq!(ended.outcome, Some(Outcome::Exit(ExitOutcome::exited(7))));
    }
}

#[test]
fn an_old_exit_watcher_cannot_clear_or_attribute_the_replacement_watch() {
    let (mut s, id, _dir, _db) = retained();
    let old = s.activation_token(id).unwrap();
    s.begin_exit_watch(id, old);
    s.activation_ended(old, ExitOutcome::exited(0), false);
    let record = s.registry().record(id).unwrap().clone();
    s.commit(
        Op::ResourceRelaunch {
            resource: record.key,
            expected: record.revision,
            launch: None,
        },
        &Actor::owner(),
    )
    .unwrap();
    let replacement = s.activation_token(id).unwrap();
    s.begin_exit_watch(id, replacement);
    let wire = s.idspace.intern_resource(id);
    s.note_closed_before_exit(id, wire.clone());
    assert_eq!(s.closed_before_exit(id, old), None);
    assert_eq!(s.closed_before_exit(id, replacement), Some(wire.clone()));
    s.end_exit_watch(id, old);
    assert_eq!(s.exit_watches.len(), 1);
    assert_eq!(s.closed_before_exit(id, replacement), Some(wire));
    s.end_exit_watch(id, replacement);
    assert!(s.exit_watches.is_empty());
}

#[test]
fn refused_reconciliation_retains_unknown_and_unobserved_without_reexecution() {
    let gone = ProcIdentity {
        pid: 4242,
        pgid: 4242,
        start_raw: 1,
        boot_id: [7; 16],
    };
    for (started, proc, expected) in [
        (false, None, ExecState::Lost),
        (true, None, ExecState::Lost),
        (true, Some(gone), ExecState::Completed),
    ] {
        let (mut s, id, _dir, db) = retained();
        let producer = s.activation_token(id).unwrap();
        if started {
            s.activation_started(producer, proc);
        }
        let before = activation(&s, id);
        reject(&mut s, &db);
        s.reconcile_with(Some([7; 16]), |pid| {
            assert_eq!(pid, 4242);
            crate::reconcile::Observed {
                alive: false,
                start_raw: None,
            }
        });
        assert_eq!(activation(&s, id), before);
        assert!(s.activation_persistence_pending(producer.resource));
        recover(&mut s, &db);
        s.retry_activation_reports();
        let after = activation(&s, id);
        assert_eq!(
            (after.id, after.instance),
            (producer.activation, producer.instance)
        );
        assert_eq!(after.state, expected);
        assert_eq!(
            after.outcome,
            (expected == ExecState::Completed).then_some(Outcome::Unobserved)
        );
        assert!(!s.has_pending_activation_reports());
    }
}
