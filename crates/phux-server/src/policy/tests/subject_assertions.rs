/// workload-auth §6, both tables: every reachable row admits its verbs on
/// its subject selector and refuses the near misses (another Terminal,
/// Group, or Host; a partial multi-target grant; a child-only grant); each
/// required verb is necessary; an exempt row admits any grant; a
/// default-deny row refuses even `*@global`.
#[test]
fn every_row_admits_its_verbs_on_its_subject_and_nothing_nearby() {
    let mut world = world();
    let rows = rows(&mut world);
    for Row { frame, admit, deny } in &rows {
        let rule = frame_rule(frame);
        let case = rule.case;
        for set in admit {
            assert!(admits_set(&world, set, frame), "`{case}` refused {set:?}");
            for verb in rule.verb_set().iter() {
                let fewer = without_verb(set, verb);
                assert!(
                    !admits_set(&world, &fewer, frame),
                    "`{case}` admitted {fewer:?}, which lacks {}",
                    verb.name()
                );
            }
        }
        for set in deny {
            assert!(!admits_set(&world, set, frame), "`{case}` admitted {set:?}");
        }
        match rule.requirement {
            Requirement::Exempt(Exemption::Handshake) | Requirement::Deny => {
                assert!(admit.is_empty(), "`{case}` is default-deny");
            }
            // The owner-socket predicate: no scoped grant rides that socket.
            Requirement::Verbs(_) if rule.subject.requires_owner_uds() => {
                assert!(admit.is_empty(), "`{case}` is owner-socket only");
            }
            Requirement::Exempt(_) => {}
            Requirement::Verbs(_) | Requirement::Nested => assert!(
                !admit.is_empty() && !deny.is_empty(),
                "`{case}` needs both a grant that admits and one that does not"
            ),
        }
    }
    let covered: Vec<&str> = rows.iter().map(|row| frame_rule(&row.frame).case).collect();
    for rule in FRAME_RULES.iter().chain(COMMAND_RULES.iter()) {
        assert!(
            covered.contains(&rule.case) || UNREACHABLE.contains(&rule.case),
            "no table entry for `{}`",
            rule.case
        );
    }
}

// -----------------------------------------------------------------------------
// Selectors against the live topology.
// -----------------------------------------------------------------------------

#[test]
fn terminal_scoped_grant_cannot_reach_another_terminal_group_or_satellite() {
    let world = world();
    let grant = scoped(&[&format!("*@terminal:{}", local_id(&world.alpha))]);
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
    // A Terminal grant never reaches the Group that holds it.
    assert!(!admits(
        &world,
        &grant,
        &attach(AttachTarget::ByName("alpha".to_owned()))
    ));
    assert!(!admits_command(
        &world,
        &grant,
        Command::DetachClients {
            session: Some("alpha".to_owned()),
        }
    ));
    assert!(!admits(
        &world,
        &grant,
        &spawn(None, Some(world.alpha.clone()), None)
    ));
    // Server state is admitted and filtered at the source (§6), so the
    // Terminal grant reads its own Terminal and nothing else there.
    assert!(admits_command(
        &world,
        &grant,
        Command::GetState {
            scope: StateScope::Server,
        }
    ));
    // A satellite grant reaches only its own host's Terminals.
    let satellite = scoped(&["*@host:devbox"]);
    assert!(admits_command(
        &world,
        &satellite,
        get_screen(WireResourceId::satellite("devbox", 3))
    ));
    assert!(!admits_command(
        &world,
        &satellite,
        get_screen(WireResourceId::satellite("other", 3))
    ));
    assert!(!admits_command(
        &world,
        &satellite,
        get_screen(world.alpha.clone())
    ));
}

#[test]
fn group_ceiling_stops_matching_when_the_terminal_moves_out_and_resumes_when_it_returns() {
    let mut world = world();
    let grant = scoped(&[&format!("observe,bind@group:{}", world.alpha_group)]);
    let screen = || get_screen(world.alpha.clone());
    assert!(admits_command(&world, &grant, screen()));

    world
        .state
        .move_resource_to(
            world.alpha_core,
            world.beta_session,
            &phux_core::Actor::owner(),
        )
        .unwrap();
    assert!(
        !admits_command(&world, &grant, screen()),
        "the Group clause was flattened into a Terminal grant"
    );

    world
        .state
        .move_resource_to(
            world.alpha_core,
            world.alpha_session,
            &phux_core::Actor::owner(),
        )
        .unwrap();
    assert!(admits_command(&world, &grant, screen()));
}

#[test]
fn undoing_a_move_needs_organize_on_the_session_the_resource_leaves() {
    let mut world = world();
    let key = |state: &ServerState, session| state.registry().session_key(session).unwrap();
    let (alpha, beta) = (
        key(&world.state, world.alpha_session),
        key(&world.state, world.beta_session),
    );
    let revision =
        |state: &ServerState, k: &phux_core::SessionKey| state.org().session(k).unwrap().revision;
    let moved = world
        .state
        .commit(
            phux_core::Op::ResourceMove {
                resource: world
                    .state
                    .registry()
                    .resource_key(world.alpha_core)
                    .unwrap(),
                from: alpha,
                from_expected: revision(&world.state, &alpha),
                to: beta,
                to_expected: revision(&world.state, &beta),
            },
            &phux_core::Actor::owner(),
        )
        .unwrap();
    let undo = || Command::OrganizationUndo {
        operation: phux_protocol::ids::OperationId::new([0x42; 16]).unwrap(),
        undo: crate::org_wire::operation(moved.applied.operation).unwrap(),
        expected: Vec::new(),
    };
    let beta_group = world
        .state
        .idspace
        .session_wire(world.beta_session)
        .unwrap()
        .get();
    let terminal = local_id(&world.alpha);
    let alpha_group = world.alpha_group;
    // The resource and the session it returns to are not enough: the undo
    // also takes it out of the session it sits in now.
    let partial = scoped(&[
        &format!("organize@terminal:{terminal}"),
        &format!("organize@group:{alpha_group}"),
    ]);
    assert!(!admits_command(&world, &partial, undo()));
    let full = scoped(&[
        &format!("organize@terminal:{terminal}"),
        &format!("organize@group:{alpha_group}"),
        &format!("organize@group:{beta_group}"),
    ]);
    assert!(admits_command(&world, &full, undo()));
}

#[test]
fn absent_and_unauthorized_targets_get_the_same_denial() {
    let world = world();
    let grant = scoped(&[&format!("*@group:{}", world.alpha_group)]);
    let refuse = |command: Command| {
        enforce(&world.state, CLIENT, &grant, Request::Command(&command)).unwrap_err()
    };
    assert_eq!(
        refuse(get_screen(world.beta.clone())),
        refuse(get_screen(WireResourceId::local(999)))
    );
    let refuse_spawn = |owner: WireResourceId| {
        let frame = spawn(None, Some(owner), None);
        enforce(&world.state, CLIENT, &grant, Request::Frame(&frame)).unwrap_err()
    };
    assert_eq!(
        refuse_spawn(world.beta.clone()),
        refuse_spawn(WireResourceId::local(999))
    );
    let refuse_attach = |name: &str| {
        let frame = attach(AttachTarget::ByName(name.to_owned()));
        enforce(&world.state, CLIENT, &grant, Request::Frame(&frame)).unwrap_err()
    };
    assert_eq!(refuse_attach("beta"), refuse_attach("nowhere"));
}

#[test]
fn owner_addressed_spawn_needs_create_on_the_owners_group_and_bind_on_the_owner() {
    let world = world();
    let owned = spawn(None, Some(world.alpha.clone()), None);
    let group = world.alpha_group;
    let terminal = local_id(&world.alpha);
    assert!(admits(
        &world,
        &scoped(&[&format!("create,bind@group:{group}")]),
        &owned
    ));
    assert!(admits(
        &world,
        &scoped(&[
            &format!("create@group:{group}"),
            &format!("bind@terminal:{terminal}")
        ]),
        &owned
    ));
    assert!(!admits(
        &world,
        &scoped(&[&format!("create,bind@terminal:{terminal}")]),
        &owned
    ));
    assert!(!admits(
        &world,
        &scoped(&[&format!("bind@group:{group}")]),
        &owned
    ));
}

#[test]
fn a_local_agent_session_spawn_with_a_satellite_parent_fails_closed() {
    let world = world();
    let grant = scoped(&["*@global"]);
    let frame = spawn(
        None,
        None,
        Some(SpawnResource::agent_session(
            WireResourceId::satellite("h", 7),
            "claude",
        )),
    );
    assert!(!admits(&world, &grant, &frame));
    let local = spawn(
        None,
        None,
        Some(SpawnResource::agent_session(world.alpha.clone(), "claude")),
    );
    assert!(admits(&world, &grant, &local));
}

#[test]
fn shutdown_and_open_listener_deny_remote_paired_signal_global() {
    let world = world();
    let grant = scoped(&["signal@global"]);
    assert!(!admits_command(&world, &grant, Command::Shutdown));
    assert!(!admits_command(
        &world,
        &grant,
        Command::OpenListener {
            transport: ListenerTransport::Quic,
            port_range: None,
            linger_secs: 0,
        }
    ));
    let observe_bind = scoped(&["observe@global", "bind@global"]);
    assert!(!admits_command(
        &world,
        &observe_bind,
        Command::PerfControl {
            op: PerfOp::AllocOn,
        }
    ));
    assert!(admits_command(&world, &grant, Command::Upgrade));
    assert!(admits_command(
        &world,
        &grant,
        Command::DetachClients { session: None }
    ));
}

#[test]
fn stream_bind_is_guarded_by_observe() {
    let world = world();
    let observe_one = scoped(&[&format!("observe@terminal:{}", local_id(&world.alpha))]);
    let bind = |grant: &ConnectionGrant, terminal: &WireResourceId| {
        enforce(&world.state, CLIENT, grant, Request::StreamBind(terminal)).is_ok()
    };
    assert!(bind(&observe_one, &world.alpha));
    assert!(!bind(&observe_one, &world.beta));
    assert!(!bind(&scoped(&["input,bind@global"]), &world.alpha));
}

#[test]
fn handler_bypass_canary_unclassified_frame_is_denied() {
    let world = world();
    let grant = scoped(&["*@global"]);
    for frame in [
        FrameKind::Pong { nonce: 1 },
        FrameKind::AttachReady { attach_id: 1 },
        FrameKind::Hello {
            client_name: "late".to_owned(),
            protocol_major: 0,
            protocol_minor: 9,
            protocol_patch: 0,
            client_caps: ClientCapabilities::new(),
        },
    ] {
        assert!(!admits(&world, &grant, &frame), "{frame:?}");
    }
    // A clause for every verb at Global still cannot reach a default-deny
    // row: server-owned keys and the result namespace stay closed.
    assert!(!admits(
        &world,
        &grant,
        &set(Scope::Global, WHOAMI_KEY, b"x")
    ));
}

#[test]
fn frame_ack_needs_the_connections_current_stream() {
    let world = world();
    let grant = scoped(&["observe@global"]);
    let ack = |terminal_id: WireResourceId| FrameKind::FrameAck {
        terminal_id,
        stream_id: StreamId::new(1).unwrap(),
        bootstrap_id: BootstrapId::new(1).unwrap(),
        seq: 1,
    };
    assert!(
        admits(&world, &grant, &ack(world.alpha.clone())),
        "subscribed"
    );
    assert!(
        !admits(&world, &grant, &ack(world.beta.clone())),
        "OBSERVE without a subscription is no stream to acknowledge"
    );
}

#[test]
fn metadata_scopes_resolve_to_their_subjects() {
    let world = world();
    let pane = scoped(&[&format!("bind@terminal:{}", local_id(&world.alpha))]);
    assert!(admits(
        &world,
        &pane,
        &set(Scope::Resource(world.alpha.clone()), "phux.tags/v1", b"[]")
    ));
    assert!(!admits(
        &world,
        &pane,
        &set(Scope::Resource(world.beta.clone()), "phux.tags/v1", b"[]")
    ));
    // The opaque GroupId key is host-wide, and Global is Global.
    assert!(!admits(
        &world,
        &pane,
        &set(Scope::Group(GroupId::new(1)), "phux.tui.layout/v1", b"{}")
    ));
    assert!(admits(
        &world,
        &scoped(&["bind@host"]),
        &set(Scope::Group(GroupId::new(1)), "phux.tui.layout/v1", b"{}")
    ));
    assert!(!admits(
        &world,
        &scoped(&["bind@host"]),
        &set(Scope::Global, "phux.session.name/v1", b"a\0b")
    ));
}
