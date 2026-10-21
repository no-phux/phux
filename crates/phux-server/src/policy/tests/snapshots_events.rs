// -----------------------------------------------------------------------------
// Filtered results (§6): what an admitted row returns.
// -----------------------------------------------------------------------------

/// `world`'s whole-server snapshot as `CLIENT` holding `scopes` may see it
/// with `verb`, cut with a listener report so server-global data shows.
fn filtered(world: &mut World, scopes: &[&str], verb: Verb) -> SessionSnapshot {
    world.state.set_connection_grant(CLIENT, scoped(scopes));
    let alpha = world.state.find_session_by_name("alpha").unwrap();
    let snapshot = world
        .state
        .build_session_snapshot(alpha)
        .unwrap()
        .with_listeners(phux_protocol::wire::RemoteListenersReport::new());
    super::filter::filter_snapshot(&world.state, CLIENT, verb, snapshot)
}

fn resource_ids(snapshot: &SessionSnapshot) -> Vec<WireResourceId> {
    snapshot
        .resources
        .iter()
        .map(|resource| resource.id.clone())
        .collect()
}

/// `GET_STATE { SERVER }` returns only resources the INVENTORY selectors
/// match, and server-global data (the listener report) only with Global;
/// `ATTACH` filters by OBSERVE the same way.
#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "one grant matrix walked in order; every assert! scores as a branch"
)]
fn a_snapshot_keeps_only_what_the_grant_covers() {
    let mut world = world();
    let (alpha, beta) = (world.alpha.clone(), world.beta.clone());
    let g = world.alpha_group;

    let global = filtered(&mut world, &["inventory@global"], Verb::Inventory);
    assert_eq!(resource_ids(&global), vec![alpha.clone(), beta.clone()]);
    assert!(
        global.listeners().is_some(),
        "Global reads server-global data"
    );

    let host = filtered(&mut world, &["inventory@host"], Verb::Inventory);
    assert_eq!(resource_ids(&host), vec![alpha.clone(), beta.clone()]);
    assert_eq!(host.sessions.len(), 2);
    assert!(host.listeners().is_none(), "the host is not Global");

    let group = filtered(
        &mut world,
        &[&format!("bind,observe@group:{g}")],
        Verb::Observe,
    );
    assert_eq!(resource_ids(&group), vec![alpha]);
    assert_eq!(group.sessions.len(), 1, "{:?}", group.sessions);
    assert!(
        group
            .windows
            .iter()
            .all(|window| window.session_id.get() == g)
    );
    assert_eq!(group.focused_session.get(), g, "the focus stays visible");

    let terminal = filtered(
        &mut world,
        &[&format!("inventory@terminal:{}", local_id(&beta))],
        Verb::Inventory,
    );
    assert_eq!(resource_ids(&terminal), vec![beta]);
    assert!(terminal.windows.is_empty(), "nor its window");
    assert!(
        terminal.sessions.is_empty(),
        "a Terminal grant holds no Group"
    );
    assert_eq!(
        terminal.focused_session.get(),
        0,
        "a focus the grant cannot see is not disclosed"
    );

    // The verb matters: an OBSERVE grant inventories nothing.
    let wrong_verb = filtered(&mut world, &["observe@global"], Verb::Inventory);
    assert!(resource_ids(&wrong_verb).is_empty());
    assert!(wrong_verb.listeners().is_none());

    // A hub asks only the satellites the grant may inventory.
    let mut satellite = |scopes: &[&str]| {
        world.state.set_connection_grant(CLIENT, scoped(scopes));
        super::filter::admits_satellite(&world.state, CLIENT, Verb::Inventory, "h")
    };
    assert!(!satellite(&["inventory@host"]));
    assert!(!satellite(&["inventory@host:other"]));
    assert!(satellite(&["inventory@host:h"]));
    assert!(satellite(&["inventory@global"]));

    // Other connections and an uncovered parent are not disclosed.
    let child = world
        .state
        .new_agent_session(
            world.alpha_core,
            &phux_core::resource::AgentFacet {
                provider: "claude".to_owned(),
                native_id: None,
                state: None,
            },
        )
        .unwrap();
    let child = world.state.intern_terminal_wire(child);
    let alpha_session = world.state.find_session_by_name("alpha").unwrap();
    let mut whole = world.state.build_session_snapshot(alpha_session).unwrap();
    for resource in &mut whole.resources {
        resource.viewers = vec![phux_protocol::ids::ClientId::new(99)];
        resource.input_holder = Some(phux_protocol::ids::ClientId::new(99));
        if resource.id == child {
            resource.parent = Some(world.alpha.clone());
        }
    }
    world.state.set_connection_grant(
        CLIENT,
        scoped(&[&format!("inventory@terminal:{}", local_id(&child))]),
    );
    let only_child = super::filter::filter_snapshot(&world.state, CLIENT, Verb::Inventory, whole);
    assert_eq!(resource_ids(&only_child), vec![child]);
    let seen = &only_child.resources[0];
    assert!(seen.viewers.is_empty() && seen.input_holder.is_none());
    assert_eq!(seen.parent, None, "the parent is outside the grant");

    // The owner's grant filters nothing.
    world
        .state
        .set_connection_grant(CLIENT, ConnectionGrant::owner());
    let alpha_session = world.state.find_session_by_name("alpha").unwrap();
    let whole = world.state.build_session_snapshot(alpha_session).unwrap();
    let owner =
        super::filter::filter_snapshot(&world.state, CLIENT, Verb::Inventory, whole.clone());
    assert_eq!(owner, whole);
}

/// `SESSION_STATE.recent_input` names the session's recent input target
/// (R14) and is withheld when the grant does not cover that Terminal.
#[test]
fn a_snapshot_reports_the_recent_input_target_the_grant_covers() {
    let mut world = world();
    let alpha_session = world.alpha_session;
    let whole = world.state.build_session_snapshot(alpha_session).unwrap();
    assert!(
        whole.sessions.iter().all(|s| s.recent_input.is_none()),
        "no input yet: nothing recent, no first-member fallback"
    );

    let _ = world
        .state
        .registry_mut()
        .set_recent(alpha_session, Some(world.alpha_core));
    let g = world.alpha_group;
    let group = filtered(
        &mut world,
        &[&format!("bind,observe@group:{g}")],
        Verb::Observe,
    );
    assert_eq!(group.sessions[0].recent_input, Some(world.alpha.clone()));

    // A recent target outside the grant is not disclosed.
    let mut whole = world.state.build_session_snapshot(alpha_session).unwrap();
    for session in &mut whole.sessions {
        session.recent_input = Some(world.beta.clone());
    }
    let seen = super::filter::filter_snapshot(&world.state, CLIENT, Verb::Observe, whole);
    assert_eq!(seen.sessions.len(), 1);
    assert_eq!(seen.sessions[0].recent_input, None);
}

/// `SESSION_STATE.recent_input` follows membership: once the recent
/// Terminal moves to another session (ADR-0056) neither session reports it,
/// since the destination never took input there.
#[test]
fn a_moved_recent_input_target_is_reported_by_neither_session() {
    let mut world = world();
    let (alpha_session, beta_session) = (world.alpha_session, world.beta_session);
    let _ = world
        .state
        .registry_mut()
        .set_recent(alpha_session, Some(world.alpha_core));
    let before = world.state.build_session_snapshot(alpha_session).unwrap();
    let alpha_wire = world.state.idspace.session_wire(alpha_session);
    assert_eq!(
        before
            .sessions
            .iter()
            .find(|s| Some(s.id) == alpha_wire)
            .and_then(|s| s.recent_input.clone()),
        Some(world.alpha.clone())
    );

    world
        .state
        .move_resource_to(world.alpha_core, beta_session, &phux_core::Actor::owner())
        .unwrap();
    let after = world.state.build_session_snapshot(beta_session).unwrap();
    assert!(
        after.sessions.iter().all(|s| s.recent_input.is_none()),
        "{:?}",
        after.sessions
    );
}

/// The Bell events a server-wide subscription holding `scopes` receives,
/// after one Bell each on `alpha`, `beta`, no Terminal, and a child of
/// `alpha`, in that order.
fn server_wide_bells(scopes: &[&str]) -> Vec<Option<WireResourceId>> {
    use crate::state::EventRecord;
    use phux_protocol::wire::frame::AgentEvent;

    let mut world = world();
    let child = world
        .state
        .new_agent_session(
            world.alpha_core,
            &phux_core::resource::AgentFacet {
                provider: "claude".to_owned(),
                native_id: None,
                state: None,
            },
        )
        .unwrap();
    let child = world.state.intern_terminal_wire(child);
    world.state.set_connection_grant(CLIENT, scoped(scopes));
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    world.state.subscribe_events(CLIENT, None, tx);
    for terminal in [Some(world.alpha.clone()), Some(world.beta.clone()), None] {
        world
            .state
            .record_and_fanout(EventRecord::new(terminal, AgentEvent::Bell));
    }
    world.state.record_and_fanout(
        EventRecord::new(Some(child), AgentEvent::Bell).with_parent(Some(world.alpha.clone())),
    );
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|out| match out {
            crate::mailbox::Outbound::Frame(FrameKind::Event {
                terminal,
                event: AgentEvent::Bell,
                ..
            }) => Some(terminal),
            _ => None,
        })
        .collect()
}

/// `SUBSCRIBE_EVENTS { terminal: None }` under a scoped grant delivers only
/// events on Terminals it may OBSERVE (a child through its parent), and a
/// server-global event only with Global.
#[test]
fn a_server_wide_subscription_delivers_only_observable_events() {
    let world = world();
    let (alpha, beta, g) = (world.alpha.clone(), world.beta.clone(), world.alpha_group);

    let all = server_wide_bells(&["observe@global"]);
    assert_eq!(all.len(), 4, "Global sees every event: {all:?}");

    let host = server_wide_bells(&["observe@host"]);
    assert_eq!(
        host.len(),
        3,
        "no server-global event without Global: {host:?}"
    );
    assert_eq!(&host[..2], &[Some(alpha.clone()), Some(beta.clone())]);

    let group = server_wide_bells(&[&format!("observe@group:{g}")]);
    assert_eq!(group.len(), 2, "alpha and its child: {group:?}");
    assert_eq!(group[0], Some(alpha));
    assert_ne!(group[1], Some(beta.clone()));

    let other = server_wide_bells(&[&format!("observe@terminal:{}", local_id(&beta))]);
    assert_eq!(other, vec![Some(beta)]);

    let blind = server_wide_bells(&["inventory,bind@global"]);
    assert!(blind.is_empty(), "{blind:?}");
}
