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
use phux_protocol::wire::info::SessionSnapshot;

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
// The §6 tables, row by row: each row's verbs on its own subject selector.
// -----------------------------------------------------------------------------

/// One §6 row as the guard must decide it: `frame` reaches the row; every
/// grant set in `admit` is admitted; every set in `deny` is refused.
struct Row {
    frame: FrameKind,
    admit: Vec<Vec<String>>,
    deny: Vec<Vec<String>>,
}

fn row(frame: FrameKind, admit: &[&[&str]], deny: &[&[&str]]) -> Row {
    let sets = |sets: &[&[&str]]| -> Vec<Vec<String>> {
        sets.iter()
            .map(|set| set.iter().map(|scope| (*scope).to_owned()).collect())
            .collect()
    };
    Row {
        frame,
        admit: sets(admit),
        deny: sets(deny),
    }
}

/// `set` with `verb` taken out of every grant; a grant left with no verb
/// is dropped.
fn without_verb(set: &[String], verb: Verb) -> Vec<String> {
    let name = phux_protocol::scope::verb_name(verb);
    set.iter()
        .filter_map(|scope| {
            let (verbs, selector) = scope.split_once('@').unwrap();
            let verbs: Vec<&str> = if verbs == "*" {
                Verb::ALL
                    .iter()
                    .map(|verb| phux_protocol::scope::verb_name(*verb))
                    .collect()
            } else {
                verbs.split(',').collect()
            };
            let kept: Vec<&str> = verbs.into_iter().filter(|kept| *kept != name).collect();
            (!kept.is_empty()).then(|| format!("{}@{selector}", kept.join(",")))
        })
        .collect()
}

/// Whether the grant `set` spells admits `frame`; an empty set grants
/// nothing.
fn admits_set(world: &World, set: &[String], frame: &FrameKind) -> bool {
    if set.is_empty() {
        return false;
    }
    let scopes: Vec<&str> = set.iter().map(String::as_str).collect();
    admits(world, &scoped(&scopes), frame)
}

/// Every row of both §6 tables a client can reach, with the narrowest grant
/// that admits it and the near misses that must not.
#[allow(
    clippy::too_many_lines,
    reason = "one entry per spec row, kept as one flat table"
)]
fn rows(world: &mut World) -> Vec<Row> {
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
    let child = world.state.intern_terminal_wire(child);
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
        denied(samples(world)[0].clone()),
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
            command(Command::AttachResource {
                terminal_id: t.clone(),
                role_policy: None,
            }),
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
    rows.push(on_terminal(
        command(Command::ReleaseInput { terminal_id: t }),
        "bind",
    ));
    rows
}

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
        .add_pane_to_terminal_owner(&world.alpha)
        .expect("a pane beside alpha");
    assert!(
        admits(&world, &one_pane, &resize),
        "an unsubscribed pane is not a viewport target"
    );
    world.state.subscribe_terminal(CLIENT, pane, None);
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
    let foreign = world.state.terminal_from_wire(&world.beta).unwrap();
    world.state.subscribe_terminal(CLIENT, foreign, None);
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

/// The Bell events a server-wide subscription holding `scopes` receives,
/// after one Bell each on `alpha`, `beta`, no Terminal, and a child of
/// `alpha`, in that order.
fn server_wide_bells(scopes: &[&str]) -> Vec<Option<WireResourceId>> {
    use crate::state::EventRecord;
    use phux_protocol::wire::frame::AgentEvent;

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
