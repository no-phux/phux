use super::*;

/// Two servers opened the same organization before one killed a session.
/// A later session/layout write from the other must not resurrect its rows
/// while leaving its removed member absent.
#[test]
fn a_stale_writer_cannot_resurrect_a_killed_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.sqlite3");
    let mut keys = SequentialKeys::new(*b"storetst");
    let mut registry = Registry::new(org(&mut keys));
    let mut store = SqliteOrgStore::open(&path).unwrap();
    store.import(registry.org(), None, Stamp(0)).unwrap();
    let create = Command {
        operation: op_id(1),
        op: Op::SessionCreate {
            name: "closed".to_owned(),
            lifetime: SessionLifetime::Persistent,
            attrs: phux_core::Attrs::default(),
            resource: Some(shell()),
        },
    };
    commit(
        &mut store,
        &mut registry,
        &mut keys,
        &create,
        &Actor::owner(),
        1,
        Stamp(1),
    );
    let old = registry.org().clone();
    let session = old.session_by_name("closed").unwrap();
    let mut stale = SqliteOrgStore::open(&path).unwrap();
    stale.load().unwrap().unwrap();
    let kill = Command {
        operation: op_id(2),
        op: Op::SessionKill {
            session: session.key,
        },
    };
    commit(
        &mut store,
        &mut registry,
        &mut keys,
        &kill,
        &Actor::owner(),
        2,
        Stamp(2),
    );
    let resurrection = Transition {
        deltas: vec![
            phux_core::Delta::PutSession(session.clone()),
            phux_core::Delta::PutArrangement(
                old.arrangement(&session.arrangement.unwrap())
                    .unwrap()
                    .clone(),
            ),
        ],
        ..Transition::default()
    };
    assert!(matches!(
        stale.commit(&resurrection, 3, Stamp(3)),
        Err(StoreError::StaleWriter(_))
    ));
    assert!(matches!(
        stale.import(&old, None, Stamp(3)),
        Err(StoreError::StaleWriter(_))
    ));
    assert!(
        matches!(stale.load(), Err(StoreError::StaleWriter(_))),
        "a stale owner must reopen, not refresh itself"
    );
    let back = restore(reload(&path), old.instance());
    assert!(back.session(&session.key).is_none());
    assert_eq!(&back, registry.org());
}

#[test]
fn an_external_write_refuses_even_an_empty_commit_but_a_reopen_can_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.sqlite3");
    let mut keys = SequentialKeys::new(*b"storetst");
    let saved = org(&mut keys);
    let mut first = SqliteOrgStore::open(&path).unwrap();
    first.import(&saved, None, Stamp(0)).unwrap();
    let mut second = SqliteOrgStore::open(&path).unwrap();
    second.commit(&Transition::default(), 1, Stamp(1)).unwrap();
    assert!(matches!(
        first.commit(&Transition::default(), 2, Stamp(2)),
        Err(StoreError::StaleWriter(_))
    ));
    drop(first);
    drop(second);
    let mut reopened = SqliteOrgStore::open(&path).unwrap();
    reopened.load().unwrap().unwrap();
    reopened
        .commit(&Transition::default(), 2, Stamp(2))
        .unwrap();
    reopened
        .commit(&Transition::default(), 3, Stamp(3))
        .unwrap();
    assert_eq!(reopened.load().unwrap().unwrap().journal_seq, 3);
}

#[test]
fn a_raw_connection_fences_read_commit_and_import() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.sqlite3");
    let mut keys = SequentialKeys::new(*b"storetst");
    let saved = org(&mut keys);
    let mut store = SqliteOrgStore::open(&path).unwrap();
    store.import(&saved, None, Stamp(0)).unwrap();
    let other = Connection::open(&path).unwrap();
    other
        .execute("UPDATE sessions SET revision = revision + 1", [])
        .unwrap();
    assert!(matches!(store.load(), Err(StoreError::StaleWriter(_))));
    assert!(matches!(
        store.commit(&Transition::default(), 1, Stamp(1)),
        Err(StoreError::StaleWriter(_))
    ));
    assert!(matches!(
        store.import(&saved, None, Stamp(1)),
        Err(StoreError::StaleWriter(_))
    ));
}
