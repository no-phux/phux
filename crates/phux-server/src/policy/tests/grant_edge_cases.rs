#[test]
fn append_resource_output_is_admitted_through_the_parent_alone() {
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
    let child_wire = world.state.intern_resource_wire(child);
    let append = Command::AppendResourceOutput {
        terminal_id: child_wire.clone(),
        bytes: b"{}\n".to_vec(),
    };
    let parent = local_id(&world.alpha);
    let on_parent = scoped(&[&format!("bind,input@terminal:{parent}")]);
    let on_child = scoped(&[&format!("bind,input@terminal:{}", local_id(&child_wire))]);
    assert!(admits_command(&world, &on_parent, append.clone()));
    assert!(
        !admits_command(&world, &on_child, append),
        "a grant naming only the child does not suffice"
    );
    // Every other named-Terminal row admits a child through its parent.
    let observe_parent = scoped(&[&format!("observe@terminal:{parent}")]);
    assert!(admits_command(
        &world,
        &observe_parent,
        get_screen(child_wire)
    ));
}

#[test]
fn another_uid_on_the_owner_socket_is_not_the_owner() {
    let mut stranger = peer(TransportType::UnixSocket);
    stranger.uid = stranger.uid.wrapping_add(1);
    assert!(ScopedPolicy::local().decide(&stranger, None).is_err());
    let (_dir, path, _id) = enrolled(&["*@global"]);
    assert!(paired_policy(&path).decide(&stranger, None).is_err());
}

#[test]
fn an_expired_grant_admits_nothing() {
    let world = world();
    let mut grant = scoped(&["*@global"]);
    assert!(admits_command(
        &world,
        &grant,
        get_screen(world.alpha.clone())
    ));
    grant.expires_at = Some(Utc::now() - chrono::Duration::seconds(1));
    assert!(!admits_command(
        &world,
        &grant,
        get_screen(world.alpha.clone())
    ));
    assert!(
        !admits(&world, &grant, &FrameKind::Ping { nonce: 1 }),
        "past its expiry the grant admits nothing, not even liveness"
    );
}

#[test]
fn viewport_resize_checks_every_subscribed_session_pane_including_unnamed_ones() {
    let mut world = world();
    let resize = FrameKind::ViewportResize {
        viewport: ViewportInfo::new(80, 24),
    };
    let one_pane = scoped(&[&format!("bind@terminal:{}", local_id(&world.alpha))]);
    assert!(
        admits(&world, &one_pane, &resize),
        "the only pane is granted"
    );
    // A second pane no client has a wire id for yet.
    let pane = world
        .state
        .add_pane_to_session(world.alpha_session)
        .expect("a pane beside alpha");
    assert!(
        admits(&world, &one_pane, &resize),
        "an unsubscribed pane is not a viewport target"
    );
    world.state.subscribe_resource(CLIENT, pane, None);
    assert!(
        !admits(&world, &one_pane, &resize),
        "an unnamed pane in the session is checked, not skipped"
    );
    let group = scoped(&[&format!("bind@group:{}", world.alpha_group)]);
    assert!(admits(&world, &group, &resize), "the Group contains it");
}

#[test]
fn viewport_resize_does_not_require_scope_for_foreign_resource_subscriptions() {
    let mut world = world();
    let resize = FrameKind::ViewportResize {
        viewport: ViewportInfo::new(100, 30),
    };
    let home = scoped(&[&format!("bind@group:{}", world.alpha_group)]);
    let foreign = world.state.resource_from_wire(&world.beta).unwrap();
    world.state.subscribe_resource(CLIENT, foreign, None);
    assert!(
        admits(&world, &home, &resize),
        "a home-session vote cannot resize a foreign resource subscription"
    );
    let foreign_only = scoped(&[&format!("bind@terminal:{}", local_id(&world.beta))]);
    assert!(
        !admits(&world, &foreign_only, &resize),
        "the actual home-session targets still require authorization"
    );
}

#[test]
fn append_to_a_satellite_child_fails_closed() {
    let world = world();
    let append = Command::AppendResourceOutput {
        terminal_id: WireResourceId::satellite("h", 3),
        bytes: b"{}\n".to_vec(),
    };
    for scopes in [["*@global"], ["*@host:h"]] {
        assert!(!admits_command(&world, &scoped(&scopes), append.clone()));
    }
}

#[test]
fn a_full_64_grant_ceiling_loads_and_admits_as_one_clause_per_grant() {
    let mut scopes = vec!["*@global".to_owned()];
    scopes.extend((0..63).map(|n| format!("observe@host:h{n}")));
    assert!(crate::workload::validate_scopes(&scopes).is_ok());
    let ceiling = TerminalScopeSet::parse_all(&scopes).unwrap();
    let grant = ConnectionGrant::scoped(ceiling, None).unwrap();
    let Authority::Scoped { effective, .. } = &grant.authority else {
        panic!("a scoped grant");
    };
    assert_eq!(effective.clauses().len(), 64);
    scopes.push("observe@host:h63".to_owned());
    assert!(
        crate::workload::validate_scopes(&scopes).is_err(),
        "65 selectors exceed the set"
    );
}

#[test]
fn persisted_ceilings_refuse_ids_that_restart_with_the_server() {
    use crate::workload::{WorkloadError, validate_scopes};
    for scope in [
        "observe@group:1",
        "observe@terminal:3",
        "observe@terminal:h/3",
    ] {
        let scopes = ["inventory@global".to_owned(), scope.to_owned()];
        assert!(
            matches!(
                validate_scopes(&scopes),
                Err(WorkloadError::UnstableSelector { index: 2 })
            ),
            "{scope}"
        );
    }
    let session = phux_protocol::ids::SessionKey::new([0x11; 16]).unwrap();
    let resource = phux_protocol::ids::ResourceKey::new([0x22; 16]).unwrap();
    for scope in [
        "*@global".to_owned(),
        "observe@host".to_owned(),
        "observe@host:devbox".to_owned(),
        format!("observe,input@session:{session}"),
        format!("*@resource:{resource}"),
    ] {
        assert!(
            validate_scopes(std::slice::from_ref(&scope)).is_ok(),
            "{scope}"
        );
    }
    // A record key outlives the process, so a registry may hold one.
    let key = [8_u8; 4];
    let keyed = serde_json::json!({
        "version": 1,
        "credentials": [{
            "id": crate::workload::credential_id(&key),
            "public_key": hex::encode(key),
            "scopes": [format!("observe@session:{session}"), format!("input@resource:{resource}")],
            "expires_at": null,
            "revoked_at": null,
        }],
    });
    assert!(WorkloadRegistry::from_bytes(keyed.to_string().as_bytes()).is_ok());
    // A registry file holding one is malformed: the whole snapshot is.
    let key = [7_u8; 4];
    let file = serde_json::json!({
        "version": 1,
        "credentials": [{
            "id": crate::workload::credential_id(&key),
            "public_key": hex::encode(key),
            "scopes": ["observe@terminal:3"],
            "expires_at": null,
            "revoked_at": null,
        }],
    });
    assert!(WorkloadRegistry::from_bytes(file.to_string().as_bytes()).is_err());
}

#[test]
fn any_grant_reads_its_own_whoami() {
    let world = world();
    let whoami = FrameKind::GetMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: WHOAMI_KEY.to_owned(),
    };
    let terminal = format!("input@terminal:{}", local_id(&world.alpha));
    for grant in [scoped(&["observe@host:x"]), scoped(&[&terminal])] {
        assert!(admits(&world, &grant, &whoami));
    }
    // Any other Global key still needs OBSERVE on Global.
    let other = FrameKind::GetMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: "phux.config.reload/v1".to_owned(),
    };
    assert!(!admits(&world, &scoped(&["observe@host:x"]), &other));
    assert!(admits(&world, &scoped(&["observe@global"]), &other));
}

#[test]
fn clearing_keep_empty_is_signal_on_the_named_session() {
    let world = world();
    let clear = |name: &str| {
        set(
            Scope::Global,
            SESSION_KEEP_EMPTY_KEY,
            &encode_session_keep_empty(name, false),
        )
    };
    let on_alpha = scoped(&[&format!("signal@group:{}", world.alpha_group)]);
    assert!(admits(&world, &on_alpha, &clear("alpha")));
    assert!(
        !admits(&world, &on_alpha, &clear("beta")),
        "another session"
    );
    assert!(
        !admits(&world, &on_alpha, &clear("nowhere")),
        "an absent session is refused like a foreign one"
    );
    assert!(
        !admits(
            &world,
            &scoped(&[&format!("bind@group:{}", world.alpha_group)]),
            &clear("alpha")
        ),
        "SIGNAL, not BIND"
    );
    assert!(admits(&world, &scoped(&["signal@host"]), &clear("nowhere")));
    // Setting the mark stays CREATE and BIND on Global.
    let mark = set(
        Scope::Global,
        SESSION_KEEP_EMPTY_KEY,
        &encode_session_keep_empty("alpha", true),
    );
    assert!(!admits(&world, &on_alpha, &mark));
    assert!(admits(&world, &scoped(&["create,bind@global"]), &mark));
    // Any other value, and deleting the key, stay default-deny.
    let all = scoped(&["*@global"]);
    assert!(!admits(
        &world,
        &all,
        &set(Scope::Global, SESSION_KEEP_EMPTY_KEY, b"alpha")
    ));
    let delete = FrameKind::DeleteMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: SESSION_KEEP_EMPTY_KEY.to_owned(),
    };
    assert!(!admits(&world, &all, &delete));
}
