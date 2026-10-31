use std::sync::atomic::Ordering;

use super::*;

/// The arrangement `session` names.
fn arrangement_of(state: &ServerState, session: SessionId) -> phux_core::Arrangement {
    let key = state
        .registry()
        .session(session)
        .and_then(|s| s.arrangement)
        .expect("every session has an arrangement");
    state.org().arrangement(&key).cloned().expect("arrangement")
}

/// A session created with its first terminal shows it in one group from
/// the creating commit on; an empty session's arrangement has no groups.
#[test]
fn a_launched_session_is_created_with_its_terminal_arranged() {
    let mut state = ServerState::new();
    let (session, terminal) = state.seed_session("work");
    let key = state.registry().record(terminal).expect("record").key;
    let arrangement = arrangement_of(&state, session);
    assert_eq!(arrangement.revision, phux_core::Revision::FIRST);
    assert_eq!(arrangement.groups.len(), 1);
    assert_eq!(arrangement.groups[0].name, None);
    assert_eq!(arrangement.resources(), vec![key]);

    let empty = state.seed_empty_session("empty").expect("empty session");
    assert!(arrangement_of(&state, empty).groups.is_empty());
}

#[test]
fn a_failed_store_write_refuses_and_leaves_memory_unchanged() {
    let mut state = ServerState::new();
    let (session, terminal) = state.seed_session("work");
    let store = MemOrgStore::default();
    let fail = store.fail_commits();
    state.set_org_store(Box::new(store));
    let before = state.org().clone();
    let wire_before = state.idspace.session_wire(session);

    fail.store(true, Ordering::Relaxed);
    let refused = state.commit(
        Op::SessionCreate {
            name: "other".to_owned(),
            lifetime: SessionLifetime::Persistent,
            attrs: phux_core::Attrs::default(),
            resource: None,
        },
        &Actor::owner(),
    );
    assert!(
        matches!(refused, Err(Refusal::StoreFailure { .. })),
        "{refused:?}"
    );
    state.retire_record(terminal, ExitOutcome::exited(0));
    assert_eq!(state.org(), &before, "no record moved");
    assert!(state.registry().record(terminal).is_some());
    assert_eq!(state.idspace.session_wire(session), wire_before);

    fail.store(false, Ordering::Relaxed);
    state.retire_record(terminal, ExitOutcome::exited(0));
    assert!(state.registry().record(terminal).is_none());
}

#[test]
fn executor_reports_are_refused_from_a_client() {
    let mut state = ServerState::new();
    let (_, terminal) = state.seed_session("work");
    let record = state.registry().record(terminal).unwrap().clone();
    let refused = state.commit(
        Op::ActivationLost {
            resource: record.key,
            activation: phux_core::ActivationId::FIRST,
        },
        &Actor::owner(),
    );
    assert!(matches!(refused, Err(Refusal::Unauthorized { .. })));
}

#[test]
fn a_remove_policy_exit_removes_the_record_and_an_ephemeral_session() {
    let mut state = ServerState::new();
    let (session, terminal) = state
        .create_terminal(
            TerminalTarget::NewSession {
                name: "quick",
                lifetime: SessionLifetime::Ephemeral,
            },
            TerminalLaunch::default(),
            ExitPolicy::Remove,
            &Actor::owner(),
        )
        .unwrap();
    state.activation_started(terminal, None);
    assert!(state.has_live_activation());
    state.activation_ended(terminal, ExitOutcome::exited(0), false);
    assert!(state.registry().record(terminal).is_none());
    assert!(state.registry().session(session).is_none());
    assert!(!state.has_live_activation());
}

#[test]
fn a_retain_policy_exit_keeps_the_record_with_its_outcome() {
    let mut state = ServerState::new();
    let (_, terminal) = state
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
    state.activation_ended(terminal, ExitOutcome::exited(3), false);
    let record = state.registry().record(terminal).expect("retained");
    let activation = record.activation.as_ref().unwrap();
    assert_eq!(activation.state, ExecState::Completed);
    assert_eq!(
        activation.outcome,
        Some(Outcome::Exit(ExitOutcome::exited(3)))
    );
}

/// ADR-0163 §8: every journaled organization event names a principal:
/// the one the committing connection authenticated as, or the server.
#[test]
fn every_journaled_organization_event_names_its_principal() {
    use phux_protocol::wire::frame::AgentEvent;

    let mut state = ServerState::new();
    let (session, terminal) = state.seed_session("work");
    let owner = ClientId(1);
    state.set_connection_grant(owner, crate::policy::ConnectionGrant::owner());
    let workload = ClientId(2);
    let workload_id = format!("sha256:{}", "a".repeat(64));
    state.set_connection_grant(
        workload,
        crate::policy::ConnectionGrant::scoped(
            phux_protocol::scope::TerminalScopeSet::parse_all(&["*@global"]).unwrap(),
            Some(workload_id.clone()),
        )
        .unwrap(),
    );
    let first = state.journal.head() + 1;

    let key = state.registry().session_key(session).unwrap();
    let rename = Command {
        operation: state.mint_operation(),
        op: Op::SessionRename {
            session: key,
            name: "renamed".to_owned(),
        },
    };
    let as_owner = state.actor_for(owner);
    state
        .commit_command_from(&rename, &as_owner, Some(owner))
        .unwrap();
    let create = Command {
        operation: state.mint_operation(),
        op: Op::SessionCreate {
            name: "made-by-automation".to_owned(),
            lifetime: SessionLifetime::Persistent,
            attrs: phux_core::Attrs::default(),
            resource: None,
        },
    };
    let as_workload = state.actor_for(workload);
    assert_eq!(
        as_workload.principal,
        PrincipalId::Credential(workload_id.clone())
    );
    state
        .commit_command_from(&create, &as_workload, Some(workload))
        .unwrap();
    state.retire_record(terminal, ExitOutcome::exited(0));

    let principals: Vec<_> = state
        .journal
        .retained()
        .filter(|entry| entry.seq() >= first)
        .filter(|entry| !matches!(entry.event, AgentEvent::TerminalControl { .. }))
        .map(|entry| {
            entry
                .stamp
                .actor
                .as_ref()
                .and_then(phux_protocol::wire::frame::ActorRef::principal)
                .unwrap_or_else(|| panic!("no principal on {:?}", entry.event))
        })
        .collect();
    let id = |principal: PrincipalId| crate::org_wire::principal_id(&principal).unwrap();
    assert!(principals.len() >= 3, "{principals:?}");
    assert!(principals.contains(&id(PrincipalId::Owner)));
    assert!(principals.contains(&id(PrincipalId::Credential(workload_id))));
    assert!(principals.contains(&id(PrincipalId::System)));
}

#[test]
fn shutdown_leaves_records_for_the_next_start() {
    let mut state = ServerState::new();
    let (_, terminal) = state.seed_session("work");
    state.begin_shutdown();
    state.retire_record(terminal, ExitOutcome::exited(0));
    assert!(state.registry().record(terminal).is_some());
}
