/// A stable index per client-originated `FrameKind` variant; `None` for a
/// server-to-client variant. Exhaustive, like [`command_variant`].
const fn client_frame_variant(frame: &FrameKind) -> Option<usize> {
    match frame {
        FrameKind::Hello { .. } => Some(0),
        FrameKind::Ping { .. } => Some(1),
        FrameKind::Attach { .. } => Some(2),
        FrameKind::Detach => Some(3),
        FrameKind::InputKey { .. } => Some(4),
        FrameKind::InputMouse { .. } => Some(5),
        FrameKind::InputFocus { .. } => Some(6),
        FrameKind::InputPaste { .. } => Some(7),
        FrameKind::InputTerminalReply { .. } => Some(8),
        FrameKind::FrameAck { .. } => Some(9),
        FrameKind::ViewportResize { .. } => Some(10),
        FrameKind::HistoryRequest { .. } => Some(11),
        FrameKind::GetMetadata { .. } => Some(12),
        FrameKind::SetMetadata { .. } => Some(13),
        FrameKind::DeleteMetadata { .. } => Some(14),
        FrameKind::ListMetadata { .. } => Some(15),
        FrameKind::SubscribeMetadata { .. } => Some(16),
        FrameKind::ListDirectory { .. } => Some(17),
        FrameKind::SpawnResource { .. } => Some(18),
        FrameKind::MoveResource { .. } => Some(19),
        FrameKind::ResizeTerminal { .. } => Some(20),
        FrameKind::Command { .. } => Some(21),
        FrameKind::SubscribeEvents { .. } => Some(22),
        FrameKind::PathQuery { .. } => Some(23),
        FrameKind::HelloOk { .. }
        | FrameKind::Pong { .. }
        | FrameKind::ResourceOutput { .. }
        | FrameKind::Attached { .. }
        | FrameKind::AttachReady { .. }
        | FrameKind::Detached { .. }
        | FrameKind::BootstrapBegin { .. }
        | FrameKind::BootstrapChunk { .. }
        | FrameKind::BootstrapReady { .. }
        | FrameKind::HistoryPage { .. }
        | FrameKind::BootstrapTombstone { .. }
        | FrameKind::HistoryTombstone { .. }
        | FrameKind::HistoryRejected { .. }
        | FrameKind::Bell { .. }
        | FrameKind::Error { .. }
        | FrameKind::MetadataChanged { .. }
        | FrameKind::MetadataValue { .. }
        | FrameKind::MetadataKeys { .. }
        | FrameKind::DirectoryListing { .. }
        | FrameKind::PathResults { .. }
        | FrameKind::ResourceSpawned { .. }
        | FrameKind::ResourceMoved { .. }
        | FrameKind::ResourceClosed { .. }
        | FrameKind::CommandResult { .. }
        | FrameKind::Event { .. } => None,
    }
}

const CLIENT_FRAME_VARIANTS: usize = 24;

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
    owner_terminal: Option<ResourceId>,
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

fn agent(parent: ResourceId) -> SpawnResource {
    SpawnResource::agent_session(parent, "claude")
}

/// `resource` with a client idempotency key and a retention request.
fn keyed(resource: SpawnResource) -> SpawnResource {
    resource
        .with_retain_secs(Some(30))
        .with_idempotency_key(IdempotencyKey::new([3; 16]))
}

/// The three ways a spawn's kind can contradict its binding.
fn mismatched_spawns() -> [SpawnResource; 3] {
    let mut parented_terminal = agent(terminal());
    parented_terminal.kind = ResourceKind::Terminal;
    let mut orphan_agent = agent(terminal());
    orphan_agent.parent = None;
    let mut orphan_unknown = agent(terminal());
    orphan_unknown.kind = ResourceKind::Unknown { tag: 9 };
    orphan_unknown.parent = None;
    [parented_terminal, orphan_agent, orphan_unknown]
}

fn spawn_samples() -> Vec<(FrameKind, &'static Rule)> {
    let remote_parent = ResourceId::satellite("h", 7);
    let mut samples = vec![
        (spawn(None, None, None), &F_SPAWN_LOCAL),
        (spawn(None, Some(terminal()), None), &F_SPAWN_OWNED),
        (spawn(Some("h"), None, None), &F_SPAWN_SATELLITE),
        (
            spawn(Some("h"), Some(terminal()), None),
            &F_SPAWN_SATELLITE_OWNED,
        ),
        (
            spawn(None, None, Some(agent(terminal()))),
            &F_SPAWN_AGENT_LOCAL,
        ),
        (
            spawn(Some("h"), None, Some(agent(remote_parent.clone()))),
            &F_SPAWN_AGENT_SATELLITE,
        ),
        // A local or different-host parent under a satellite spawn, and an
        // agent session that also names an owner: no row covers them.
        (
            spawn(Some("h"), None, Some(agent(terminal()))),
            &F_UNCLASSIFIED,
        ),
        (
            spawn(Some("other"), None, Some(agent(remote_parent))),
            &F_UNCLASSIFIED,
        ),
        (
            spawn(None, Some(terminal()), Some(agent(terminal()))),
            &F_UNCLASSIFIED,
        ),
    ];
    // A client key (ADR-0126) or a retention request (ADR-0124) changes what
    // the server does with the spawn, never which row admits it.
    for (satellite, owner, expected) in [
        (None, None, &F_SPAWN_LOCAL),
        (None, Some(terminal()), &F_SPAWN_OWNED),
        (Some("h"), None, &F_SPAWN_SATELLITE),
    ] {
        samples.push((
            spawn(satellite, owner, Some(keyed(SpawnResource::default()))),
            expected,
        ));
    }
    samples.push((
        spawn(None, None, Some(keyed(agent(terminal())))),
        &F_SPAWN_AGENT_LOCAL,
    ));
    for resource in mismatched_spawns() {
        samples.push((spawn(None, None, Some(resource)), &F_SPAWN_KIND_MISMATCH));
    }
    samples
}

fn set(scope: Scope, key: &str, value: &[u8]) -> FrameKind {
    FrameKind::SetMetadata {
        request_id: 1,
        scope,
        key: key.to_owned(),
        value: value.to_vec(),
    }
}

fn delete(key: &str) -> FrameKind {
    FrameKind::DeleteMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: key.to_owned(),
    }
}

fn subscribe(key: &str) -> FrameKind {
    FrameKind::SubscribeMetadata {
        scope: Scope::Global,
        key: key.to_owned(),
    }
}

fn metadata_samples() -> Vec<(FrameKind, &'static Rule)> {
    let result = format!("{SESSION_CREATE_RESULT_KEY_PREFIX}token");
    let pane = Scope::Resource(terminal());
    let mark = encode_session_keep_empty("work", true);
    let unmark = encode_session_keep_empty("work", false);
    vec![
        (
            FrameKind::GetMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: WHOAMI_KEY.to_owned(),
            },
            &F_WHOAMI,
        ),
        (
            FrameKind::GetMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: RESOURCE_AGENT_KEY.to_owned(),
            },
            &F_GET_METADATA,
        ),
        (
            set(Scope::Global, SESSION_CREATE_KEY, b"{}"),
            &F_SESSION_CREATE,
        ),
        (
            set(Scope::Global, SESSION_NAME_KEY, b"old\0new"),
            &F_METADATA_WRITE,
        ),
        (
            set(Scope::Global, SESSION_KEEP_EMPTY_KEY, &mark),
            &F_KEEP_EMPTY_MARK,
        ),
        (
            set(Scope::Global, SESSION_KEEP_EMPTY_KEY, &unmark),
            &F_KEEP_EMPTY_CLEAR,
        ),
        (
            set(Scope::Global, SESSION_KEEP_EMPTY_KEY, b"work"),
            &F_KEEP_EMPTY_OTHER,
        ),
        (
            set(Scope::Global, CONFIG_RELOAD_KEY, b"1"),
            &F_CONFIG_RELOAD,
        ),
        (
            set(Scope::Global, SESSION_CREATE_RESULT_KEY, b"x"),
            &F_RESULT_NAMESPACE_WRITE,
        ),
        (set(Scope::Global, &result, b"x"), &F_RESULT_NAMESPACE_WRITE),
        (
            set(pane.clone(), RESOURCE_PANE_OCCUPANT_KEY, b"x"),
            &F_SERVER_OWNED_WRITE,
        ),
        (set(Scope::Global, WHOAMI_KEY, b"x"), &F_SERVER_OWNED_WRITE),
        // The special rows are Global-scoped; elsewhere the key is ordinary.
        (
            set(pane.clone(), SESSION_CREATE_KEY, b"{}"),
            &F_METADATA_WRITE,
        ),
        (set(pane, RESOURCE_TAGS_KEY, b"[]"), &F_METADATA_WRITE),
        (delete(SESSION_CREATE_RESULT_KEY), &F_RESULT_NAMESPACE_WRITE),
        (delete(RESOURCE_PANE_OCCUPANT_KEY), &F_SERVER_OWNED_WRITE),
        (delete(WHOAMI_KEY), &F_SERVER_OWNED_WRITE),
        (delete(CONFIG_RELOAD_KEY), &F_SERVER_OWNED_WRITE),
        (delete(SESSION_KEEP_EMPTY_KEY), &F_SERVER_OWNED_WRITE),
        (delete(SESSION_CREATE_KEY), &F_METADATA_WRITE),
        (
            FrameKind::ListMetadata {
                request_id: 1,
                scope: Scope::Global,
            },
            &F_LIST_METADATA,
        ),
        (subscribe(&result), &F_RESULT_NAMESPACE_SUBSCRIBE),
        (subscribe(RESOURCE_AGENT_KEY), &F_SUBSCRIBE_METADATA),
    ]
    .into_iter()
    .chain(approval_samples())
    .collect()
}

/// The decision key and the approval records (ADR-0128).
fn approval_samples() -> Vec<(FrameKind, &'static Rule)> {
    let id = "0123456789abcdef0123456789abcdef";
    let decide = format!("{APPROVAL_DECIDE_KEY_PREFIX}{id}");
    let record = format!("{APPROVAL_KEY_PREFIX}{id}");
    let pane = Scope::Resource(terminal());
    vec![
        (set(Scope::Global, &decide, b"approve"), &F_APPROVAL_DECIDE),
        (set(Scope::Global, &decide, b"deny"), &F_APPROVAL_DECIDE),
        (
            set(Scope::Global, &decide, b"maybe"),
            &F_APPROVAL_DECIDE_OTHER,
        ),
        (
            set(
                Scope::Global,
                &format!("{APPROVAL_DECIDE_KEY_PREFIX}NOT-AN-ID"),
                b"approve",
            ),
            &F_APPROVAL_DECIDE_OTHER,
        ),
        (set(Scope::Global, &record, b"{}"), &F_SERVER_OWNED_WRITE),
        (set(pane.clone(), &record, b"{}"), &F_SERVER_OWNED_WRITE),
        (delete(&record), &F_SERVER_OWNED_WRITE),
        (delete(&decide), &F_SERVER_OWNED_WRITE),
        // Only the Global decision is intercepted; elsewhere it is ordinary.
        (set(pane, &decide, b"approve"), &F_METADATA_WRITE),
    ]
}

fn input_samples() -> Vec<(FrameKind, &'static Rule)> {
    vec![
        (
            FrameKind::InputKey {
                terminal_id: terminal(),
                event: KeyEvent {
                    action: KeyAction::Press,
                    key: PhysicalKey::A,
                    mods: ModSet::empty(),
                    consumed_mods: ModSet::empty(),
                    composing: false,
                    text: None,
                    unshifted_codepoint: None,
                },
            },
            &F_INPUT,
        ),
        (
            FrameKind::InputMouse {
                terminal_id: terminal(),
                event: MouseEvent {
                    action: MouseAction::Press,
                    button: MouseButton::Left,
                    mods: ModSet::empty(),
                    x: 1.0,
                    y: 1.0,
                },
            },
            &F_INPUT,
        ),
        (
            FrameKind::InputFocus {
                terminal_id: terminal(),
                event: FocusEvent::Gained,
            },
            &F_INPUT,
        ),
        (
            FrameKind::InputPaste {
                terminal_id: terminal(),
                event: PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: b"x".to_vec(),
                },
            },
            &F_INPUT,
        ),
        (
            FrameKind::InputTerminalReply {
                terminal_id: terminal(),
                bytes: Bytes::from_static(b"\x1b[0n"),
            },
            &F_INPUT,
        ),
    ]
}

fn stream_samples() -> Vec<(FrameKind, &'static Rule)> {
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    vec![
        (
            FrameKind::FrameAck {
                terminal_id: terminal(),
                stream_id,
                bootstrap_id,
                seq: 1,
            },
            &F_FRAME_ACK,
        ),
        (
            FrameKind::HistoryRequest {
                terminal_id: terminal(),
                stream_id,
                bootstrap_id,
                cursor: Bytes::new(),
                max_bytes: 1,
                max_rows: 1,
            },
            &F_HISTORY_REQUEST,
        ),
        (
            FrameKind::ViewportResize {
                viewport: ViewportInfo::new(80, 24),
            },
            &F_VIEWPORT_RESIZE,
        ),
        (
            FrameKind::ResizeTerminal {
                terminal_id: terminal(),
                cols: 80,
                rows: 24,
                cell_px: None,
            },
            &F_RESIZE_TERMINAL,
        ),
        (
            FrameKind::MoveResource {
                request_id: 1,
                terminal: terminal(),
                owner_terminal: ResourceId::local(8),
            },
            &F_MOVE_RESOURCE,
        ),
        (
            FrameKind::SubscribeEvents {
                terminal: Some(terminal()),
                after_seq: None,
            },
            &F_SUBSCRIBE_EVENTS_ONE,
        ),
        (
            FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: None,
            },
            &F_SUBSCRIBE_EVENTS_ALL,
        ),
        // A journal cursor resumes the same subscription: same rows.
        (
            FrameKind::SubscribeEvents {
                terminal: Some(terminal()),
                after_seq: Some(41),
            },
            &F_SUBSCRIBE_EVENTS_ONE,
        ),
        (
            FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: Some(41),
            },
            &F_SUBSCRIBE_EVENTS_ALL,
        ),
    ]
}

fn connection_samples() -> Vec<(FrameKind, &'static Rule)> {
    vec![
        (
            FrameKind::Hello {
                client_name: "samples".to_owned(),
                protocol_major: 0,
                protocol_minor: 9,
                protocol_patch: 0,
                client_caps: ClientCapabilities::new(),
            },
            &F_HELLO,
        ),
        (FrameKind::Ping { nonce: 1 }, &F_PING),
        (FrameKind::Detach, &F_DETACH),
        (attach(AttachTarget::Last), &F_ATTACH),
        (attach(AttachTarget::ByName("work".to_owned())), &F_ATTACH),
        (attach(AttachTarget::ById(SessionId::new(1))), &F_ATTACH),
        (
            attach(AttachTarget::CreateIfMissing {
                name: "work".to_owned(),
                command: None,
                cwd: None,
            }),
            &F_ATTACH_CREATE,
        ),
        (
            FrameKind::ListDirectory {
                request_id: 1,
                path: String::new(),
                host: None,
            },
            &F_LIST_DIRECTORY,
        ),
        (
            FrameKind::PathQuery {
                request_id: 1,
                root: "/".to_owned(),
                query: String::new(),
                recursive: false,
                host: None,
            },
            &F_PATH_QUERY,
        ),
        // The envelope defers to the nested command's row.
        (
            FrameKind::Command {
                request_id: 1,
                command: Command::Upgrade,
            },
            &C_UPGRADE,
        ),
        (
            FrameKind::Command {
                request_id: 1,
                command: Command::Transcribe {
                    upload_id: upload(),
                    terminal_id: terminal(),
                },
            },
            &C_TRANSCRIBE,
        ),
    ]
}
