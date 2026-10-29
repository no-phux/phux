use super::*;

#[test]
fn create_rename_and_page_round_trip_with_typed_refusals() {
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
fn a_deferred_create_stays_dormant_until_relaunched_with_its_provenance() {
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

/// A `resource:` grant names the record, not an activation: it admits
/// the relaunch of a dormant record and the live handle that relaunch
/// starts, which no `terminal:` id could have named in advance.
#[test]
fn a_resource_key_grant_reaches_the_activation_a_relaunch_starts() {
    use phux_protocol::scope::TerminalScopeSet;
    use phux_protocol::wire::frame::LaunchMode;

    run(async {
        let state = SharedState::new();
        let root = CancellationToken::new();
        let (session, _) = state.with_mut(|s| s.seed_session("work"));
        let session =
            state.with(|s| ow::session_key(s.registry().session_key(session).unwrap()).unwrap());
        let create = Command::ResourceCreate {
            placement: None,
            operation: op(1),
            session,
            kind: phux_protocol::ids::ResourceKind::Terminal,
            launch: Some(Box::new(LaunchSpec::new(vec!["sh".to_owned()]))),
            name: None,
            on_exit: phux_protocol::wire::frame::ExitPolicy::Remove,
            launch_mode: LaunchMode::Deferred,
        };
        let resource = committed(handle(&state, CLIENT, create, &root))
            .resource
            .expect("a record");
        let grant = crate::policy::ConnectionGrant::scoped(
            TerminalScopeSet::parse_all(&[&format!("*@resource:{resource}")]).unwrap(),
            Some("sha256:test".to_owned()),
        )
        .unwrap();
        let allowed = |s: &crate::state::ServerState, command: &Command| {
            crate::policy::enforce(s, CLIENT, &grant, crate::policy::Request::Command(command))
                .is_ok()
        };
        let record = page(&state, OrgFilter::Session(session))
            .resources
            .into_iter()
            .find(|r| r.key == resource)
            .expect("listed");
        let relaunch = Command::ResourceRelaunch {
            operation: op(2),
            resource,
            expected: record.revision,
        };
        assert!(state.with(|s| allowed(s, &relaunch)));
        assert!(matches!(
            handle(&state, CLIENT, relaunch, &root),
            CommandResult::Accepted { .. }
        ));
        let (live, other) = state.with_mut(|s| {
            let key = phux_core::ResourceKey::from_bytes(*resource.as_bytes());
            let id = s.registry().resource_id(&key).unwrap();
            let (_, sibling) = s.seed_session("elsewhere");
            (s.intern_terminal_wire(id), s.intern_terminal_wire(sibling))
        });
        let screen = |terminal_id| Command::GetScreen {
            terminal_id,
            request_scrollback: None,
            cells: false,
            format: 0,
        };
        state.with(|s| {
            assert!(allowed(s, &screen(live)), "the new activation's handle");
            assert!(!allowed(s, &screen(other)), "another record's handle");
        });
    });
}
