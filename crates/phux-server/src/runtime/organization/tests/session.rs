use super::*;

/// `SESSION_KILL` ends a running member and removes a dormant one in
/// one step, deletes the session, counts what it ended, and replays.
#[test]
fn session_kill_ends_every_member_and_deletes_the_session() {
    use phux_protocol::wire::frame::LaunchMode;
    run(async {
        let state = crate::runtime::apps::terminal::test_support::terminal_state();
        let root = CancellationToken::new();
        let receipt = committed(handle(
            &state,
            CLIENT,
            session_create(1, "work", true),
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
        assert!(state.with(|s| s.sessions().by_name("work").is_none()));
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
        let state = crate::runtime::apps::terminal::test_support::terminal_state();
        let root = CancellationToken::new();
        let receipt = committed(handle(
            &state,
            CLIENT,
            session_create(1, "work", true),
            &root,
        ));
        let session = receipt.session.expect("created session");
        let wire = receipt.target.expect("a launched engine").resource;
        let core = state
            .with(|s| s.idspace.resource_from_wire(&wire))
            .expect("live core id");
        assert!(
            state.with(|s| s.resources().handle(core).is_some()),
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
            state.with(|s| s.resources().handle(core).is_some()),
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
            state.with(|s| s.resources().handle(core).is_none()),
            "a committed kill closes the engine"
        );
    });
}

#[test]
fn terminate_checks_identity_and_starts_only_after_commit() {
    use std::sync::atomic::Ordering;

    use crate::org_store::MemOrgStore;

    run(async {
        let state = crate::runtime::apps::terminal::test_support::terminal_state();
        let root = CancellationToken::new();
        let receipt = committed(handle(
            &state,
            CLIENT,
            session_create(1, "work", true),
            &root,
        ));
        let target = receipt.target.expect("a launched engine");
        let pane = state.with(|s| s.idspace.resource_from_wire(&target.resource).unwrap());
        let stale_request = Command::ResourceTerminate {
            operation: op(2),
            target: LiveTarget {
                activation: phux_protocol::ids::ActivationId::new(7),
                ..target.clone()
            },
            mode: TerminateMode::Graceful,
        };
        assert!(matches!(
            handle(&state, CLIENT, stale_request, &root),
            CommandResult::Refused(WireRefusal::Stale { .. })
        ));

        let terminate = Command::ResourceTerminate {
            operation: op(3),
            target,
            mode: TerminateMode::Graceful,
        };
        let store = MemOrgStore::default();
        let fail = store.fail_commits();
        state.with_mut(|s| s.set_org_store(Box::new(store)));
        fail.store(true, Ordering::Relaxed);
        let head = state.with(ServerState::journal_head);
        assert!(matches!(
            handle(&state, CLIENT, terminate.clone(), &root),
            CommandResult::Refused(WireRefusal::StoreFailure { .. })
        ));
        state.with(|s| {
            assert!(s.resources().handle(pane).is_some());
            assert!(s.pending_close_reason(pane).is_none());
            assert_eq!(s.journal_head(), head);
        });

        fail.store(false, Ordering::Relaxed);
        let accepted = CommandResult::Accepted { operation: op(3) };
        assert_eq!(handle(&state, CLIENT, terminate.clone(), &root), accepted);
        state.with(|s| {
            let record = s.registry().record(pane).unwrap();
            assert_eq!(
                record.activation.as_ref().unwrap().state,
                phux_core::ExecState::Stopping,
                "the outcome follows as ActivationChanged"
            );
            assert!(s.pending_close_reason(pane).is_some());
        });
        let head = state.with(ServerState::journal_head);
        state.with_mut(|s| {
            let launch = phux_core::LaunchSpec::Agent(phux_core::resource::AgentLaunch {
                provider: "test".to_owned(),
                native_id: None,
            });
            assert!(matches!(
                s.create_child(
                    pane,
                    phux_core::NewResource::child(launch, None),
                    &phux_core::Actor::owner(),
                ),
                Err(phux_core::Refusal::NotFound)
            ));
            assert!(s.registry().children(pane).is_empty());
        });
        assert_eq!(handle(&state, CLIENT, terminate, &root), accepted);
        assert_eq!(state.with(ServerState::journal_head), head);
    });
}
