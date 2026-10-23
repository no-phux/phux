//! Mutations: extracted from the shared module.
use super::*;

/// `SESSION_KILL` ends a running member and removes a dormant one in
/// one step, deletes the session, counts what it ended, and replays.
#[test]
pub(super) fn session_kill_ends_every_member_and_deletes_the_session() {
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
pub(super) fn session_kill_store_failure_closes_nothing() {
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
            .with(|s| s.terminal_from_wire(&wire))
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
pub(super) fn create_rename_and_page_round_trip_with_typed_refusals() {
    run(async {
        let state = SharedState::new();
        let root = CancellationToken::new();
        let create = |n, name: &str| Command::SessionCreate {
            operation: op(n),
            name: name.to_owned(),
            lifetime: SessionLifetime::Persistent,
            attrs: vec![("project".to_owned(), "phux".to_owned())],
            launch: Some(Box::new(LaunchSpec::new(Vec::new()))),
        };
        let receipt = committed(handle(&state, CLIENT, create(1, "work"), &root));
        let session = receipt.session.expect("created session");
        let target = receipt.target.expect("a launched resource");
        assert_eq!(receipt.revision, Revision::new(1));

        // A retry with the same id and payload replays; another payload
        // under that id is a duplicate.
        assert_eq!(
            committed(handle(&state, CLIENT, create(1, "work"), &root)).session,
            Some(session)
        );
        assert_eq!(
            handle(&state, CLIENT, create(1, "other"), &root),
            CommandResult::Refused(WireRefusal::DuplicateOperation)
        );
        assert_eq!(
            handle(&state, CLIENT, create(2, "work"), &root),
            CommandResult::Refused(WireRefusal::NameTaken)
        );

        let listing = page(&state, OrgFilter::Session(session));
        assert_eq!(listing.sessions.len(), 1);
        let record = listing.sessions[0].clone();
        assert_eq!(record.name, "work");
        assert_eq!(record.members, 1);
        assert_eq!(
            record.attrs,
            vec![("project".to_owned(), "phux".to_owned())]
        );
        assert!(
            record.arrangement.is_some(),
            "every session has an arrangement"
        );
        assert_eq!(listing.resources.len(), 1);
        let activation = listing.resources[0].activation.expect("activation");
        assert_eq!(activation.state, ExecState::Running, "the launch ran");
        assert_eq!(listing.resources[0].live, Some(target.resource));
        assert!(
            listing.inbox.is_some(),
            "the owner sees the Inbox designation"
        );

        let rename = Command::SessionRename {
            operation: op(4),
            session,
            name: "renamed".to_owned(),
        };
        let renamed = committed(handle(&state, CLIENT, rename, &root));
        assert_eq!(renamed.revision.get(), record.revision.get() + 1);
        assert!(state.with(|s| s.session_by_name("renamed").is_some()));

        let link = |n: u8, project: Option<&str>| Command::SessionSetProject {
            operation: op(n),
            session,
            project: project.map(str::to_owned),
        };
        let linked = committed(handle(&state, CLIENT, link(5, Some("other")), &root));
        assert_eq!(linked.revision.get(), renamed.revision.get() + 1);
        let attrs = page(&state, OrgFilter::Session(session)).sessions[0]
            .attrs
            .clone();
        let pair = |k: &str, v: &str| (k.to_owned(), v.to_owned());
        assert_eq!(
            attrs,
            vec![pair("project.link", "other"), pair("project", "phux")],
            "the link rides beside the create's free-text label"
        );
        for (n, bad) in [(6, " "), (8, " other"), (9, "other ")] {
            assert!(matches!(
                handle(&state, CLIENT, link(n, Some(bad)), &root),
                CommandResult::Refused(WireRefusal::InvalidArgument { .. })
            ));
        }
        let cleared = committed(handle(&state, CLIENT, link(7, None), &root));
        assert_eq!(cleared.revision.get(), linked.revision.get() + 1);
        assert_eq!(
            page(&state, OrgFilter::Session(session)).sessions[0].attrs,
            vec![pair("project", "phux")],
            "unlinking leaves the label"
        );
    });
}

#[test]
pub(super) fn a_deferred_create_stays_dormant_until_relaunched_with_its_provenance() {
    use phux_protocol::wire::frame::{LaunchMode, RESOURCE_AGENT_SESSION_KEY, Scope};

    run(async {
        let state = SharedState::new();
        let root = CancellationToken::new();
        let (session, _) = state.with_mut(|s| s.seed_session("work"));
        let session =
            state.with(|s| ow::session_key(s.registry().session_key(session).unwrap()).unwrap());
        let provenance = br#"{"plugin_id":"p","integration_id":"i","native_id":"n"}"#;
        let create = Command::ResourceCreate {
            placement: None,
            operation: op(1),
            session,
            kind: phux_protocol::ids::ResourceKind::Terminal,
            launch: Some(Box::new(
                LaunchSpec::new(vec!["sh".to_owned()]).with_restore(Some(provenance.to_vec())),
            )),
            name: None,
            on_exit: phux_protocol::wire::frame::ExitPolicy::Remove,
            launch_mode: LaunchMode::Deferred,
        };
        let receipt = committed(handle(&state, CLIENT, create, &root));
        let resource = receipt.resource.expect("a record");
        assert!(receipt.target.is_none(), "nothing launched");
        let listing = page(&state, OrgFilter::Session(session));
        let record = listing
            .resources
            .iter()
            .find(|r| r.key == resource)
            .expect("listed")
            .clone();
        assert!(record.activation.is_none(), "dormant: no activation");
        assert!(record.live.is_none(), "dormant: no live handle");

        let relaunch = Command::ResourceRelaunch {
            operation: op(2),
            resource,
            expected: record.revision,
        };
        assert!(matches!(
            handle(&state, CLIENT, relaunch, &root),
            CommandResult::Accepted { .. }
        ));
        let wire = state.with_mut(|s| {
            let key = phux_core::ResourceKey::from_bytes(*resource.as_bytes());
            let id = s.registry().resource_id(&key).unwrap();
            assert!(
                s.registry().record(id).unwrap().activation.is_some(),
                "the relaunch started an activation"
            );
            s.intern_terminal_wire(id)
        });
        assert_eq!(
            state.with(|s| s
                .metadata()
                .get(&Scope::Resource(wire), RESOURCE_AGENT_SESSION_KEY)),
            Some(provenance.to_vec()),
            "the launch installs the stored agent-session provenance"
        );
    });
}
