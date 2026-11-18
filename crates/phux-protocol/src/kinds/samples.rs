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

use crate::wire::frame::{ControlClass, RolePolicy};

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
    InputMode, KillPrecondition, ListenerTransport, PerfOp, ReportedAgentState, TerminalSignal,
    ViewportInfo, encode_session_keep_empty,
};

fn terminal() -> ResourceId {
    ResourceId::local(7)
}

fn upload() -> FileUploadId {
    FileUploadId::new([1; 16]).unwrap()
}

/// One sample per forwarding op, then each bounded op at the largest values
/// the decoder accepts; all share a row.
fn forward_samples() -> Vec<(Command, &'static Rule)> {
    use crate::forward::{
        ForwardOp, MAX_AGENT_MESSAGE_BYTES, MAX_FORWARD_ADDR_BYTES, MAX_FORWARD_CHUNK_BYTES,
    };
    [
        ForwardOp::Open {
            addr: "localhost".to_owned(),
            port: 3000,
        },
        ForwardOp::Read {
            id: 1,
            max_bytes: 1,
        },
        ForwardOp::Write {
            id: 1,
            bytes: vec![1],
            fin: false,
        },
        ForwardOp::Close { id: 1 },
        ForwardOp::AgentOffer,
        ForwardOp::AgentReply {
            id: 1,
            message: vec![crate::forward::SSH_AGENT_FAILURE],
        },
        ForwardOp::Open {
            addr: "x".repeat(MAX_FORWARD_ADDR_BYTES),
            port: u16::MAX,
        },
        ForwardOp::Read {
            id: u64::MAX,
            max_bytes: u32::try_from(MAX_FORWARD_CHUNK_BYTES).unwrap(),
        },
        ForwardOp::Write {
            id: u64::MAX,
            bytes: vec![0xa5; MAX_FORWARD_CHUNK_BYTES],
            fin: false,
        },
        ForwardOp::Write {
            id: 1,
            bytes: Vec::new(),
            fin: true,
        },
        ForwardOp::AgentReply {
            id: u64::MAX,
            message: vec![5; MAX_AGENT_MESSAGE_BYTES],
        },
    ]
    .into_iter()
    .map(|op| (Command::Forward { op }, &C_FORWARD))
    .collect()
}

/// One sample per service-HTTP command, then a request carrying every
/// optional part and a read at the largest size the decoder accepts; all
/// share a row.
fn service_http_samples() -> Vec<(Command, &'static Rule)> {
    use crate::service_http::{MAX_SERVICE_HTTP_READ_BYTES, ServiceHttpRequest};
    let bare = ServiceHttpRequest {
        host: None,
        service: "opencode".to_owned(),
        method: "GET".to_owned(),
        target: "/api/info".to_owned(),
        headers: Vec::new(),
        body: Vec::new(),
    };
    let full = ServiceHttpRequest {
        host: Some("mini".to_owned()),
        method: "POST".to_owned(),
        target: "/api/session?directory=%2Ftmp".to_owned(),
        headers: vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("accept".to_owned(), "text/event-stream".to_owned()),
        ],
        body: br#"{"title":"x"}"#.to_vec(),
        ..bare.clone()
    };
    [
        Command::ServiceHttpOpen {
            request: Box::new(bare),
        },
        Command::ServiceHttpRead {
            exchange: 1,
            max_bytes: 1,
        },
        Command::ServiceHttpClose { exchange: 1 },
        Command::ServiceHttpOpen {
            request: Box::new(full),
        },
        Command::ServiceHttpRead {
            exchange: u64::MAX,
            max_bytes: u32::try_from(MAX_SERVICE_HTTP_READ_BYTES).unwrap(),
        },
    ]
    .into_iter()
    .map(|command| (command, &C_SERVICE_HTTP))
    .collect()
}

fn operation() -> IdempotencyKey {
    IdempotencyKey::new([3; 16]).unwrap()
}

fn session_key() -> crate::ids::SessionKey {
    crate::ids::SessionKey::new([4; 16]).unwrap()
}

fn resource_key() -> crate::ids::ResourceKey {
    crate::ids::ResourceKey::new([5; 16]).unwrap()
}

include!("samples/command_samples.rs");
include!("samples/frame_samples.rs");
include!("samples/metadata_samples.rs");

/// Every client-originated frame shape, each with the row it lands on. The
/// wire schema samples these too, so each optional field appears both set
/// and unset.
#[allow(
    clippy::redundant_pub_crate,
    reason = "the wire schema's samples, outside `kinds`, read it"
)]
pub(crate) fn frame_samples() -> Vec<(FrameKind, &'static Rule)> {
    let mut samples = connection_samples();
    samples.extend(input_samples());
    samples.extend(stream_samples());
    samples.extend(metadata_samples());
    samples.extend(spawn_samples());
    samples.extend(host_samples());
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

/// Read the nested tag through the public TLV decoder. Command field lengths
/// use varints, so a larger native JSON body changes its tag's byte offset.
fn encoded_command_tag(command: &Command) -> u8 {
    let bytes = command_frame(command.clone());
    let mut decoder = crate::wire::decode::Decoder::new(&bytes);
    decoder.read_u32_be().unwrap();
    decoder.read_u8().unwrap();
    while let Some((field_id, value)) = decoder.read_field().unwrap() {
        if field_id == 2 {
            return *value.first().unwrap();
        }
    }
    panic!("COMMAND field missing from encoded frame");
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

/// Assert that server-to-client `frame` has no client index, classifies as
/// unclassified, and is carried by no catalog method. The wire-schema
/// samples run it over every server-to-client variant.
#[allow(
    clippy::redundant_pub_crate,
    reason = "the wire schema's tests, outside `kinds`, call it"
)]
pub(crate) fn assert_wrong_direction_denied(frame: &FrameKind) {
    let unclassified: &Rule = &F_UNCLASSIFIED;
    assert_ne!(frame.type_byte() & 0x80, 0, "{frame:?} is client to server");
    assert_eq!(client_frame_variant(frame), None, "{frame:?}");
    assert!(std::ptr::eq(frame_rule(frame), unclassified), "{frame:?}");
    assert!(frame_method(frame.type_byte()).is_none(), "{frame:?}");
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
        dangerous: false,
        served_by: None,
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

/// The dangerous methods include held actions and credential delivery:
/// every consumer confirmation (MCP `destructiveHint` and `confirm`, CLI
/// `--yes`) derives from this set, so it is pinned here by name.
#[test]
fn dangerous_methods_are_exactly_the_adr_0128_list() {
    let dangerous: BTreeSet<&str> = methods()
        .filter(|method| method.dangerous)
        .map(|method| method.name)
        .collect();
    let golden: BTreeSet<&str> = [
        "KILL_RESOURCE",
        "KILL_RESOURCE_IF",
        "KILL_RESOURCES",
        "CLOSE_TAB_RESOURCES",
        "CREDENTIALS",
        "SIGNAL_TERMINAL",
        "DETACH_CLIENTS",
        "SHUTDOWN",
        "OPEN_LISTENER",
        "UPGRADE",
        CONFIG_RELOAD_KEY,
        APPROVAL_DECIDE_KEY_PREFIX,
        "SESSION_DELETE",
        "RESOURCE_TERMINATE",
        "RESOURCE_REMOVE",
        "SESSION_KILL",
    ]
    .into_iter()
    .collect();
    assert_eq!(dangerous, golden);
}

/// R07: the v2 verbs gate only the organization commands and the
/// delegation commands that came with them (ADR-0163). Every legacy command
/// and frame keeps exactly the verbs it needed before, so no stored grant
/// loses an operation, and every organization method is gated on
/// `features_ext.ORGANIZATION`.
#[test]
fn only_organization_commands_need_the_v2_verbs() {
    let v2 = Verbs::of(&[Verb::Organize, Verb::Terminate]);
    for (command, rule) in command_samples() {
        let needs_v2 = rule.verb_set().bits() & v2.bits() != 0;
        if matches!(
            command,
            Command::DelegateAgent { .. } | Command::RevokeDelegation { .. }
        ) {
            assert_eq!(rule.verb_set(), Verbs::of(&[Verb::Terminate]));
            assert!(rule.subject.requires_owner_uds(), "{command:?}");
        } else if command.is_organization() {
            let method = command_method(encoded_command_tag(&command)).unwrap();
            let gate = match command {
                Command::SessionKill { .. } => crate::caps::ServerFeatureExt::SessionKill,
                Command::SessionSetProject { .. } => crate::caps::ServerFeatureExt::SessionProject,
                Command::ArrangementLock { .. } => crate::caps::ServerFeatureExt::TabLock,
                Command::GetCreation { .. } => crate::caps::ServerFeatureExt::ApiDescription,
                _ => crate::caps::ServerFeatureExt::Organization,
            };
            assert_eq!(method.gate, Some(FeatureGate::ext(gate)), "{}", method.name);
        } else {
            assert!(!needs_v2, "{command:?} must keep its legacy verbs");
        }
    }
    for (frame, rule) in frame_samples() {
        assert_eq!(rule.verb_set().bits() & v2.bits(), 0, "{frame:?}");
    }
}

#[test]
fn freeze_resume_and_deny_are_the_payload_exceptions() {
    let signal = |signal| Command::SignalTerminal {
        terminal_id: terminal(),
        signal,
        operation_id: None,
    };
    for reversible in [TerminalSignal::Freeze, TerminalSignal::Resume] {
        assert!(!command_is_dangerous(&signal(reversible)));
    }
    for ending in [
        TerminalSignal::Interrupt,
        TerminalSignal::Terminate,
        TerminalSignal::Kill,
    ] {
        assert!(command_is_dangerous(&signal(ending)));
    }
    assert!(command_is_dangerous(&Command::KillResources {
        ids: vec![],
        operation_id: None,
    }));
    assert!(!command_is_dangerous(&Command::GetState {
        scope: StateScope::Server
    }));
    let decide = format!("{APPROVAL_DECIDE_KEY_PREFIX}0123456789abcdef0123456789abcdef");
    assert!(frame_is_dangerous(&set(Scope::Global, &decide, b"approve")));
    assert!(!frame_is_dangerous(&set(Scope::Global, &decide, b"deny")));
    assert!(frame_is_dangerous(&set(
        Scope::Global,
        CONFIG_RELOAD_KEY,
        b"1"
    )));
    assert!(!frame_is_dangerous(&set(
        Scope::Global,
        RESOURCE_TAGS_KEY,
        b"[]"
    )));
}

/// Every served child kind lands on the parent rows, not default-deny, so a
/// scoped grant can open one beside a Terminal it may bind.
#[test]
fn every_served_child_kind_spawns_on_the_parent_rows() {
    let remote_parent = ResourceId::satellite("h", 7);
    for spec in KINDS.iter().filter(|spec| !spec.kind.is_terminal()) {
        let child = |parent: ResourceId| SpawnResource {
            kind: spec.kind,
            ..agent(parent)
        };
        for (frame, expected) in [
            (
                spawn(None, None, Some(child(terminal()))),
                &F_SPAWN_CHILD_LOCAL,
            ),
            (
                spawn(Some("h"), None, Some(child(remote_parent.clone()))),
                &F_SPAWN_CHILD_SATELLITE,
            ),
            (
                spawn(None, Some(terminal()), Some(child(terminal()))),
                &F_UNCLASSIFIED,
            ),
        ] {
            assert!(std::ptr::eq(frame_rule(&frame), expected), "{}", spec.name);
        }
    }
    let unknown = SpawnResource {
        kind: ResourceKind::Unknown { tag: 9 },
        ..agent(terminal())
    };
    let unclassified: &Rule = &F_UNCLASSIFIED;
    assert!(std::ptr::eq(
        frame_rule(&spawn(None, None, Some(unknown))),
        unclassified
    ));
}
