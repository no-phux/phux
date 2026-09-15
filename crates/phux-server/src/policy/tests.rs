//! The grant engines and the dispatch guard, below the wire
//! (`docs/spec/workload-auth.md` §5-§8).
//!
//! The verb-only matrices derive every expectation from the classifier's own
//! row for the sample, so they pin the guard to `phux_protocol::kinds` rather
//! than to a second table: for each of the six verbs, a grant of only that
//! verb at Global admits exactly the rows that need only that verb and are
//! not owner-socket rows, admits the liveness and cleanup exemptions, and
//! refuses everything else.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use chrono::Utc;
use phux_core::ids::{ResourceId as CoreResourceId, WindowId};
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::ids::{
    BootstrapId, FileUploadId, GroupId, InputOperationId, ResourceId as WireResourceId,
    SatelliteHost, SessionId as WireSessionId, StreamId,
};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::kinds::{
    COMMAND_RULES, Exemption, FRAME_RULES, Requirement, Verb, Verbs, frame_rule,
};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{
    AttachTarget, CONFIG_RELOAD_KEY, Command, FrameKind, InputMode, KillPrecondition,
    ListenerTransport, ReportedAgentState, SESSION_CREATE_KEY, SESSION_KEEP_EMPTY_KEY, Scope,
    SpawnResource, StateScope, TerminalSignal, ViewportInfo, WHOAMI_KEY, encode_session_keep_empty,
};

use super::{
    Authority, ConnectionGrant, PolicyPosture, PostureError, Request, ScopedPolicy, enforce,
};
use crate::auth::AuthenticatedCredential;
use crate::state::{ClientId, ServerState};
use crate::workload::{ReloadingWorkloadRegistry, WorkloadRegistry};

const CLIENT: ClientId = ClientId(7);

/// Two sessions, one Terminal each, with the client attached to `alpha`.
struct World {
    state: ServerState,
    alpha: WireResourceId,
    beta: WireResourceId,
    alpha_core: CoreResourceId,
    alpha_window: WindowId,
    beta_window: WindowId,
    alpha_group: u32,
    /// A held kill of `alpha`, pending, so a decision on it resolves.
    approval: phux_protocol::ids::ApprovalId,
}

fn world() -> World {
    let mut state = ServerState::new();
    let (alpha_session, alpha_window, alpha_core) = state.seed_session("alpha");
    let (beta_session, beta_window, beta_core) = state.seed_session("beta");
    let alpha = state.intern_terminal_wire(alpha_core);
    let beta = state.intern_terminal_wire(beta_core);
    let alpha_group = state.idspace.intern_session(alpha_session).get();
    state.idspace.intern_session(beta_session);
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
        alpha_window,
        beta_window,
        alpha_group,
        approval,
    }
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
fn samples(world: &World) -> Vec<FrameKind> {
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
        FrameKind::SubscribeMetadata {
            scope: Scope::Resource(t.clone()),
            key: "phux.agent/v1".to_owned(),
        },
        FrameKind::Pong { nonce: 1 },
        command(Command::AttachResource {
            terminal_id: t.clone(),
            role_policy: None,
        }),
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
        command(Command::AppendResourceOutput {
            terminal_id: t,
            bytes: b"{}\n".to_vec(),
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
    let world = world();
    let name = phux_protocol::scope::verb_name(verb);
    let grant = scoped(&[&format!("{name}@global")]);
    let samples = samples(&world);
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
    let world = world();
    let owner = ConnectionGrant::owner();
    for frame in &samples(&world) {
        assert!(admits(&world, &owner, frame), "{frame:?}");
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
    // Nor server-global data.
    assert!(!admits_command(
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
        .registry_mut()
        .move_terminal(world.alpha_core, world.beta_window)
        .unwrap();
    assert!(
        !admits_command(&world, &grant, screen()),
        "the Group clause was flattened into a Terminal grant"
    );

    world
        .state
        .registry_mut()
        .move_terminal(world.alpha_core, world.alpha_window)
        .unwrap();
    assert!(admits_command(&world, &grant, screen()));
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

// -----------------------------------------------------------------------------
// Engines and posture.
// -----------------------------------------------------------------------------

/// A peer on `transport` running as the serving user.
fn peer(transport: TransportType) -> PeerIdentity {
    PeerIdentity {
        uid: nix::unistd::geteuid().as_raw(),
        pid: None,
        exe_path: None,
        mcp_host_key: None,
        transport,
        source_addr: None,
    }
}

fn credential(id: &str, scopes: &[&str]) -> AuthenticatedCredential {
    AuthenticatedCredential {
        id: id.to_owned(),
        principal: id.to_owned(),
        scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
        issued_at: Utc::now(),
        expires_at: None,
        generation: 1,
        registry_instance: None,
    }
}

/// A registry file holding one credential with `scopes`.
fn enrolled(scopes: &[&str]) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workload-keys");
    let scopes = scopes.iter().map(|scope| (*scope).to_owned()).collect();
    let id = WorkloadRegistry::register(&path, b"policy-test-key", scopes, None)
        .unwrap()
        .id;
    (dir, path, id)
}

fn paired_policy(path: &std::path::Path) -> ScopedPolicy {
    ScopedPolicy::paired(Arc::new(
        ReloadingWorkloadRegistry::load(path.to_owned()).unwrap(),
    ))
}

#[test]
fn local_policy_admits_only_the_owner_socket() {
    let policy = ScopedPolicy::local();
    assert!(
        policy
            .decide(&peer(TransportType::UnixSocket), None)
            .unwrap()
            .is_owner()
    );
    for transport in [
        TransportType::Quic,
        TransportType::WebSocket,
        TransportType::WebTransport,
        TransportType::SshTunnel,
        TransportType::Localhost,
    ] {
        assert!(
            policy.decide(&peer(transport), None).is_err(),
            "{transport:?}"
        );
    }
}

#[test]
fn paired_policy_mints_the_registry_ceiling_not_the_cached_scopes() {
    let (_dir, path, id) = enrolled(&["observe@host:devbox"]);
    let policy = paired_policy(&path);
    // The owner socket keeps the owner's grant in paired mode.
    assert!(
        policy
            .decide(&peer(TransportType::UnixSocket), None)
            .unwrap()
            .is_owner()
    );
    let grant = policy
        .decide(
            &peer(TransportType::Quic),
            Some(&credential(&id, &["*@global"])),
        )
        .unwrap();
    let Authority::Scoped { granted, .. } = &grant.authority else {
        panic!("a workload connection got the owner's grant");
    };
    assert_eq!(
        granted,
        &TerminalScopeSet::parse_all(&["observe@host:devbox"]).unwrap()
    );
    assert_eq!(grant.credential_id.as_deref(), Some(id.as_str()));
    assert_eq!(grant.registry_generation, 1);
    assert!(grant.registry_instance.is_some());
}

#[test]
fn paired_policy_refuses_bearer_unknown_and_revoked_credentials() {
    let (_dir, path, id) = enrolled(&["*@global"]);
    let policy = paired_policy(&path);
    let quic = peer(TransportType::Quic);
    assert!(policy.decide(&quic, None).is_err(), "no credential");
    assert!(
        policy
            .decide(&quic, Some(&credential("bearer-1", &["*@global"])))
            .is_err(),
        "a bearer credential is admission, never authority"
    );
    let unknown = format!("sha256:{}", "0".repeat(64));
    assert!(
        policy
            .decide(&quic, Some(&credential(&unknown, &[])))
            .is_err()
    );
    assert!(policy.decide(&quic, Some(&credential(&id, &[]))).is_ok());
    WorkloadRegistry::revoke(&path, &id).unwrap();
    assert!(
        policy.decide(&quic, Some(&credential(&id, &[]))).is_err(),
        "a revoked credential mints nothing at the next HELLO"
    );
}

#[test]
fn paired_mode_without_registry_denies_everything() {
    let dir = tempfile::tempdir().unwrap();
    let policy = paired_policy(&dir.path().join("workload-keys"));
    let id = format!("sha256:{}", "a".repeat(64));
    for transport in [
        TransportType::Quic,
        TransportType::WebSocket,
        TransportType::WebTransport,
    ] {
        assert!(
            policy
                .decide(&peer(transport), Some(&credential(&id, &["*@global"])))
                .is_err(),
            "{transport:?}"
        );
    }
}

#[test]
fn posture_follows_the_mode_the_environment_and_the_listeners() {
    use phux_config::PolicyMode::{Local, Paired};
    let cases = [
        (
            None,
            false,
            false,
            Ok(PolicyPosture::Transitional {
                remote_listener: false,
            }),
        ),
        (
            None,
            false,
            true,
            Ok(PolicyPosture::Transitional {
                remote_listener: true,
            }),
        ),
        (None, true, true, Ok(PolicyPosture::Paired)),
        (Some(Paired), false, true, Ok(PolicyPosture::Paired)),
        (Some(Local), false, false, Ok(PolicyPosture::Local)),
        (
            Some(Local),
            true,
            false,
            Err(PostureError::LocalWithWorkloadMtls),
        ),
        (
            Some(Local),
            false,
            true,
            Err(PostureError::LocalWithRemoteListener),
        ),
    ];
    for (mode, env, remote, want) in cases {
        assert_eq!(
            PolicyPosture::resolve(mode, env, remote),
            want,
            "{mode:?} env={env} remote={remote}"
        );
    }
    assert!(
        PolicyPosture::Transitional {
            remote_listener: true
        }
        .warns_remote_owner_grant()
    );
    assert!(
        !PolicyPosture::Transitional {
            remote_listener: false
        }
        .warns_remote_owner_grant()
    );
    assert!(PolicyPosture::Paired.requires_workload_mtls());
    assert!(!PolicyPosture::Local.requires_workload_mtls());
}

#[test]
fn denial_errors_are_limited_to_one_per_interval() {
    let mut grant = scoped(&["observe@global"]);
    let start = Instant::now();
    assert!(grant.admit_denial_error(start));
    assert!(!grant.admit_denial_error(start + Duration::from_millis(999)));
    assert!(grant.admit_denial_error(start + Duration::from_secs(1)));
}

#[test]
fn append_resource_output_is_admitted_through_the_parent_alone() {
    let mut world = world();
    let child = world
        .state
        .registry_mut()
        .new_agent_session(
            world.alpha_core,
            phux_core::resource::AgentFacet {
                provider: "claude".to_owned(),
                native_id: None,
                state: None,
            },
        )
        .unwrap();
    let child_wire = world.state.intern_terminal_wire(child);
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
fn viewport_resize_checks_every_pane_including_unnamed_ones() {
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
    world
        .state
        .add_pane_to_terminal_owner(&world.alpha)
        .expect("a pane beside alpha");
    assert!(
        !admits(&world, &one_pane, &resize),
        "an unnamed pane in the session is checked, not skipped"
    );
    let group = scoped(&[&format!("bind@group:{}", world.alpha_group)]);
    assert!(admits(&world, &group, &resize), "the Group contains it");
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
    for scope in ["*@global", "observe@host", "observe@host:devbox"] {
        assert!(validate_scopes(&[scope.to_owned()]).is_ok(), "{scope}");
    }
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
