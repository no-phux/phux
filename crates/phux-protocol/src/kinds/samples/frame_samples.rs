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
        FrameKind::ClipboardReply { .. } => Some(24),
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

const CLIENT_FRAME_VARIANTS: usize = 25;

fn attach(target: AttachTarget) -> FrameKind {
    attach_as(target, None)
}

fn attach_as(target: AttachTarget, role_policy: Option<RolePolicy>) -> FrameKind {
    FrameKind::Attach {
        attach_id: 1,
        target,
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
        role_policy,
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
            &F_SPAWN_CHILD_LOCAL,
        ),
        (
            spawn(Some("h"), None, Some(agent(remote_parent.clone()))),
            &F_SPAWN_CHILD_SATELLITE,
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
        &F_SPAWN_CHILD_LOCAL,
    ));
    // Every launch field at once, and an agent naming its native session.
    let launched = FrameKind::SpawnResource {
        request_id: 2,
        group: GroupId::new(1),
        command: Some(vec!["zsh".to_owned(), "-i".to_owned()]),
        cwd: Some("/home/u".to_owned()),
        env: Some(vec![("LANG".to_owned(), "C".to_owned())]),
        term: Some("ghostty".to_owned()),
        satellite: Some(SatelliteHost::new("h")),
        owner_terminal: Some(ResourceId::local(8)),
        agent_session: Some(b"{}".to_vec()),
        initial_size: Some((132, 43)),
        resource: Some(Box::new(
            keyed(SpawnResource::default()).with_bind_instance(true),
        )),
    };
    samples.push((launched, &F_SPAWN_SATELLITE_OWNED));
    let native = agent(terminal()).with_native_id(Some("session-42".to_owned()));
    samples.push((spawn(None, None, Some(native)), &F_SPAWN_CHILD_LOCAL));
    for resource in mismatched_spawns() {
        samples.push((spawn(None, None, Some(resource)), &F_SPAWN_KIND_MISMATCH));
    }
    for (control, expected) in [
        (ControlClass::Page, &F_SPAWN_MACHINE_CONTROL),
        (ControlClass::Window, &F_SPAWN_MACHINE_CONTROL),
        (ControlClass::Display, &F_SPAWN_MACHINE_CONTROL),
    ] {
        let surface = SpawnResource {
            kind: ResourceKind::PixelSurface,
            control: Some(control),
            ..agent(terminal())
        };
        samples.push((spawn(None, None, Some(surface)), expected));
    }
    samples
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
        (
            FrameKind::ClipboardReply {
                terminal_id: terminal(),
                selection: b'c',
                text: crate::input::paste::ClipboardData("clip".to_owned()),
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
            FrameKind::ResizeTerminal {
                terminal_id: terminal(),
                cols: 80,
                rows: 24,
                cell_px: Some((8, 16)),
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
        (
            FrameKind::Hello {
                client_name: "samples".to_owned(),
                protocol_major: 1,
                protocol_minor: 0,
                protocol_patch: 0,
                client_caps: ClientCapabilities::new()
                    .with_compression(crate::caps::CompressionSet::from_bits(
                        crate::caps::CompressionSet::DEFLATE,
                    ))
                    .with_ssh_origin(crate::wire::ssh_origin::SshOrigin {
                        client: "192.0.2.1:50000".parse().unwrap(),
                        server: Some("192.0.2.2:22".parse().unwrap()),
                    })
                    .with_quic_streams(true)
                    .with_platform(crate::caps::ClientPlatform::parse("macos")),
            },
            &F_HELLO,
        ),
        (FrameKind::Ping { nonce: 1 }, &F_PING),
        (FrameKind::Detach, &F_DETACH),
        (attach(AttachTarget::Last), &F_ATTACH),
        (attach(AttachTarget::ByName("work".to_owned())), &F_ATTACH),
        (attach(AttachTarget::ById(SessionId::new(1))), &F_ATTACH),
        (
            attach_as(AttachTarget::Last, Some(RolePolicy::PRIMARY)),
            &F_ATTACH,
        ),
        (
            attach(AttachTarget::CreateIfMissing {
                name: "work".to_owned(),
                command: None,
                cwd: None,
            }),
            &F_ATTACH_CREATE,
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

/// The host path queries, with and without a satellite.
fn host_samples() -> Vec<(FrameKind, &'static Rule)> {
    vec![
        (
            FrameKind::ListDirectory {
                request_id: 1,
                path: String::new(),
                host: None,
            },
            &F_LIST_DIRECTORY,
        ),
        (
            FrameKind::ListDirectory {
                request_id: 1,
                path: "/home".to_owned(),
                host: Some(SatelliteHost::new("edge")),
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
        (
            FrameKind::PathQuery {
                request_id: 1,
                root: "/home".to_owned(),
                query: "src".to_owned(),
                recursive: true,
                host: Some(SatelliteHost::new("edge")),
            },
            &F_PATH_QUERY,
        ),
    ]
}
