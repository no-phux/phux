//! Retirement persistence, descendant ordering, and replacement fencing.
use super::*;

#[test]
fn a_failed_end_keeps_the_intended_retirement_after_live_teardown() {
    let (mut s, id, _dir, db) = retained();
    let key = s.registry().resource_key(id).unwrap();
    reject(&mut s, &db);
    s.reap_resource(id);
    assert!(s.resources().handle(id).is_none());
    assert_eq!(activation(&s, id).state, ExecState::Starting);
    assert!(s.activation_persistence_pending(key));
    assert!(s.org.pending_reports.front().unwrap().retire);
    recover(&mut s, &db);
    s.retry_activation_reports();
    assert!(s.org().resource(&key).is_none());
    assert!(!s.has_pending_activation_reports());
    assert!(
        s.org
            .store
            .load()
            .unwrap()
            .unwrap()
            .resources
            .iter()
            .all(|r| r.key != key)
    );
}

#[test]
fn failed_removal_retries_only_retirement_after_the_end_is_durable() {
    let (mut s, id, _dir, db) = retained();
    let key = s.registry().resource_key(id).unwrap();
    change_trigger(
        &mut s,
        &db,
        "CREATE TRIGGER reject_remove BEFORE DELETE ON resources BEGIN SELECT RAISE(FAIL, 'test removal refusal'); END;",
    );
    s.reap_resource(id);
    let ended = activation(&s, id);
    assert_eq!(ended.state, ExecState::Completed);
    assert!(s.activation_persistence_pending(key));
    let pending = s.org.pending_reports.front().unwrap();
    assert!(pending.retire && pending.start.is_none() && pending.end.is_none());
    for _ in 0..3 {
        s.retry_activation_reports();
        assert_eq!(activation(&s, id), ended);
        assert!(s.resources().handle(id).is_none());
    }
    change_trigger(&mut s, &db, "DROP TRIGGER reject_remove;");
    s.retry_activation_reports();
    assert!(s.org().resource(&key).is_none());
    assert!(!s.has_pending_activation_reports());
}

#[test]
fn a_delayed_retirement_cannot_remove_a_relaunched_record() {
    let (mut s, id, _dir, db) = retained();
    change_trigger(
        &mut s,
        &db,
        "CREATE TRIGGER reject_remove BEFORE DELETE ON resources BEGIN SELECT RAISE(FAIL, 'test removal refusal'); END;",
    );
    s.reap_resource(id);
    let record = s.registry().record(id).unwrap().clone();
    assert!(s.activation_persistence_pending(record.key));
    change_trigger(&mut s, &db, "DROP TRIGGER reject_remove;");
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
    s.retry_activation_reports();
    assert_eq!(activation(&s, id), replacement);
    assert!(!s.has_pending_activation_reports());
}

#[test]
fn retirement_honors_terminate_retention_shutdown_and_durable_kill_suppression() {
    let (mut s, id, _dir, db) = retained();
    let key = s.registry().resource_key(id).unwrap();
    s.mark_terminating(id);
    reject(&mut s, &db);
    s.reap_resource(id);
    assert!(!s.org.pending_reports.front().unwrap().retire);
    recover(&mut s, &db);
    s.retry_activation_reports();
    assert_eq!(
        s.org()
            .resource(&key)
            .unwrap()
            .activation
            .as_ref()
            .unwrap()
            .state,
        ExecState::Completed
    );
    assert!(!s.has_pending_activation_reports());

    let (mut s, id, _dir, _db) = retained();
    let key = s.registry().resource_key(id).unwrap();
    s.suppress_retirement(&[key]);
    s.reap_resource(id);
    assert!(s.org().resource(&key).is_some());
    assert!(!s.has_pending_activation_reports());
    s.clear_retirement_suppression();
    s.begin_shutdown();
    s.reap_resource(id);
    assert!(s.org().resource(&key).is_some());
    assert!(!s.has_pending_activation_reports());
}

#[test]
fn a_hard_crash_cannot_recover_an_undurable_removal_intent() {
    let (mut s, id, dir, db) = retained();
    let key = s.registry().resource_key(id).unwrap();
    change_trigger(
        &mut s,
        &db,
        "CREATE TRIGGER reject_remove BEFORE DELETE ON resources BEGIN SELECT RAISE(FAIL, 'test removal refusal'); END;",
    );
    s.reap_resource(id);
    assert!(s.activation_persistence_pending(key));
    let ended = s.org().resource(&key).unwrap().clone();
    assert_eq!(
        ended.activation.as_ref().unwrap().state,
        ExecState::Completed
    );
    drop(s); // The pending removal, unlike the observed end, was never durable.
    db.execute_batch("DROP TRIGGER reject_remove;").unwrap();
    drop(db);
    let mut store = SqliteOrgStore::open(&dir.path().join("org.sqlite3")).unwrap();
    let stored = store.load().unwrap().unwrap();
    let mut restarted = ServerState::new();
    let org = Org::restore(
        stored.authority,
        restarted.instance_id(),
        stored.sessions,
        stored.resources,
        stored.arrangements,
        stored.receipts,
    )
    .unwrap();
    restarted.install_org(Box::new(store), org, true);
    restarted.reconcile_with(None, |_| panic!("settled activation must not probe"));
    restarted.retry_activation_reports();
    assert!(!restarted.has_pending_activation_reports());
    assert_eq!(
        restarted.org().resource(&key),
        Some(&ended),
        "retain policy survives; no invented durable retirement intent"
    );
}

#[test]
fn a_child_retirement_outage_cannot_retire_its_parent_first() {
    let (mut s, parent, _dir, db) = retained();
    let child = s
        .create_child(
            parent,
            phux_core::NewResource::child(
                phux_core::LaunchSpec::Tree(phux_core::resource::TreeLaunch {
                    target: "child".into(),
                }),
                None,
            ),
            &Actor::owner(),
        )
        .unwrap();
    let parent_key = s.registry().resource_key(parent).unwrap();
    let child_key = s.registry().resource_key(child).unwrap();
    change_trigger(
        &mut s,
        &db,
        &format!(
            "CREATE TRIGGER reject_child BEFORE DELETE ON resources WHEN OLD.key = X'{}' BEGIN SELECT RAISE(FAIL, 'test child retirement'); END;",
            hex::encode(child_key.as_bytes()),
        ),
    );
    s.reap_resource(parent);
    assert!(s.activation_persistence_pending(child_key));
    assert!(s.activation_persistence_pending(parent_key));
    assert_eq!(activation(&s, parent).state, ExecState::Starting);
    assert!(s.resources().handle(parent).is_none() && s.resources().handle(child).is_none());
    change_trigger(&mut s, &db, "DROP TRIGGER reject_child;");
    s.retry_activation_reports();
    assert!(s.org().resource(&child_key).is_none());
    assert!(s.org().resource(&parent_key).is_none());
    assert!(!s.has_pending_activation_reports());
}

#[test]
fn dormant_retirement_is_retained_but_cannot_remove_a_later_activation() {
    for relaunch in [false, true] {
        let (mut s, parent, _dir, db) = retained();
        let mut spec = phux_core::NewResource::child(
            phux_core::LaunchSpec::Tree(phux_core::resource::TreeLaunch {
                target: "deferred".into(),
            }),
            None,
        );
        spec.launch_mode = phux_core::LaunchMode::Deferred;
        let child = s.create_child(parent, spec, &Actor::owner()).unwrap();
        let record = s.registry().record(child).unwrap().clone();
        assert!(record.activation.is_none());
        change_trigger(
            &mut s,
            &db,
            "CREATE TRIGGER reject_remove BEFORE DELETE ON resources BEGIN SELECT RAISE(FAIL, 'test dormant removal'); END;",
        );
        s.reap_resource(child);
        assert!(s.activation_persistence_pending(record.key));
        change_trigger(&mut s, &db, "DROP TRIGGER reject_remove;");
        if relaunch {
            s.commit(
                Op::ResourceRelaunch {
                    resource: record.key,
                    expected: record.revision,
                    launch: None,
                },
                &Actor::owner(),
            )
            .unwrap();
        }
        s.retry_activation_reports();
        if relaunch {
            assert!(s.org().resource(&record.key).unwrap().activation.is_some());
        } else {
            assert!(s.org().resource(&record.key).is_none());
        }
        assert!(!s.has_pending_activation_reports());
    }
}
