use super::*;

/// `SESSION_KILL` ends a running member and removes a dormant one in
/// one step, deletes the session, counts what it ended, and replays.
#[test]
fn session_kill_ends_every_member_and_deletes_the_session() {
    use phux_protocol::wire::frame::LaunchMode;
    run(async {
        let state = SharedState::new();
        let root = CancellationToken::new();
        let receipt = committed(handle(
            &state,
            CLIENT,
            Command::SessionCreate {
                operation: op(1),
                name: "work".to_owned(),
                lifetime: SessionLifetime::Persistent,
                attrs: Vec::new(),
                launch: Some(Box::new(LaunchSpec::new(Vec::new()))),
            },
            &root,
        ));
        let session = receipt.session.expect("created session");
        committed(handle(
            &state,
            CLIENT,
            Command::ResourceCreate {
                placement: None,
                operation: op(2),
                session,
                kind: phux_protocol::ids::ResourceKind::Terminal,
                launch: Some(Box::new(LaunchSpec::new(Vec::new()))),
                name: None,
                on_exit: phux_protocol::wire::frame::ExitPolicy::Remove,
                launch_mode: LaunchMode::Deferred,
            },
            &root,
        ));
        let kill = Command::SessionKill {
            operation: op(3),
            session,
        };
        let killed = committed(handle(&state, CLIENT, kill.clone(), &root));
        assert_eq!(killed.members, Some(2));
        assert!(
            page(&state, OrgFilter::Session(session))
                .sessions
                .is_empty()
        );
        assert!(state.with(|s| s.session_by_name("work").is_none()));
        let replayed = committed(handle(&state, CLIENT, kill, &root));
        assert_eq!(replayed.members, None, "a replay answers without the count");

        let missing = Command::SessionKill {
            operation: op(4),
            session,
        };
        assert_eq!(
            handle(&state, CLIENT, missing, &root),
            CommandResult::Refused(WireRefusal::NotFound)
        );
        let inbox = state.with(|s| s.org().authority().inbox_session);
        let inbox = Command::SessionKill {
            operation: op(5),
            session: ow::session_key(inbox).expect("non-zero key"),
        };
        assert!(matches!(
            handle(&state, CLIENT, inbox, &root),
            CommandResult::Refused(WireRefusal::InvalidArgument { .. })
        ));
    });
}

/// A store that refuses the kill must leave the live engine and the
/// records in place: `STORE_FAILURE` means nothing changed, and the same
/// operation may be retried.
#[test]
fn session_kill_store_failure_closes_nothing() {
    use std::sync::atomic::Ordering;

    use crate::org_store::MemOrgStore;

    run(async {
        let state = SharedState::new();
        let root = CancellationToken::new();
        let receipt = committed(handle(
            &state,
            CLIENT,
            Command::SessionCreate {
                operation: op(1),
                name: "work".to_owned(),
                lifetime: SessionLifetime::Persistent,
                attrs: Vec::new(),
                launch: Some(Box::new(LaunchSpec::new(Vec::new()))),
            },
            &root,
        ));
        let session = receipt.session.expect("created session");
        let wire = receipt.target.expect("a launched engine").resource;
        let core = state
            .with(|s| s.resource_from_wire(&wire))
            .expect("live core id");
        assert!(
            state.with(|s| s.resource_handle(core).is_some()),
            "the launch registered an engine"
        );
        let head = state.with(ServerState::journal_head);

        let store = MemOrgStore::default();
        let fail = store.fail_commits();
        state.with_mut(|s| s.set_org_store(Box::new(store)));
        fail.store(true, Ordering::Relaxed);

        let kill = Command::SessionKill {
            operation: op(2),
            session,
        };
        assert!(matches!(
            handle(&state, CLIENT, kill.clone(), &root),
            CommandResult::Refused(WireRefusal::StoreFailure { .. })
        ));
        assert!(
            state.with(|s| s.resource_handle(core).is_some()),
            "a refused kill does not close the engine"
        );
        assert!(state.with(|s| s.registry().record(core).is_some()));
        assert_eq!(
            page(&state, OrgFilter::Session(session)).sessions.len(),
            1,
            "the session is still listed"
        );
        assert_eq!(
            state.with(ServerState::journal_head),
            head,
            "a refused kill journals nothing"
        );

        fail.store(false, Ordering::Relaxed);
        let killed = committed(handle(&state, CLIENT, kill, &root));
        assert_eq!(killed.members, Some(1));
        assert!(
            page(&state, OrgFilter::Session(session))
                .sessions
                .is_empty()
        );
        assert!(
            state.with(|s| s.resource_handle(core).is_none()),
            "a committed kill closes the engine"
        );
    });
}

#[test]
fn terminate_is_accepted_and_a_stale_target_refused() {
    run(async {
        let state = SharedState::new();
        let root = CancellationToken::new();
        let (_, pane) = state.with_mut(|s| s.seed_session("work"));
        let target = |activation| LiveTarget {
            resource: state.with_mut(|s| s.intern_resource_wire(pane)),
            instance: state.with(|s| s.idspace.instance()),
            activation: phux_protocol::ids::ActivationId::new(activation),
        };
        let terminate = |n, activation| Command::ResourceTerminate {
            operation: op(n),
            target: target(activation),
            mode: TerminateMode::Graceful,
        };
        assert!(matches!(
            handle(&state, CLIENT, terminate(1, 7), &root),
            CommandResult::Refused(WireRefusal::Stale { .. })
        ));
        assert_eq!(
            handle(&state, CLIENT, terminate(2, 1), &root),
            CommandResult::Accepted { operation: op(2) }
        );
        let record = state.with(|s| s.registry().record(pane).cloned()).unwrap();
        assert_eq!(
            record.activation.unwrap().state,
            phux_core::ExecState::Stopping,
            "the outcome follows as ActivationChanged"
        );
    });
}

#[test]
fn organization_events_are_journaled_with_their_actor() {
    run(async {
        let state = SharedState::new();
        state.with_mut(|s| s.set_client_name(CLIENT, "agent".to_owned()));
        let before = state.with(ServerState::journal_head);
        let create = Command::SessionCreate {
            operation: op(1),
            name: "work".to_owned(),
            lifetime: SessionLifetime::Persistent,
            attrs: Vec::new(),
            launch: None,
        };
        let _ = committed(handle(&state, CLIENT, create, &CancellationToken::new()));
        let after = state.with(ServerState::journal_head);
        assert_eq!(
            after,
            before + 2,
            "SessionCreated and its ArrangementChanged"
        );
    });
}
