//! Row-exact classifier coverage (ADR-0125).
//!
//! One sample per `Command` variant and per client-originated `FrameKind`
//! variant, with every payload-dependent split, each pinned to the exact row
//! it must land on, and that row checked against the method the catalog
//! files the message under. The index matches below are exhaustive, so a
//! new variant does not compile until it has an index, and the coverage
//! assertions then fail until it has a sample. Moving a message to another
//! row, even one with the same verbs, fails here; the spec golden in
//! `tests/kinds_table.rs` pins each row's requirement and subject.

use std::collections::BTreeSet;

use bytes::{Bytes, BytesMut};

use super::*;
use crate::caps::ClientCapabilities;
use crate::ids::{
    BootstrapId, FileUploadId, GroupId, IdempotencyKey, InputOperationId, SessionId, StreamId,
};
use crate::input::InputEvent;
use crate::input::focus::FocusEvent;
use crate::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use crate::input::mouse::{MouseAction, MouseButton, MouseEvent};
use crate::input::paste::{PasteEvent, PasteTrust};
use crate::wire::frame::{
    InputMode, KillPrecondition, ListenerTransport, ReportedAgentState, TerminalSignal,
    ViewportInfo, encode_session_keep_empty,
};

fn terminal() -> ResourceId {
    ResourceId::local(7)
}

fn upload() -> FileUploadId {
    FileUploadId::new([1; 16]).unwrap()
}

/// A stable index per `Command` variant. Exhaustive: a new variant is a
/// compile error here until it gets an index, then a sample below.
const fn command_variant(command: &Command) -> usize {
    match command {
        Command::AttachResource { .. } => 0,
        Command::DetachResource { .. } => 1,
        Command::KillResource { .. } => 2,
        Command::GetState { .. } => 3,
        Command::GetScreen { .. } => 4,
        Command::RouteInput { .. } => 5,
        Command::ApplyInput { .. } => 6,
        Command::KillResources { .. } => 7,
        Command::DetachClients { .. } => 8,
        Command::GetTerminalState { .. } => 9,
        Command::SubscribeResourceEvents { .. } => 10,
        Command::Upgrade => 11,
        Command::Shutdown => 12,
        Command::AcquireInput { .. } => 13,
        Command::ReleaseInput { .. } => 14,
        Command::SignalTerminal { .. } => 15,
        Command::PutFile { .. } => 16,
        Command::ReportAsked { .. } => 17,
        Command::ReportAgentState { .. } => 18,
        Command::GetPerf { .. } => 19,
        Command::Transcribe { .. } => 20,
        Command::AppendResourceOutput { .. } => 21,
        Command::KillResourceIf { .. } => 22,
        Command::OpenListener { .. } => 23,
    }
}

const COMMAND_VARIANTS: usize = 24;

#[allow(
    clippy::too_many_lines,
    reason = "one sample per Command variant, kept as one flat table"
)]
fn command_samples() -> Vec<(Command, &'static Rule)> {
    let focus = InputEvent::Focus(FocusEvent::Gained);
    vec![
        (
            Command::AttachResource {
                terminal_id: terminal(),
                role_policy: None,
            },
            &C_ATTACH_RESOURCE,
        ),
        (
            Command::DetachResource {
                terminal_id: terminal(),
            },
            &C_DETACH_RESOURCE,
        ),
        (
            Command::KillResource {
                terminal_id: terminal(),
            },
            &C_KILL_RESOURCE,
        ),
        (
            Command::GetState {
                scope: StateScope::Server,
            },
            &C_GET_STATE,
        ),
        (
            Command::GetScreen {
                terminal_id: terminal(),
                request_scrollback: None,
                cells: false,
                format: 0,
            },
            &C_GET_SCREEN,
        ),
        (
            Command::RouteInput {
                terminal_id: terminal(),
                event: focus.clone(),
            },
            &C_INPUT,
        ),
        (
            Command::ApplyInput {
                operation_id: InputOperationId::new([2; 16]).unwrap(),
                terminal_id: terminal(),
                events: vec![focus],
            },
            &C_INPUT,
        ),
        (
            Command::KillResources {
                ids: vec![terminal()],
            },
            &C_KILL_RESOURCES,
        ),
        (
            Command::DetachClients {
                session: Some("work".to_owned()),
            },
            &C_DETACH_CLIENTS_SESSION,
        ),
        (
            Command::DetachClients { session: None },
            &C_DETACH_CLIENTS_ALL,
        ),
        (
            Command::GetTerminalState {
                terminal_id: terminal(),
                include_scrollback: false,
                max_scrollback_lines: 0,
            },
            &C_GET_TERMINAL_STATE,
        ),
        (
            Command::SubscribeResourceEvents {
                terminal_id: terminal(),
                event_types: Vec::new(),
            },
            &C_SUBSCRIBE_RESOURCE_EVENTS,
        ),
        (Command::Upgrade, &C_UPGRADE),
        (Command::Shutdown, &C_SHUTDOWN),
        (
            Command::AcquireInput {
                terminal_id: terminal(),
                mode: InputMode::Cooperative,
                ttl_ms: 0,
            },
            &C_INPUT_LEASE,
        ),
        (
            Command::ReleaseInput {
                terminal_id: terminal(),
            },
            &C_INPUT_LEASE,
        ),
        (
            Command::SignalTerminal {
                terminal_id: terminal(),
                signal: TerminalSignal::Interrupt,
            },
            &C_SIGNAL_TERMINAL,
        ),
        (
            Command::PutFile {
                upload_id: upload(),
                terminal_id: terminal(),
                extension: "wav".to_owned(),
                offset: 0,
                data: vec![1],
                final_chunk: false,
                sha256: None,
            },
            &C_PUT_FILE,
        ),
        (
            Command::ReportAsked {
                terminal_id: terminal(),
                id: "q".to_owned(),
                question: "continue?".to_owned(),
                suggestions: Vec::new(),
                elapsed_seconds: None,
            },
            &C_AGENT_REPORT,
        ),
        (
            Command::ReportAgentState {
                terminal_id: terminal(),
                state: ReportedAgentState::Working,
            },
            &C_AGENT_REPORT,
        ),
        (Command::GetPerf { reset: false }, &C_GET_PERF),
        (Command::GetPerf { reset: true }, &C_GET_PERF_RESET),
        (
            Command::Transcribe {
                upload_id: upload(),
                terminal_id: terminal(),
            },
            &C_TRANSCRIBE,
        ),
        (
            Command::AppendResourceOutput {
                terminal_id: terminal(),
                bytes: b"{}\n".to_vec(),
            },
            &C_APPEND_RESOURCE_OUTPUT,
        ),
        (
            Command::KillResourceIf {
                terminal_id: terminal(),
                precondition: KillPrecondition::default(),
            },
            &C_KILL_RESOURCE_IF,
        ),
        (
            Command::OpenListener {
                transport: ListenerTransport::Quic,
                port_range: None,
                linger_secs: 0,
            },
            &C_OPEN_LISTENER,
        ),
    ]
}

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
        | FrameKind::ResourceSpawned { .. }
        | FrameKind::ResourceMoved { .. }
        | FrameKind::ResourceClosed { .. }
        | FrameKind::CommandResult { .. }
        | FrameKind::Event { .. } => None,
    }
}

const CLIENT_FRAME_VARIANTS: usize = 23;

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

fn frame_samples() -> Vec<(FrameKind, &'static Rule)> {
    let mut samples = connection_samples();
    samples.extend(input_samples());
    samples.extend(stream_samples());
    samples.extend(metadata_samples());
    samples.extend(spawn_samples());
    samples
}

fn command_frame(command: Command) -> BytesMut {
    let mut out = BytesMut::new();
    FrameKind::Command {
        request_id: 1,
        command,
    }
    .encode(&mut out);
    out
}

/// The nested tag of an encoded `COMMAND`. The envelope fields before the
/// command do not depend on it, so the tag sits where `UPGRADE`'s does: the
/// last byte of that bodyless frame.
fn encoded_command_tag(command: &Command) -> u8 {
    let upgrade = command_frame(Command::Upgrade);
    let offset = upgrade.len() - 1;
    assert_eq!(upgrade[offset], 0x0e, "UPGRADE's tag closes its frame");
    command_frame(command.clone())[offset]
}

fn names_row(method: &MethodSpec, rule: &Rule) -> bool {
    method.rules.iter().any(|row| std::ptr::eq(*row, rule))
}

#[test]
fn every_command_variant_lands_on_its_row() {
    let samples = command_samples();
    let covered: BTreeSet<usize> = samples
        .iter()
        .map(|(command, _)| command_variant(command))
        .collect();
    assert_eq!(
        covered,
        (0..COMMAND_VARIANTS).collect(),
        "every Command variant needs a sample"
    );
    for (command, expected) in &samples {
        let rule = command_rule(command);
        assert!(
            std::ptr::eq(rule, *expected),
            "{command:?} lands on `{}`, expected `{}`",
            rule.case,
            expected.case
        );
        let tag = encoded_command_tag(command);
        let method = command_method(tag).unwrap();
        assert!(
            names_row(method, rule),
            "{} does not name the row `{}` its instance lands on",
            method.name,
            rule.case
        );
    }
}

#[test]
fn every_client_frame_variant_lands_on_its_row() {
    let samples = frame_samples();
    let covered: BTreeSet<usize> = samples
        .iter()
        .filter_map(|(frame, _)| client_frame_variant(frame))
        .collect();
    assert_eq!(
        covered,
        (0..CLIENT_FRAME_VARIANTS).collect(),
        "every client-originated FrameKind variant needs a sample"
    );
    for (frame, expected) in &samples {
        let rule = frame_rule(frame);
        assert!(
            std::ptr::eq(rule, *expected),
            "{frame:?} lands on `{}`, expected `{}`",
            rule.case,
            expected.case
        );
        if matches!(frame, FrameKind::Command { .. }) {
            continue;
        }
        let method = frame_method(frame.type_byte()).unwrap();
        assert!(
            names_row(method, rule),
            "{} does not name the row `{}` its instance lands on",
            method.name,
            rule.case
        );
    }
}

#[test]
fn wrong_direction_frames_are_denied() {
    let unclassified: &Rule = &F_UNCLASSIFIED;
    for frame in [
        FrameKind::Pong { nonce: 1 },
        FrameKind::AttachReady { attach_id: 1 },
        FrameKind::Bell {
            terminal_id: terminal(),
        },
    ] {
        assert_eq!(client_frame_variant(&frame), None);
        assert!(std::ptr::eq(frame_rule(&frame), unclassified), "{frame:?}");
        assert!(frame_method(frame.type_byte()).is_none(), "{frame:?}");
    }
}

#[test]
fn only_exempt_or_read_only_methods_are_not_mutating() {
    static DENIED_ONLY: [&Rule; 1] = [&C_UNCLASSIFIED];
    let denied = MethodSpec {
        name: "DENIED",
        carrier: Carrier::Command(0xff),
        rules: &DENIED_ONLY,
        gate: None,
        shipped: false,
    };
    assert!(
        denied.mutating(),
        "a denied method never reads as read-only"
    );
    for name in ["COMMAND", "TRANSCRIBE", "SET_METADATA", "SPAWN_RESOURCE"] {
        assert!(method_named(name).unwrap().mutating(), "{name}");
    }
    for name in [
        "HELLO",
        "PING",
        "DETACH",
        "DETACH_RESOURCE",
        "GET_SCREEN",
        "GET_STATE",
        "SUBSCRIBE_METADATA",
        WHOAMI_KEY,
    ] {
        assert!(!method_named(name).unwrap().mutating(), "{name}");
    }
}
