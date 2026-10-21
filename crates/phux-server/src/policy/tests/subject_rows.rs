/// Every row of both §6 tables a client can reach, with the narrowest grant
/// that admits it and the near misses that must not.
#[allow(
    clippy::too_many_lines,
    reason = "one entry per spec row, kept as one flat table"
)]
fn rows(world: &mut World) -> Vec<Row> {
    let child = agent_child(world);
    let native = native_samples(world, &child);
    let t = world.alpha.clone();
    let beta_group = world
        .state
        .find_session_by_name("beta")
        .and_then(|session| world.state.idspace.session_wire(session))
        .unwrap()
        .get();
    let s = |verbs: &str, selector: &str| format!("{verbs}@{selector}");
    let ta = format!("terminal:{}", local_id(&t));
    let tb = format!("terminal:{}", local_id(&world.beta));
    let tc = format!("terminal:{}", local_id(&child));
    let ga = format!("group:{}", world.alpha_group);
    let gb = format!("group:{beta_group}");
    let upload = FileUploadId::new([1; 16]).unwrap();
    let focus = InputEvent::Focus(FocusEvent::Gained);

    // A named-Terminal row needing `verbs`: the Terminal, its Group, and
    // the local host admit; the other Terminal and a satellite do not.
    let on_terminal = |frame: FrameKind, verbs: &str| -> Row {
        let (on_t, on_g) = (s(verbs, &ta), s(verbs, &ga));
        let (on_other, remote) = (s(verbs, &tb), s(verbs, "host:h"));
        row(
            frame,
            &[&[&on_t], &[&on_g], &[&s(verbs, "host")]],
            &[&[&on_other], &[&remote], &[&s(verbs, &gb)]],
        )
    };
    // A Global row needing `verbs`: only Global admits.
    let on_global = |frame: FrameKind, verbs: &str| -> Row {
        row(
            frame,
            &[&[&s(verbs, "global")]],
            &[&[&s(verbs, "host")], &[&s(verbs, &ta)]],
        )
    };
    let denied = |frame: FrameKind| row(frame, &[], &[&["*@global"]]);
    let exempt = |frame: FrameKind| row(frame, &[&["inventory@terminal:999"]], &[]);

    let command = |command: Command| FrameKind::Command {
        request_id: 1,
        command,
    };
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    let mut rows = vec![
        denied(samples(world, &child)[0].clone()),
        exempt(FrameKind::Ping { nonce: 1 }),
        exempt(FrameKind::Detach),
        row(
            attach(AttachTarget::ByName("alpha".to_owned())),
            &[&[&s("bind,observe", &ga)], &[&s("bind,observe", "host")]],
            &[
                &[&s("bind,observe", &gb)],
                &[&s("bind,observe", &ta)],
                &[&s("bind,observe", "host:h")],
            ],
        ),
        row(
            attach(AttachTarget::CreateIfMissing {
                name: "fresh".to_owned(),
                command: None,
                cwd: None,
            }),
            &[&[&s("create,bind,observe", "host")]],
            &[
                &[&s("create,bind,observe", &ga)],
                &[&s("create,bind,observe", &ta)],
            ],
        ),
        on_terminal(
            FrameKind::HistoryRequest {
                terminal_id: t.clone(),
                stream_id,
                bootstrap_id,
                cursor: Bytes::new(),
                max_bytes: 1,
                max_rows: 1,
            },
            "observe",
        ),
        on_terminal(
            FrameKind::FrameAck {
                terminal_id: t.clone(),
                stream_id,
                bootstrap_id,
                seq: 1,
            },
            "observe",
        ),
        on_terminal(
            FrameKind::InputFocus {
                terminal_id: t.clone(),
                event: FocusEvent::Gained,
            },
            "input",
        ),
        row(
            FrameKind::ViewportResize {
                viewport: ViewportInfo::new(80, 24),
            },
            &[&[&s("bind", &ta)], &[&s("bind", &ga)]],
            &[&[&s("bind", &tb)], &[&s("bind", &gb)]],
        ),
        row(
            spawn(Some("h"), None, None),
            &[&[&s("create", "host:h")]],
            &[&[&s("create", "host")], &[&s("create", "host:other")]],
        ),
        row(
            spawn(None, None, None),
            &[&[&s("create", "host")]],
            &[&[&s("create", &ga)], &[&s("create", "host:h")]],
        ),
        row(
            spawn(None, Some(t.clone()), None),
            &[&[&s("create", &ga), &s("bind", &ta)]],
            &[
                &[&s("create", &gb), &s("bind", &ta)],
                &[&s("create", &ga), &s("bind", &tb)],
            ],
        ),
        denied(spawn(Some("h"), Some(t.clone()), None)),
        row(
            spawn(
                None,
                None,
                Some(SpawnResource::agent_session(t.clone(), "claude")),
            ),
            &[&[&s("create", &ga), &s("bind", &ta)]],
            &[
                &[&s("create", &gb), &s("bind", &ta)],
                &[&s("create", &ga), &s("bind", &tb)],
            ],
        ),
        row(
            spawn(
                Some("h"),
                None,
                Some(SpawnResource::agent_session(
                    WireResourceId::satellite("h", 7),
                    "claude",
                )),
            ),
            &[&[&s("create", "host:h"), &s("bind", "terminal:h/7")]],
            &[
                &[&s("create", "host:h"), &s("bind", "terminal:h/8")],
                &[&s("create", "host:other"), &s("bind", "terminal:h/7")],
            ],
        ),
        // A different-host parent is default-deny, whatever is granted.
        denied(spawn(
            Some("h"),
            None,
            Some(SpawnResource::agent_session(
                WireResourceId::satellite("other", 7),
                "claude",
            )),
        )),
        denied(spawn(
            None,
            None,
            Some(SpawnResource::agent_session(t.clone(), "claude")).map(|mut orphan| {
                orphan.parent = None;
                orphan
            }),
        )),
        on_terminal(
            FrameKind::ResizeTerminal {
                terminal_id: t.clone(),
                cols: 80,
                rows: 24,
                cell_px: None,
            },
            "bind",
        ),
        row(
            FrameKind::MoveResource {
                request_id: 1,
                terminal: t.clone(),
                owner_terminal: world.beta.clone(),
            },
            &[&[&s("bind", &ta), &s("bind", &tb)]],
            &[&[&s("bind", &ta)], &[&s("bind", &tb)]],
        ),
        on_terminal(
            FrameKind::SubscribeEvents {
                terminal: Some(t.clone()),
                after_seq: None,
            },
            "observe",
        ),
        // Filtered at the source: OBSERVE anywhere admits.
        row(
            FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: None,
            },
            &[&[&s("observe", &tb)], &[&s("observe", "host:h")]],
            &[&["inventory,create,bind,input,signal@global"]],
        ),
        exempt(FrameKind::GetMetadata {
            request_id: 1,
            scope: Scope::Global,
            key: WHOAMI_KEY.to_owned(),
        }),
        on_terminal(
            FrameKind::GetMetadata {
                request_id: 1,
                scope: Scope::Resource(t.clone()),
                key: "phux.agent/v1".to_owned(),
            },
            "observe",
        ),
        on_global(set(Scope::Global, SESSION_CREATE_KEY, b"{}"), "create,bind"),
        on_global(
            set(
                Scope::Global,
                SESSION_KEEP_EMPTY_KEY,
                &encode_session_keep_empty("alpha", true),
            ),
            "create,bind",
        ),
        row(
            set(
                Scope::Global,
                SESSION_KEEP_EMPTY_KEY,
                &encode_session_keep_empty("alpha", false),
            ),
            &[&[&s("signal", &ga)], &[&s("signal", "host")]],
            &[&[&s("signal", &gb)], &[&s("signal", &ta)]],
        ),
        denied(set(Scope::Global, SESSION_KEEP_EMPTY_KEY, b"alpha")),
        on_global(set(Scope::Global, CONFIG_RELOAD_KEY, b"1"), "signal"),
        // The held kill names `alpha`: SIGNAL on it decides.
        row(
            set(Scope::Global, &world.approval.decide_key(), b"approve"),
            &[&[&s("signal", &ta)], &[&s("signal", &ga)]],
            &[&[&s("signal", &tb)], &[&s("signal", "host:h")]],
        ),
        denied(set(Scope::Global, &world.approval.decide_key(), b"maybe")),
        denied(set(Scope::Global, "phux.session.created/v1", b"x")),
        denied(FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: "phux.session.created/v1".to_owned(),
        }),
        denied(set(Scope::Global, WHOAMI_KEY, b"x")),
        on_terminal(
            set(Scope::Resource(t.clone()), "phux.tags/v1", b"[]"),
            "bind",
        ),
        on_global(
            FrameKind::ListMetadata {
                request_id: 1,
                scope: Scope::Global,
            },
            "inventory",
        ),
        on_global(
            FrameKind::ListDirectory {
                request_id: 1,
                path: String::new(),
                host: None,
            },
            "inventory",
        ),
        on_global(
            FrameKind::PathQuery {
                request_id: 1,
                root: "~".to_owned(),
                query: String::new(),
                recursive: false,
                host: None,
            },
            "inventory",
        ),
        on_terminal(
            FrameKind::SubscribeMetadata {
                scope: Scope::Resource(t.clone()),
                key: "phux.agent/v1".to_owned(),
            },
            "observe",
        ),
        denied(FrameKind::Pong { nonce: 1 }),
        // --- Nested commands ---
        on_terminal(
            command(Command::attach_terminal(t.clone(), None)),
            "bind,observe",
        ),
        exempt(command(Command::DetachResource {
            terminal_id: t.clone(),
        })),
        on_terminal(
            command(Command::KillResource {
                terminal_id: t.clone(),
                operation_id: None,
            }),
            "signal",
        ),
        on_terminal(
            command(Command::KillResourceIf {
                terminal_id: t.clone(),
                precondition: KillPrecondition::default(),
                operation_id: None,
            }),
            "signal",
        ),
        on_terminal(command(get_screen(t.clone())), "observe"),
        on_terminal(
            command(Command::ApplyInput {
                operation_id: InputOperationId::new([2; 16]).unwrap(),
                terminal_id: t.clone(),
                events: vec![focus],
            }),
            "input",
        ),
        // All-or-nothing: a grant on one of the two named Terminals fails.
        row(
            command(Command::KillResources {
                ids: vec![t.clone(), world.beta.clone()],
                operation_id: None,
            }),
            &[
                &[&s("signal", &ta), &s("signal", &tb)],
                &[&s("signal", "host")],
            ],
            &[&[&s("signal", &ta)], &[&s("signal", &tb)]],
        ),
        row(
            command(Command::CloseTabResources {
                ids: vec![t.clone(), world.beta.clone()],
            }),
            &[&[&s("signal", &ta), &s("signal", &tb)]],
            &[&[&s("signal", &ta)], &[&s("signal", &gb)]],
        ),
        // Filtered at the source: INVENTORY anywhere admits.
        row(
            command(Command::GetState {
                scope: StateScope::Server,
            }),
            &[&[&s("inventory", &tb)], &[&s("inventory", "host:h")]],
            &[&["observe,create,bind,input,signal@global"]],
        ),
        on_terminal(
            command(Command::GetTerminalState {
                terminal_id: t.clone(),
                include_scrollback: false,
                max_scrollback_lines: 0,
            }),
            "inventory",
        ),
        on_terminal(
            command(Command::SubscribeResourceEvents {
                terminal_id: t.clone(),
                event_types: Vec::new(),
            }),
            "observe",
        ),
        on_global(command(Command::Upgrade), "signal"),
        on_terminal(
            command(Command::AcquireInput {
                terminal_id: t.clone(),
                mode: InputMode::Cooperative,
                ttl_ms: 0,
            }),
            "bind",
        ),
        on_terminal(
            command(Command::SignalTerminal {
                terminal_id: t.clone(),
                signal: TerminalSignal::Interrupt,
                operation_id: None,
            }),
            "signal",
        ),
        on_terminal(
            command(Command::ReportAgentState {
                terminal_id: t.clone(),
                state: ReportedAgentState::Working,
            }),
            "bind",
        ),
        on_terminal(
            command(Command::PutFile {
                upload_id: upload,
                terminal_id: t.clone(),
                extension: "wav".to_owned(),
                offset: 0,
                data: vec![1],
                final_chunk: false,
                sha256: None,
            }),
            "input",
        ),
        on_global(
            command(Command::GetFile {
                host: None,
                path: "/tmp/deck.pptx".to_owned(),
                offset: 0,
                max_bytes: 1,
            }),
            "input",
        ),
        on_terminal(
            command(Command::Checkout {
                terminal_id: t.clone(),
                request: phux_protocol::checkout::CheckoutRequest::Stage {
                    paths: vec!["a.txt".to_owned()],
                },
            }),
            "input",
        ),
        on_terminal(
            command(Command::Transcribe {
                upload_id: upload,
                terminal_id: t.clone(),
            }),
            "input",
        ),
        row(
            command(Command::DetachClients {
                session: Some("alpha".to_owned()),
            }),
            &[&[&s("signal", &ga)], &[&s("signal", "host")]],
            &[&[&s("signal", &gb)], &[&s("signal", &ta)]],
        ),
        on_global(command(Command::DetachClients { session: None }), "signal"),
        // Owner-socket rows: no remote grant reaches them.
        denied(command(Command::Shutdown)),
        denied(command(Command::OpenListener {
            transport: ListenerTransport::Quic,
            port_range: None,
            linger_secs: 0,
        })),
        denied(command(Command::PerfControl {
            op: PerfOp::TraceStop,
        })),
        on_global(command(Command::GetPerf { reset: false }), "observe"),
        on_global(command(Command::GetPerf { reset: true }), "observe,bind"),
        // Through the parent alone: a grant naming only the child fails.
        row(
            command(Command::AppendResourceOutput {
                terminal_id: child,
                bytes: b"{}\n".to_vec(),
            }),
            &[&[&s("bind,input", &ta)], &[&s("bind,input", &ga)]],
            &[&[&s("bind,input", &tc)], &[&s("bind,input", &tb)]],
        ),
    ];
    // The other members of shared rows, each checked like the first.
    for frame in [
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
        command(Command::RouteInput {
            terminal_id: t.clone(),
            event: InputEvent::Focus(FocusEvent::Gained),
        }),
    ] {
        rows.push(on_terminal(frame, "input"));
    }
    // Organization rows, in `organization_samples` order.
    let org = organization_samples(world, &t);
    let on_alpha = |frame: &FrameKind, verbs: &str| {
        row(frame.clone(), &[&[&s(verbs, &ga)]], &[&[&s(verbs, &gb)]])
    };
    let on_alpha_resource = |frame: &FrameKind, verbs: &str| {
        row(
            frame.clone(),
            &[&[&s(verbs, &ta)], &[&s(verbs, &ga)]],
            &[&[&s(verbs, &tb)], &[&s(verbs, &gb)]],
        )
    };
    let moving = |frame: &FrameKind| {
        row(
            frame.clone(),
            &[&[&s("organize", &ga), &s("organize", &gb)]],
            &[&[&s("organize", &ga)], &[&s("organize", &gb)]],
        )
    };
    rows.extend([
        // Filtered at the source: INVENTORY anywhere admits.
        row(
            org[0].clone(),
            &[&[&s("inventory", &tb)], &[&s("inventory", "host:h")]],
            &[&["observe,create,bind,input,signal,organize,terminate@global"]],
        ),
        on_global(org[1].clone(), "create"),
        on_alpha(&org[2], "organize"),
        on_alpha(&org[3], "terminate"),
        on_alpha(&org[4], "create"),
        moving(&org[5]),
        moving(&org[6]),
        on_alpha_resource(&org[7], "organize"),
        on_terminal(org[8].clone(), "terminate"),
        on_alpha_resource(&org[9], "terminate"),
        on_alpha_resource(&org[10], "create"),
        // An undo whose receipt records no inverse needs Global.
        on_global(org[11].clone(), "organize"),
        on_alpha(&org[12], "inventory"),
        on_alpha(&org[13], "organize"),
        on_alpha(&org[14], "organize"),
    ]);
    rows.push(on_terminal(
        command(Command::ReleaseInput { terminal_id: t }),
        "bind",
    ));
    // Owner-socket only: no scoped grant mints or
    // withdraws a delegation.
    rows.extend(delegation_samples(world).into_iter().map(denied));
    rows.extend([
        row(
            native[0].clone(),
            &[&[&s("create,bind", &ga)], &[&s("create,bind", "host")]],
            &[&[&s("create,bind", &gb)], &[&s("create,bind", &ta)]],
        ),
        row(
            native[1].clone(),
            &[
                &[&s("input", &tc)],
                &[&s("input", &ta)],
                &[&s("input", &ga)],
                &[&s("input", "host")],
            ],
            &[&[&s("input", &tb)], &[&s("input", &gb)]],
        ),
        on_terminal(native[2].clone(), "bind"),
        row(
            native[3].clone(),
            &[&[&s("inventory", &ga)], &[&s("inventory", "host")]],
            &[&[&s("observe", &ga)]],
        ),
        row(
            native[4].clone(),
            &[
                &[&s("create,bind,input", &ga)],
                &[&s("create,bind,input", "host")],
            ],
            &[&[&s("create,bind,input", &gb)], &[&s("create,bind", &ga)]],
        ),
    ]);
    rows
}
