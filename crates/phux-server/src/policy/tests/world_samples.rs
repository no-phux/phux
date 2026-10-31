/// Keys of `alpha`'s records, its arrangement, and the operation that
/// created the arrangement.
struct OrgKeys {
    session: phux_protocol::ids::SessionKey,
    other_session: phux_protocol::ids::SessionKey,
    resource: phux_protocol::ids::ResourceKey,
    arrangement: phux_protocol::ids::ArrangementKey,
    operation: phux_protocol::ids::OperationId,
}

/// Commit an arrangement for `session` showing `pane`, returning the
/// arrangement's key and the operation that created it.
fn arrange(
    state: &mut ServerState,
    session: CoreSessionId,
    pane: CoreResourceId,
) -> (phux_core::ArrangementKey, phux_core::OperationId) {
    let session_key = state.registry().session_key(session).unwrap();
    let resource = state.registry().resource_key(pane).unwrap();
    let commit = state
        .commit(
            phux_core::Op::ArrangementReplace {
                owner: phux_core::ArrangementOwner::Session(session_key),
                // Every session starts with an empty arrangement at revision 1.
                expected: phux_core::Revision::FIRST,
                groups: vec![phux_core::Group {
                    id: [9; 16],
                    name: None,
                    tree: phux_core::LayoutNode::Leaf(phux_core::EntryRef::member(
                        [8; 16], resource,
                    )),
                }],
            },
            &phux_core::Actor::owner(),
        )
        .unwrap();
    let key = state
        .org()
        .session(&session_key)
        .and_then(|s| s.arrangement)
        .unwrap();
    (key, commit.applied.operation)
}

/// Two sessions, one Terminal each, with the client attached to `alpha`.
struct World {
    state: ServerState,
    alpha: WireResourceId,
    beta: WireResourceId,
    alpha_core: CoreResourceId,
    alpha_session: CoreSessionId,
    beta_session: CoreSessionId,
    alpha_group: u32,
    /// The organization keys the organization command samples name.
    org: OrgKeys,
    /// A held kill of `alpha`, pending, so a decision on it resolves.
    approval: phux_protocol::ids::ApprovalId,
}

fn world() -> World {
    let mut state = ServerState::new();
    let (alpha_session, alpha_core) = state.seed_session("alpha");
    let (beta_session, beta_core) = state.seed_session("beta");
    let alpha = state.intern_resource_wire(alpha_core);
    let beta = state.intern_resource_wire(beta_core);
    let alpha_group = state.idspace.intern_session(alpha_session).get();
    state.idspace.intern_session(beta_session);
    let (arrangement, operation) = arrange(&mut state, alpha_session, alpha_core);
    let org = OrgKeys {
        session: crate::org_wire::session_key(state.registry().session_key(alpha_session).unwrap())
            .unwrap(),
        other_session: crate::org_wire::session_key(
            state.registry().session_key(beta_session).unwrap(),
        )
        .unwrap(),
        resource: crate::org_wire::resource_key(state.registry().resource_key(alpha_core).unwrap())
            .unwrap(),
        arrangement: crate::org_wire::arrangement_key(arrangement).unwrap(),
        operation: crate::org_wire::operation(operation).unwrap(),
    };
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    state.attach_default_caps(CLIENT, "alpha", tx).unwrap();
    let approval = state
        .open_approval(
            ClientId(99),
            &Command::KillResource {
                terminal_id: alpha.clone(),
                operation_id: None,
            },
        )
        .unwrap()
        .id;
    World {
        state,
        alpha,
        beta,
        alpha_core,
        alpha_session,
        beta_session,
        alpha_group,
        org,
        approval,
    }
}

fn agent_child(world: &mut World) -> WireResourceId {
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
    world.state.intern_resource_wire(child)
}

fn local_id(wire: &WireResourceId) -> u32 {
    match wire {
        WireResourceId::Local { id } | WireResourceId::Satellite { id, .. } => *id,
    }
}

fn scoped(scopes: &[&str]) -> ConnectionGrant {
    let granted = TerminalScopeSet::parse_all(scopes).unwrap();
    ConnectionGrant::scoped(granted, Some("sha256:test".to_owned())).unwrap()
}

fn admits(world: &World, grant: &ConnectionGrant, frame: &FrameKind) -> bool {
    enforce(&world.state, CLIENT, grant, Request::Frame(frame)).is_ok()
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "every test builds its command inline"
)]
fn admits_command(world: &World, grant: &ConnectionGrant, command: Command) -> bool {
    enforce(&world.state, CLIENT, grant, Request::Command(&command)).is_ok()
}

fn get_screen(terminal_id: WireResourceId) -> Command {
    Command::GetScreen {
        terminal_id,
        request_scrollback: None,
        cells: false,
        format: 0,
    }
}

fn attach(target: AttachTarget) -> FrameKind {
    FrameKind::Attach {
        attach_id: 1,
        target,
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
        role_policy: None,
    }
}

fn spawn(
    satellite: Option<&str>,
    owner_terminal: Option<WireResourceId>,
    resource: Option<SpawnResource>,
) -> FrameKind {
    FrameKind::SpawnResource {
        request_id: 1,
        group: GroupId::new(1),
        command: None,
        cwd: None,
        env: None,
        term: None,
        satellite: satellite.map(SatelliteHost::new),
        owner_terminal,
        agent_session: None,
        initial_size: None,
        resource: resource.map(Box::new),
    }
}

fn set(scope: Scope, key: &str, value: &[u8]) -> FrameKind {
    FrameKind::SetMetadata {
        request_id: 1,
        scope,
        key: key.to_owned(),
        value: value.to_vec(),
    }
}

fn command(command: Command) -> FrameKind {
    FrameKind::Command {
        request_id: 1,
        command,
    }
}

// -----------------------------------------------------------------------------
// One sample per row the client can reach.
// -----------------------------------------------------------------------------

#[allow(
    clippy::too_many_lines,
    reason = "one sample per reachable row, kept as one flat table"
)]
fn samples(world: &World, agent: &WireResourceId) -> Vec<FrameKind> {
    let t = world.alpha.clone();
    let upload = FileUploadId::new([1; 16]).unwrap();
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    let focus = InputEvent::Focus(FocusEvent::Gained);
    let key = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::A,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    };
    let remote_parent = WireResourceId::satellite("h", 7);
    vec![
        FrameKind::Hello {
            client_name: "matrix".to_owned(),
            protocol_major: 0,
            protocol_minor: 9,
            protocol_patch: 0,
            client_caps: ClientCapabilities::new(),
        },
        FrameKind::Ping { nonce: 1 },
        FrameKind::Detach,
        attach(AttachTarget::Last),
        attach(AttachTarget::ByName("alpha".to_owned())),
        attach(AttachTarget::ById(WireSessionId::new(world.alpha_group))),
        attach(AttachTarget::CreateIfMissing {
            name: "fresh".to_owned(),
            command: None,
            cwd: None,
        }),
        FrameKind::HistoryRequest {
            terminal_id: t.clone(),
            stream_id,
            bootstrap_id,
            cursor: Bytes::new(),
            max_bytes: 1,
            max_rows: 1,
        },
        FrameKind::FrameAck {
            terminal_id: t.clone(),
            stream_id,
            bootstrap_id,
            seq: 1,
        },
        FrameKind::InputKey {
            terminal_id: t.clone(),
            event: key,
        },
        FrameKind::InputMouse {
            terminal_id: t.clone(),
            event: MouseEvent {
                action: MouseAction::Press,
                button: MouseButton::Left,
                mods: ModSet::empty(),
                x: 1.0,
                y: 1.0,
            },
        },
        FrameKind::InputFocus {
            terminal_id: t.clone(),
            event: FocusEvent::Gained,
        },
        FrameKind::InputPaste {
            terminal_id: t.clone(),
            event: PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"x".to_vec(),
            },
        },
        FrameKind::InputTerminalReply {
            terminal_id: t.clone(),
            bytes: Bytes::from_static(b"\x1b[0n"),
        },
        FrameKind::ViewportResize {
            viewport: ViewportInfo::new(80, 24),
        },
        spawn(Some("h"), None, None),
        spawn(None, None, None),
        spawn(None, Some(t.clone()), None),
        spawn(Some("h"), Some(t.clone()), None),
        spawn(
            None,
            None,
            Some(SpawnResource::agent_session(t.clone(), "claude")),
        ),
        spawn(
            Some("h"),
            None,
            Some(SpawnResource::agent_session(remote_parent, "claude")),
        ),
        spawn(
            None,
            None,
            Some(SpawnResource::agent_session(t.clone(), "claude")).map(|mut orphan| {
                orphan.parent = None;
                orphan
            }),
        ),
        FrameKind::ResizeTerminal {
            terminal_id: t.clone(),
            cols: 80,
            rows: 24,
            cell_px: None,
        },
        FrameKind::MoveResource {
            request_id: 1,
            terminal: t.clone(),
            owner_terminal: world.beta.clone(),
        },
        FrameKind::SubscribeEvents {
            terminal: Some(t.clone()),
            after_seq: None,
        },
        FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: None,
        },
        FrameKind::GetMetadata {
            request_id: 1,
            scope: Scope::Global,
            key: WHOAMI_KEY.to_owned(),
        },
        FrameKind::GetMetadata {
            request_id: 1,
            scope: Scope::Resource(t.clone()),
            key: "phux.agent/v1".to_owned(),
        },
        set(Scope::Global, SESSION_CREATE_KEY, b"{}"),
        set(
            Scope::Global,
            SESSION_KEEP_EMPTY_KEY,
            &encode_session_keep_empty("alpha", true),
        ),
        set(
            Scope::Global,
            SESSION_KEEP_EMPTY_KEY,
            &encode_session_keep_empty("alpha", false),
        ),
        set(Scope::Global, SESSION_KEEP_EMPTY_KEY, b"alpha"),
        set(Scope::Global, CONFIG_RELOAD_KEY, b"1"),
        set(Scope::Global, &world.approval.decide_key(), b"approve"),
        set(Scope::Global, &world.approval.decide_key(), b"maybe"),
        set(Scope::Global, "phux.session.created/v1", b"x"),
        FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: "phux.session.created/v1".to_owned(),
        },
        set(Scope::Global, WHOAMI_KEY, b"x"),
        set(Scope::Resource(t.clone()), "phux.tags/v1", b"[]"),
        FrameKind::ListMetadata {
            request_id: 1,
            scope: Scope::Global,
        },
        FrameKind::ListDirectory {
            request_id: 1,
            path: String::new(),
            host: None,
        },
        FrameKind::PathQuery {
            request_id: 1,
            root: "~".to_owned(),
            query: String::new(),
            recursive: false,
            host: None,
        },
        FrameKind::SubscribeMetadata {
            scope: Scope::Resource(t.clone()),
            key: "phux.agent/v1".to_owned(),
        },
        FrameKind::Pong { nonce: 1 },
        command(Command::attach_terminal(t.clone(), None)),
        command(Command::DetachResource {
            terminal_id: t.clone(),
        }),
        command(Command::KillResource {
            terminal_id: t.clone(),
            operation_id: None,
        }),
        command(Command::KillResourceIf {
            terminal_id: t.clone(),
            precondition: KillPrecondition::default(),
            operation_id: None,
        }),
        command(get_screen(t.clone())),
        command(Command::RouteInput {
            terminal_id: t.clone(),
            event: focus.clone(),
        }),
        command(Command::ApplyInput {
            operation_id: InputOperationId::new([2; 16]).unwrap(),
            terminal_id: t.clone(),
            events: vec![focus],
        }),
        command(Command::KillResources {
            ids: vec![t.clone()],
            operation_id: None,
        }),
        command(Command::CloseTabResources {
            ids: vec![t.clone()],
        }),
        command(Command::DescribeApi),
        command(Command::GetState {
            scope: StateScope::Server,
        }),
        command(Command::GetTerminalState {
            terminal_id: t.clone(),
            include_scrollback: false,
            max_scrollback_lines: 0,
        }),
        command(Command::SubscribeResourceEvents {
            terminal_id: t.clone(),
            event_types: Vec::new(),
        }),
        command(Command::Upgrade),
        command(Command::AcquireInput {
            terminal_id: t.clone(),
            mode: InputMode::Cooperative,
            ttl_ms: 0,
        }),
        command(Command::SignalTerminal {
            terminal_id: t.clone(),
            signal: TerminalSignal::Interrupt,
            operation_id: None,
        }),
        command(Command::ReportAgentState {
            terminal_id: t.clone(),
            state: ReportedAgentState::Working,
        }),
        command(Command::PutFile {
            upload_id: upload,
            terminal_id: t.clone(),
            extension: "wav".to_owned(),
            offset: 0,
            data: vec![1],
            final_chunk: false,
            sha256: None,
        }),
        command(Command::GetFile {
            host: None,
            path: "/tmp/deck.pptx".to_owned(),
            offset: 0,
            max_bytes: 1,
        }),
        command(Command::Checkout {
            terminal_id: t.clone(),
            request: phux_protocol::checkout::CheckoutRequest::Status,
        }),
        command(Command::ServiceHttpClose { exchange: 1 }),
        command(Command::Transcribe {
            upload_id: upload,
            terminal_id: t.clone(),
        }),
        command(Command::DetachClients {
            session: Some("alpha".to_owned()),
        }),
        command(Command::DetachClients { session: None }),
        command(Command::Shutdown),
        command(Command::OpenListener {
            transport: ListenerTransport::Quic,
            port_range: None,
            linger_secs: 0,
        }),
        command(Command::GetPerf { reset: false }),
        command(Command::GetPerf { reset: true }),
        command(Command::PerfControl {
            op: PerfOp::TraceStart,
        }),
        command(Command::AppendResourceOutput {
            terminal_id: t.clone(),
            bytes: b"{}\n".to_vec(),
        }),
    ]
    .into_iter()
    .chain(organization_samples(world, &t))
    .chain(delegation_samples(world))
    .chain(native_samples(world, agent))
    .collect()
}
