use phux_core::{
    Actor, Command, ExitPolicy, InstanceId, LaunchSpec, NewResource, Op, OperationId, Registry,
    ResourceKind, SequentialKeys, SessionLifetime, TerminalLaunch,
};

use super::*;

#[test]
fn socket_names_escape_to_distinct_file_names() {
    assert_eq!(
        escape_socket(Path::new("/tmp/phux-ci-42.sock")),
        "%2Ftmp%2Fphux-ci-42%2Esock"
    );
    assert_ne!(
        escape_socket(Path::new("/tmp/a_b.sock")),
        escape_socket(Path::new("/tmp/a/b.sock"))
    );
    assert_ne!(
        escape_socket(Path::new("/tmp/a%2Fb.sock")),
        escape_socket(Path::new("/tmp/a/b.sock"))
    );
}

#[test]
fn only_the_profile_socket_uses_the_shared_store() {
    let home = phux_config::instance::runtime_dir().join("phux.sock");
    let other = Path::new("/tmp/phux-private.sock");
    let OrgStoreLocation::Path(home) = OrgStoreLocation::for_socket(&home) else {
        panic!("a socket names a file store");
    };
    let OrgStoreLocation::Path(other) = OrgStoreLocation::for_socket(other) else {
        panic!("a socket names a file store");
    };
    assert!(home.ends_with("org/v1.sqlite3"), "{}", home.display());
    assert_ne!(home, other);
    assert!(other.starts_with(home.parent().unwrap().join("sockets")));
}

fn org(keys: &mut SequentialKeys) -> Org {
    use phux_core::FreshKeys as _;
    Org::new(
        AuthorityId::from_bytes(keys.fresh()),
        SessionKey::from_bytes(keys.fresh()),
        InstanceId::from_bytes(keys.fresh()),
        Stamp(1_000),
    )
}

fn shell() -> NewResource {
    NewResource {
        kind: ResourceKind::Terminal,
        name: None,
        launch: Some(LaunchSpec::Terminal(TerminalLaunch::default())),
        on_exit: ExitPolicy::Remove,
        parent: None,
        launch_mode: phux_core::LaunchMode::Immediate,
    }
}

fn op_id(n: u8) -> OperationId {
    OperationId::from_bytes([n; 16])
}

#[test]
fn intake_defaults_and_persisted_designation_names_keep_their_identity() {
    for name in ["intake", "inbox", "my queue"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.sqlite3");
        let mut keys = SequentialKeys::new(*b"storetst");
        let fresh = org(&mut keys);
        let key = fresh.authority().intake_session();
        assert_eq!(fresh.session(&key).unwrap().name, "intake");
        let mut sessions: Vec<_> = fresh.sessions().cloned().collect();
        sessions[0].name = name.to_owned();
        let saved = Org::restore(
            *fresh.authority(),
            fresh.instance(),
            sessions,
            fresh.resources().cloned(),
            fresh.arrangements().cloned(),
            fresh.receipts().cloned(),
        )
        .unwrap();
        let mut store = SqliteOrgStore::open(&path).unwrap();
        store.import(&saved, None, Stamp(1_000)).unwrap();
        drop(store);
        let stored = SqliteOrgStore::open(&path)
            .unwrap()
            .load()
            .unwrap()
            .unwrap();
        assert_eq!(stored.authority.intake_session(), key);
        assert_eq!(stored.sessions[0].key, key);
        assert_eq!(stored.sessions[0].name, name);
        assert_eq!(
            stored.sessions[0].revision,
            saved.session(&key).unwrap().revision
        );
    }
}

#[test]
fn a_fresh_file_loads_as_nothing_and_round_trips_commits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("org").join("v1.sqlite3");
    let mut keys = SequentialKeys::new(*b"storetst");
    let mut store = SqliteOrgStore::open(&path).unwrap();
    assert!(store.load().unwrap().is_none());

    let mut registry = Registry::new(org(&mut keys));
    store
        .import(registry.org(), Some(LIVE_IMPORT_MIGRATION), Stamp(1_000))
        .unwrap();
    let cmd = Command {
        operation: op_id(1),
        op: Op::SessionCreate {
            name: "work".to_owned(),
            lifetime: SessionLifetime::Persistent,
            attrs: phux_core::Attrs::default(),
            resource: Some(shell()),
        },
    };
    let applied = phux_core::apply(
        registry.org(),
        &cmd,
        &Actor::owner(),
        Stamp(2_000),
        &mut keys,
    )
    .unwrap();
    store.commit(&applied.transition, 7, Stamp(2_000)).unwrap();
    let _ = registry.commit(&applied.transition);
    drop(store);

    let mut reopened = SqliteOrgStore::open(&path).unwrap();
    assert!(reopened.has_migration(LIVE_IMPORT_MIGRATION).unwrap());
    let stored = reopened.load().unwrap().expect("written");
    assert_eq!(stored.journal_seq, 7);
    let back = Org::restore(
        stored.authority,
        registry.org().instance(),
        stored.sessions,
        stored.resources,
        stored.arrangements,
        stored.receipts,
    )
    .unwrap();
    assert_eq!(&back, registry.org());
}

#[test]
fn a_removed_resource_leaves_a_tombstone_and_receipts_prune() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.sqlite3");
    let mut keys = SequentialKeys::new(*b"storetst");
    let mut store = SqliteOrgStore::open(&path).unwrap();
    let mut registry = Registry::new(org(&mut keys));
    store.import(registry.org(), None, Stamp(0)).unwrap();
    let create = Command {
        operation: op_id(1),
        op: Op::ResourceCreate {
            placement: None,
            session: None,
            resource: shell(),
        },
    };
    let applied = phux_core::apply(
        registry.org(),
        &create,
        &Actor::owner(),
        Stamp(0),
        &mut keys,
    )
    .unwrap();
    store.commit(&applied.transition, 1, Stamp(0)).unwrap();
    let _ = registry.commit(&applied.transition);
    let resource = registry.org().resources().next().unwrap().key;
    let end = Command {
        operation: op_id(2),
        op: Op::ActivationEnded {
            resource,
            activation: phux_core::ActivationId::FIRST,
            state: phux_core::ExecState::Completed,
            diagnostic: None,
            outcome: phux_core::Outcome::Exit(phux_core::ExitOutcome::default()),
        },
    };
    let applied =
        phux_core::apply(registry.org(), &end, &Actor::system(), Stamp(5), &mut keys).unwrap();
    store.commit(&applied.transition, 2, Stamp(5)).unwrap();
    assert_eq!(store.count("resources").unwrap(), 0);
    assert_eq!(store.count("tombstones").unwrap(), 1);
    assert_eq!(store.count("receipts").unwrap(), 1);

    // A day later the receipt is gone; a month later the tombstone too.
    let later = Stamp(RECEIPT_HORIZON_MS + 10);
    store.commit(&Transition::default(), 3, later).unwrap();
    assert_eq!(store.count("receipts").unwrap(), 0);
    assert_eq!(store.count("tombstones").unwrap(), 1);
    store
        .commit(&Transition::default(), 4, Stamp(TOMBSTONE_HORIZON_MS + 10))
        .unwrap();
    assert_eq!(store.count("tombstones").unwrap(), 0);
}

#[test]
fn the_cap_drops_the_receipts_memory_drops() {
    let dir = tempfile::tempdir().unwrap();
    let mut keys = SequentialKeys::new(*b"storetst");
    let mut registry = Registry::new(org(&mut keys));
    let create = Command {
        operation: op_id(1),
        op: Op::ResourceCreate {
            placement: None,
            session: None,
            resource: shell(),
        },
    };
    registry
        .execute(&create, &Actor::owner(), Stamp(5), &mut keys)
        .unwrap();
    let template = registry.org().receipts().next().cloned().unwrap();
    // Every receipt shares one stamp, so only the operation id orders
    // them; ids descend so insertion order is not the answer.
    let receipts = (0..RECEIPT_CAP + 2).rev().map(|n| {
        let mut bytes = [0xee; 16];
        bytes[8..].copy_from_slice(&u64::try_from(n).unwrap().to_be_bytes());
        Receipt {
            operation: OperationId::from_bytes(bytes),
            ..template.clone()
        }
    });
    let o = registry.org();
    let mut org = Org::restore(
        *o.authority(),
        o.instance(),
        o.sessions().cloned(),
        o.resources().cloned(),
        o.arrangements().cloned(),
        receipts,
    )
    .unwrap();
    let mut store = SqliteOrgStore::open(&dir.path().join("v1.sqlite3")).unwrap();
    store.import(&org, None, Stamp(5)).unwrap();
    store.commit(&Transition::default(), 1, Stamp(5)).unwrap();
    let dropped = org.prune_receipts(Stamp(0), RECEIPT_CAP);
    assert_eq!(dropped.len(), 2);
    let kept: Vec<OperationId> = store
        .load()
        .unwrap()
        .unwrap()
        .receipts
        .iter()
        .map(|r| r.operation)
        .collect();
    assert_eq!(kept.len(), RECEIPT_CAP);
    assert!(dropped.iter().all(|op| !kept.contains(op)));
    let mut mirrored: Vec<OperationId> = org.receipts().map(|r| r.operation).collect();
    let mut kept = kept;
    mirrored.sort_unstable();
    kept.sort_unstable();
    assert_eq!(kept, mirrored);
}

#[test]
fn a_newer_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.sqlite3");
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
    }
    let err = SqliteOrgStore::open(&path).unwrap_err();
    assert!(
        matches!(err, StoreError::UnsupportedVersion { .. }),
        "{err}"
    );
}

#[test]
fn the_memory_store_fails_on_request() {
    let mut store = MemOrgStore::default();
    store.commit(&Transition::default(), 0, Stamp(0)).unwrap();
    store.fail_commits().store(true, Ordering::Relaxed);
    assert!(matches!(
        store.commit(&Transition::default(), 0, Stamp(0)),
        Err(StoreError::Injected)
    ));
}
