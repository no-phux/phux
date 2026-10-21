fn native_samples(world: &World, agent: &WireResourceId) -> [FrameKind; 5] {
    use phux_protocol::native_agent::{
        NativeCommand, NativeIntent, NativeRequest, NativeRequestPayload, SpawnNativeAgent,
    };
    [
        command(Command::NativeAgent {
            request: NativeRequestPayload::new(NativeRequest::Spawn {
                spec: SpawnNativeAgent {
                    session: "alpha".into(),
                    backend: "acp".into(),
                    provider: "omp".into(),
                    command: vec!["omp".into(), "--mode".into(), "rpc-ui".into()],
                    cwd: None,
                    env: std::collections::BTreeMap::default(),
                    native_id: None,
                    guardian: None,
                    guardian_key: None,
                    guardian_expected: None,
                },
            })
            .unwrap(),
        }),
        command(Command::NativeAgent {
            request: NativeRequestPayload::new(NativeRequest::Command {
                agent_session_id: agent.clone(),
                command: NativeCommand {
                    operation_id: 1,
                    activation: "activation".into(),
                    intent: NativeIntent::Prompt {
                        text: "human prompt".into(),
                    },
                },
            })
            .unwrap(),
        }),
        command(Command::NativeAgent {
            request: NativeRequestPayload::new(NativeRequest::Register {
                agent_session_id: agent.clone(),
                activation: "activation".into(),
                binding_proof: "proof".into(),
            })
            .unwrap(),
        }),
        command(Command::NativeAgent {
            request: NativeRequestPayload::new(NativeRequest::ListIntegrations {}).unwrap(),
        }),
        command(Command::NativeAgent {
            request: NativeRequestPayload::new(NativeRequest::LaunchIntegration {
                integration: "opencode".into(),
                session: world.org.session,
                cwd: "/workspace".into(),
            })
            .unwrap(),
        }),
    ]
}

#[test]
fn native_launch_requires_all_three_verbs_on_the_exact_keyed_session() {
    use phux_protocol::native_agent::{NativeRequest, NativeRequestPayload};
    let world = world();
    let launch = |session| Command::NativeAgent {
        request: NativeRequestPayload::new(NativeRequest::LaunchIntegration {
            integration: "opencode".into(),
            session,
            cwd: "/workspace".into(),
        })
        .unwrap(),
    };
    let group = world.alpha_group;
    for verbs in [
        "create",
        "bind",
        "input",
        "create,bind",
        "create,input",
        "bind,input",
    ] {
        assert!(!admits_command(
            &world,
            &scoped(&[&format!("{verbs}@group:{group}")]),
            launch(world.org.session)
        ));
    }
    let grant = scoped(&[&format!("create,bind,input@group:{group}")]);
    assert!(admits_command(&world, &grant, launch(world.org.session)));
    assert!(!admits_command(
        &world,
        &grant,
        launch(world.org.other_session)
    ));
    assert!(!admits_command(
        &world,
        &grant,
        launch(phux_protocol::ids::SessionKey::new([0xab; 16]).unwrap())
    ));
}

#[test]
fn native_launch_viewer_is_denied_even_with_owner_authority() {
    use phux_protocol::native_agent::{NativeRequest, NativeRequestPayload};
    let mut world = world();
    world
        .state
        .set_connection_grant(CLIENT, ConnectionGrant::owner());
    world.state.set_attached_viewer(CLIENT, true);
    let launch = Command::NativeAgent {
        request: NativeRequestPayload::new(NativeRequest::LaunchIntegration {
            integration: "opencode".into(),
            session: world.org.session,
            cwd: "/workspace".into(),
        })
        .unwrap(),
    };
    assert!(super::authorize_command(&world.state, CLIENT, &launch).is_err());
}

/// `DELEGATE_AGENT` on `alpha`, then `REVOKE_DELEGATION` (ADR-0163).
fn delegation_samples(world: &World) -> Vec<FrameKind> {
    let op = phux_protocol::ids::OperationId::new([6; 16]).unwrap();
    vec![
        command(Command::DelegateAgent {
            operation: op,
            session: world.org.session,
            ttl_secs: 0,
        }),
        command(Command::RevokeDelegation {
            operation: op,
            credential_id: "delegated:00".to_owned(),
        }),
    ]
}

/// One sample per organization command, naming `alpha`'s records.
#[allow(
    clippy::too_many_lines,
    reason = "one literal per command, kept as one flat table"
)]
fn organization_samples(world: &World, t: &WireResourceId) -> Vec<FrameKind> {
    use phux_protocol::ids::{ActivationId, OperationId, Revision, ServerInstance};
    use phux_protocol::wire::frame::{
        ArrangementDoc, ExitPolicy, LiveTarget, MoveItem, OrgFilter, SessionLifetime, TerminateMode,
    };
    let k = &world.org;
    let op = OperationId::new([5; 16]).unwrap();
    let rev = Revision::new(1);
    vec![
        command(Command::GetOrganization {
            filter: OrgFilter::All,
            page_size: 0,
            cursor: None,
        }),
        command(Command::SessionCreate {
            operation: op,
            name: "new".to_owned(),
            lifetime: SessionLifetime::Persistent,
            attrs: Vec::new(),
            launch: None,
        }),
        command(Command::SessionRename {
            operation: op,
            session: k.session,
            name: "renamed".to_owned(),
        }),
        command(Command::SessionDelete {
            operation: op,
            session: k.session,
        }),
        command(Command::ResourceCreate {
            operation: op,
            session: k.session,
            kind: phux_protocol::ids::ResourceKind::Terminal,
            launch: None,
            name: None,
            on_exit: ExitPolicy::Remove,
            launch_mode: phux_protocol::wire::frame::LaunchMode::Immediate,
        }),
        command(Command::ResourceMove {
            operation: op,
            resource: k.resource,
            from: k.session,
            from_expected: rev,
            to: k.other_session,
            to_expected: rev,
        }),
        command(Command::ResourceMoveMany {
            operation: op,
            items: vec![MoveItem {
                resource: k.resource,
                from: k.session,
                from_expected: rev,
            }],
            to: k.other_session,
            to_expected: rev,
        }),
        command(Command::ResourceRename {
            operation: op,
            resource: k.resource,
            name: None,
        }),
        command(Command::ResourceTerminate {
            operation: op,
            target: LiveTarget {
                resource: t.clone(),
                instance: ServerInstance::new([0; 16]),
                activation: ActivationId::new(1),
            },
            mode: TerminateMode::Graceful,
        }),
        command(Command::ResourceRemove {
            operation: op,
            resource: k.resource,
        }),
        command(Command::ResourceRelaunch {
            operation: op,
            resource: k.resource,
            expected: rev,
        }),
        command(Command::OrganizationUndo {
            operation: op,
            undo: k.operation,
            expected: Vec::new(),
        }),
        command(Command::ArrangementGet {
            arrangement: k.arrangement,
        }),
        command(Command::ArrangementReplace {
            operation: op,
            arrangement: k.arrangement,
            expected: rev,
            document: ArrangementDoc::default(),
        }),
        command(Command::SessionSetProject {
            operation: op,
            session: k.session,
            project: Some("phux".to_owned()),
        }),
    ]
}

/// What a grant of only `verb`, at Global, must decide for `frame`,
/// read from the classifier's own row.
fn expected(frame: &FrameKind, verb: Verb) -> bool {
    let rule = frame_rule(frame);
    match rule.requirement {
        Requirement::Verbs(verbs) => {
            verbs == Verbs::of(&[verb]) && !rule.subject.requires_owner_uds()
        }
        Requirement::Exempt(Exemption::Liveness | Exemption::Cleanup | Exemption::SelfRead) => true,
        Requirement::Exempt(Exemption::Handshake) | Requirement::Nested | Requirement::Deny => {
            false
        }
    }
}

/// Rows no decodable client message can reach: the envelope row itself, and
/// allocations the codec does not decode.
const UNREACHABLE: [&str; 6] = [
    "`COMMAND`",
    "`SUBSCRIBE` (unallocated)",
    "`SPAWN` (unallocated)",
    "`RESIZE_TERMINAL` (unallocated)",
    "`RUN_HOOK` (unallocated)",
    "Unknown, retired, or otherwise unclassified command tag",
];

fn verb_only_connection_matrix(verb: Verb) {
    let mut world = world();
    let agent = agent_child(&mut world);
    let name = phux_protocol::scope::verb_name(verb);
    let grant = scoped(&[&format!("{name}@global")]);
    let samples = samples(&world, &agent);
    for frame in &samples {
        assert_eq!(
            admits(&world, &grant, frame),
            expected(frame, verb),
            "{name}-only grant on `{}`: {frame:?}",
            frame_rule(frame).case
        );
    }
    let hit: Vec<&str> = samples.iter().map(|frame| frame_rule(frame).case).collect();
    for rule in FRAME_RULES.iter().chain(COMMAND_RULES.iter()) {
        assert!(
            hit.contains(&rule.case) || UNREACHABLE.contains(&rule.case),
            "no sample reaches `{}`",
            rule.case
        );
    }
}

#[test]
fn inventory_only_connection_matrix() {
    verb_only_connection_matrix(Verb::Inventory);
}

#[test]
fn observe_only_connection_matrix() {
    verb_only_connection_matrix(Verb::Observe);
}

#[test]
fn create_only_connection_matrix() {
    verb_only_connection_matrix(Verb::Create);
}

#[test]
fn bind_only_connection_matrix() {
    verb_only_connection_matrix(Verb::Bind);
}

#[test]
fn input_only_connection_matrix() {
    verb_only_connection_matrix(Verb::Input);
}

#[test]
fn signal_only_connection_matrix() {
    verb_only_connection_matrix(Verb::Signal);
}

#[test]
fn the_owner_grant_admits_every_sample() {
    let mut world = world();
    let agent = agent_child(&mut world);
    let owner = ConnectionGrant::owner();
    for frame in &samples(&world, &agent) {
        assert!(admits(&world, &owner, frame), "{frame:?}");
    }
}
