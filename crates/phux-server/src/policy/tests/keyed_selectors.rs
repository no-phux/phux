// -----------------------------------------------------------------------------
// Keyed selectors (§5.1): record keys that outlive the server process.
// -----------------------------------------------------------------------------

#[test]
fn session_key_ceiling_follows_membership_like_a_group_ceiling() {
    let mut world = world();
    let grant = scoped(&[&format!("observe,bind@session:{}", world.org.session)]);
    let screen = |world: &World| get_screen(world.alpha.clone());
    assert!(admits_command(&world, &grant, screen(&world)));
    assert!(!admits_command(
        &world,
        &grant,
        get_screen(world.beta.clone())
    ));
    // The session's own operations, and the session-wide helper.
    assert!(admits(
        &world,
        &grant,
        &attach(AttachTarget::ByName("alpha".to_owned()))
    ));
    assert!(!admits(
        &world,
        &grant,
        &attach(AttachTarget::ByName("beta".to_owned()))
    ));
    let alpha = super::GroupPoint::of(&world.state, world.alpha_session).unwrap();
    let beta = super::GroupPoint::of(&world.state, world.beta_session).unwrap();
    assert!(grant.admits_session(Verb::Observe, alpha));
    assert!(!grant.admits_session(Verb::Observe, beta));
    assert!(!grant.admits_session(Verb::Input, alpha));

    // Membership is live: moving the Terminal out of the session ends the
    // clause's reach over it, and moving it back restores it.
    world
        .state
        .move_resource_to(
            world.alpha_core,
            world.beta_session,
            &phux_core::Actor::owner(),
        )
        .unwrap();
    assert!(!admits_command(&world, &grant, screen(&world)));
    world
        .state
        .move_resource_to(
            world.alpha_core,
            world.alpha_session,
            &phux_core::Actor::owner(),
        )
        .unwrap();
    assert!(admits_command(&world, &grant, screen(&world)));
}

#[test]
fn resource_key_ceiling_reaches_exactly_that_record() {
    let mut world = world();
    let grant = scoped(&[&format!("*@resource:{}", world.org.resource)]);
    assert!(admits_command(
        &world,
        &grant,
        get_screen(world.alpha.clone())
    ));
    assert!(!admits_command(
        &world,
        &grant,
        get_screen(world.beta.clone())
    ));
    let same_id_elsewhere = WireResourceId::satellite("devbox", local_id(&world.alpha));
    assert!(!admits_command(
        &world,
        &grant,
        get_screen(same_id_elsewhere)
    ));
    // A resource grant never reaches the session that holds it.
    assert!(!admits(
        &world,
        &grant,
        &attach(AttachTarget::ByName("alpha".to_owned()))
    ));
    // It follows the record, not its session: still matched after a move.
    world
        .state
        .move_resource_to(
            world.alpha_core,
            world.beta_session,
            &phux_core::Actor::owner(),
        )
        .unwrap();
    assert!(admits_command(
        &world,
        &grant,
        get_screen(world.alpha.clone())
    ));
    // A key no record holds names nothing.
    let stranger = phux_protocol::ids::ResourceKey::new([0xEE; 16]).unwrap();
    let nobody = scoped(&[&format!("*@resource:{stranger}")]);
    assert!(!admits_command(
        &world,
        &nobody,
        get_screen(world.alpha.clone())
    ));
}

#[test]
fn keyed_ceilings_filter_snapshots_and_events_like_their_id_spellings() {
    let mut world = world();
    let (alpha, beta) = (world.alpha.clone(), world.beta.clone());
    let g = world.alpha_group;
    let (session_key, resource_key) = (world.org.session, world.org.resource);

    let session = filtered(
        &mut world,
        &[&format!("observe@session:{session_key}")],
        Verb::Observe,
    );
    assert_eq!(resource_ids(&session), vec![alpha.clone()]);
    assert_eq!(session.sessions.len(), 1);
    assert_eq!(session.sessions[0].id.get(), g);

    let resource = filtered(
        &mut world,
        &[&format!("observe@resource:{resource_key}")],
        Verb::Observe,
    );
    assert_eq!(resource_ids(&resource), vec![alpha.clone()]);
    assert!(
        resource.sessions.is_empty(),
        "a resource grant holds no Group"
    );

    // An event is stamped with the keys when it is recorded, and judged
    // without the state lock against them.
    let observe = |scopes: &[&str], terminal: &WireResourceId| {
        let effective =
            EffectiveScopeSet::unattenuated(&TerminalScopeSet::parse_all(scopes).unwrap()).unwrap();
        let subject = super::filter::EventSubject::at(&world.state, Some(terminal), None);
        super::filter::observes_event(&effective, Some(terminal), None, &subject)
    };
    for scope in [
        format!("observe@session:{}", world.org.session),
        format!("observe@resource:{}", world.org.resource),
    ] {
        assert!(observe(&[&scope], &alpha), "{scope}");
        assert!(!observe(&[&scope], &beta), "{scope}");
    }
    assert!(!observe(
        &[&format!("observe@session:{}", world.org.other_session)],
        &alpha
    ));
}

/// A session's record key exists from creation; its wire id only once
/// something interns one. A `session:` grant must reach it either way.
#[test]
fn session_key_ceiling_reaches_a_session_no_wire_id_names_yet() {
    use phux_protocol::wire::frame::{ExitPolicy, LaunchMode};

    let mut world = world();
    let (gamma, gamma_core) = world.state.seed_session("gamma");
    assert!(
        world.state.idspace.session_wire(gamma).is_none(),
        "the case under test: no wire id names gamma yet"
    );
    let key =
        crate::org_wire::session_key(world.state.registry().session_key(gamma).unwrap()).unwrap();
    let create = |grant: &ConnectionGrant| {
        admits_command(
            &world,
            grant,
            Command::ResourceCreate {
                operation: phux_protocol::ids::OperationId::new([6; 16]).unwrap(),
                session: key,
                kind: phux_protocol::ids::ResourceKind::Terminal,
                launch: None,
                name: None,
                on_exit: ExitPolicy::Remove,
                launch_mode: LaunchMode::Immediate,
            },
        )
    };
    assert!(create(&scoped(&[&format!("create@session:{key}")])));
    assert!(!create(&scoped(&[&format!(
        "create@session:{}",
        world.org.session
    )])));
    // No wire id: a `group:` selector, whatever its id, names nothing here.
    let point = super::GroupPoint::of(&world.state, gamma).unwrap();
    assert_eq!(point.id, None);
    assert!(!point.is_in(&phux_protocol::scope::Selector::Group(0)));
    assert!(scoped(&[&format!("organize@session:{key}")]).admits_session(Verb::Organize, point));

    // A Terminal in it is reached through the key, and so are its events.
    let terminal = world.state.intern_terminal_wire(gamma_core);
    assert!(world.state.idspace.session_wire(gamma).is_none());
    let observe = scoped(&[&format!("observe@session:{key}")]);
    assert!(admits_command(
        &world,
        &observe,
        get_screen(terminal.clone())
    ));
    assert!(!admits_command(
        &world,
        &observe,
        get_screen(world.alpha.clone())
    ));
    let effective = EffectiveScopeSet::unattenuated(
        &TerminalScopeSet::parse_all(&[&format!("observe@session:{key}")]).unwrap(),
    )
    .unwrap();
    let subject = super::filter::EventSubject::at(&world.state, Some(&terminal), None);
    assert_eq!(subject.group, Some(point));
    assert!(super::filter::observes_event(
        &effective,
        Some(&terminal),
        None,
        &subject
    ));
}

/// A server rebuilt from its persisted organization store gives every
/// record the same key and fresh wire ids: a keyed grant still reaches the
/// restored session and its Terminal, and an id-spelled grant from before
/// does not.
#[test]
fn keyed_ceilings_survive_a_restart_from_the_persisted_store() {
    use crate::org_store::{OrgStore as _, SqliteOrgStore};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.sqlite3");
    let before = world();
    let (session_key, resource_key) = (before.org.session, before.org.resource);
    let old_group = before.alpha_group;
    SqliteOrgStore::open(&path)
        .unwrap()
        .import(before.state.org(), None, phux_core::Stamp(1_000))
        .unwrap();
    drop(before);

    let mut after = ServerState::new();
    let mut store = SqliteOrgStore::open(&path).unwrap();
    let stored = store.load().unwrap().expect("persisted");
    let org = phux_core::Org::restore(
        stored.authority,
        after.instance_id(),
        stored.sessions,
        stored.resources,
        stored.arrangements,
        stored.receipts,
    )
    .unwrap();
    after.install_org(Box::new(store), org, true);
    let alpha = after.find_session_by_name("alpha").unwrap();
    assert!(
        after.idspace.session_wire(alpha).is_none(),
        "a fresh start names no session on the wire yet"
    );
    let rename = Command::SessionRename {
        operation: phux_protocol::ids::OperationId::new([6; 16]).unwrap(),
        session: session_key,
        name: "renamed".to_owned(),
    };
    let core = after
        .registry()
        .resource_id(&phux_core::ResourceKey::from_bytes(
            *resource_key.as_bytes(),
        ))
        .expect("the record is restored");
    let terminal = after.intern_terminal_wire(core);
    let allowed = |scopes: &[&str], command: &Command| {
        enforce(&after, CLIENT, &scoped(scopes), Request::Command(command)).is_ok()
    };
    assert!(after.idspace.session_wire(alpha).is_none());
    assert!(allowed(
        &[&format!("organize@session:{session_key}")],
        &rename
    ));
    assert!(!allowed(&[&format!("organize@group:{old_group}")], &rename));
    for scope in [
        format!("observe@session:{session_key}"),
        format!("observe@resource:{resource_key}"),
    ] {
        assert!(allowed(&[&scope], &get_screen(terminal.clone())), "{scope}");
    }
}
