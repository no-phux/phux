//! Server-generated arrangements must satisfy the same bounds as wire writes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use phux_core::{CreatePlacement, Reply, SplitDir};

fn create(
    state: &mut ServerState,
    session: phux_core::SessionKey,
    placement: CreatePlacement,
) -> Result<OrgCommit, Refusal> {
    state.commit_system(Op::ResourceCreate {
        session: Some(session),
        placement: Some(placement),
        resource: NewResource {
            kind: ResourceKind::Terminal,
            name: None,
            launch: Some(LaunchSpec::Terminal(TerminalLaunch::default())),
            on_exit: ExitPolicy::Retain,
            parent: None,
            launch_mode: phux_core::LaunchMode::Immediate,
        },
    })
}
fn destination(state: &mut ServerState) -> phux_core::SessionKey {
    let session = state.seed_empty_session("bounds").unwrap();
    state.registry().session_key(session).unwrap()
}
#[test]
fn incremental_splits_refuse_depth_overflow_before_store_or_effects() {
    let mut state = ServerState::new();
    let session = destination(&mut state);
    let first = create(&mut state, session, CreatePlacement::NewGroup).unwrap();
    let Reply::Ok { created, .. } = first.applied.reply else {
        panic!("created")
    };
    let resource = created.resource.unwrap();
    let instance = state
        .org()
        .resource(&resource)
        .unwrap()
        .activation
        .as_ref()
        .unwrap()
        .instance;
    let beside = CreatePlacement::Beside {
        entry: created.entry.unwrap(),
        resource,
        instance,
        activation: phux_core::ActivationId::FIRST,
        dir: SplitDir::Horizontal,
        ratio: 0.5,
    };
    for _ in 0..63 {
        create(&mut state, session, beside.clone()).unwrap();
    }
    let before = state.org().clone();
    assert!(matches!(
        create(&mut state, session, beside),
        Err(Refusal::InvalidArgument {
            field: "arrangement",
            ..
        })
    ));
    assert_eq!(state.org(), &before);
}
#[test]
fn sqlite_reopens_every_accepted_near_limit_arrangement_and_receipt() {
    use crate::org_store::SqliteOrgStore;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("near-limit.sqlite3");
    let mut state = ServerState::new();
    let mut store = SqliteOrgStore::open(&path).unwrap();
    store.import(state.org(), None, Stamp(0)).unwrap();
    state.set_org_store(Box::new(store));
    let session = destination(&mut state);
    let first = create(&mut state, session, CreatePlacement::NewGroup).unwrap();
    let Reply::Ok { created, .. } = first.applied.reply else {
        panic!("create")
    };
    let resource = created.resource.unwrap();
    let instance = state
        .org()
        .resource(&resource)
        .unwrap()
        .activation
        .as_ref()
        .unwrap()
        .instance;
    let beside = CreatePlacement::Beside {
        entry: created.entry.unwrap(),
        resource,
        instance,
        activation: phux_core::ActivationId::FIRST,
        dir: SplitDir::Horizontal,
        ratio: 0.5,
    };
    let mut refused = false;
    for _ in 0..64 {
        let before = state.org().clone();
        if create(&mut state, session, beside.clone()).is_err() {
            assert_eq!(state.org(), &before);
            refused = true;
            break;
        }
    }
    assert!(
        refused,
        "persistence or wire bounds must eventually refuse deeper trees"
    );
    let arrangement_key = state.org().session(&session).unwrap().arrangement.unwrap();
    let arrangement = state.org().arrangement(&arrangement_key).unwrap().clone();
    let before = state.org().clone();
    // The same near-limit tree is nested deeper in a replace receipt. A
    // refusal must roll the whole SQLite transaction back as well.
    let replace = state.commit_system(Op::ArrangementReplace {
        owner: phux_core::ArrangementOwner::Session(session),
        expected: arrangement.revision,
        groups: arrangement.groups.clone(),
    });
    if replace.is_err() {
        assert_eq!(state.org(), &before);
    }
    let target = state.seed_empty_session("move-target").unwrap();
    let target = state.registry().session_key(target).unwrap();
    let before_move = state.org().clone();
    // A flat membership command can still carry a deeply nested inverse.
    // It must not bypass the receipt decoder check or poison the next load.
    let moved = state.commit_system(Op::ResourceMove {
        resource,
        from: session,
        from_expected: state.org().session(&session).unwrap().revision,
        to: target,
        to_expected: state.org().session(&target).unwrap().revision,
    });
    assert!(matches!(moved, Err(Refusal::StoreFailure { .. })));
    assert_eq!(state.org(), &before_move);
    let resources = state.org().resources().count();
    drop(state);
    let loaded = SqliteOrgStore::open(&path)
        .unwrap()
        .load()
        .unwrap()
        .unwrap();
    assert_eq!(loaded.resources.len(), resources);
    assert_eq!(
        loaded
            .arrangements
            .iter()
            .find(|record| record.key == arrangement_key)
            .unwrap()
            .groups,
        arrangement.groups
    );
}

#[test]
fn incremental_groups_refuse_wire_size_overflow_without_partial_creation() {
    let mut state = ServerState::new();
    let session = destination(&mut state);
    let mut refused = false;
    for _ in 0..1500 {
        let before = state.org().clone();
        if let Err(error) = create(&mut state, session, CreatePlacement::NewGroup) {
            assert!(matches!(
                error,
                Refusal::InvalidArgument {
                    field: "arrangement",
                    ..
                }
            ));
            assert_eq!(state.org(), &before);
            refused = true;
            break;
        }
    }
    assert!(
        refused,
        "64 KiB bound must eventually refuse additional groups"
    );
}
