//! The machine-readable wire schema against the codec (ADR-0117).
//!
//! `docs/spec/wire-schema.json` lists every frame's type byte and, for each
//! top-level TLV field, its id, name, value type, and presence. Everything
//! except the value type is derived here from the codec itself: frame names
//! from an exhaustive match over [`FrameKind`], field names and ids from
//! `wire/field.rs`, presence from encoding samples of every variant and
//! decoding each with one field stripped, and retired ids from the gaps in a
//! frame's id range. The rendered result must equal the committed file byte
//! for byte, so the codec cannot change shape without a schema diff in the
//! same change. Each declared value type is then checked against every
//! sampled value of its field.
//!
//! A new `FrameKind` variant fails to compile in [`frame_entry`] until it has
//! a name and an index, then fails [`every_variant_has_a_sample`] until it
//! has a sample. After an intended codec change, regenerate with
//! `PHUX_UPDATE_WIRE_SCHEMA=1 cargo nextest run -p phux-protocol wire_schema_matches`,
//! set the `type` of any new field (written as `?`), and review the diff.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use bytes::{Bytes, BytesMut};
use serde::{Deserialize, Serialize};

use super::{
    Decoder, decode_actor_ref, decode_agent_event, decode_attach_target, decode_bootstrap_codec,
    decode_bootstrap_profile, decode_client_capabilities, decode_command, decode_command_result,
    decode_env, decode_key_event, decode_mouse_event, decode_move_result, decode_paste_event,
    decode_scope, decode_server_capabilities, decode_session_snapshot, decode_spawn_result,
    decode_string_list, decode_terminal_id, decode_viewport_info,
};
use crate::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ClientCapabilities, Compression,
    CompressionSet, ServerCapabilities,
};
use crate::ids::{
    BootstrapId, ClientId, GroupId, IdempotencyKey, ResourceId, SatelliteHost, ServerInstance,
    SessionId, StreamId, WindowId,
};
use crate::input::focus::FocusEvent;
use crate::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use crate::input::mouse::{MouseAction, MouseButton, MouseEvent};
use crate::input::paste::{PasteEvent, PasteTrust};
use crate::wire::encode::wire_type;
use crate::wire::frame::{
    ActorRef, AgentEvent, AttachTarget, CloseReason, Command, CommandResult, DetachReason,
    DirectoryEntry, DirectoryErrorCode, DirectoryListing, DirectoryListingError, ErrorCode,
    EventStamp, FrameKind, HistoryRejectionReason, HistoryTombstoneReason, MoveResult,
    PathErrorCode, PathKind, PathQueryError, PathRow, PathStatus, RolePolicy, Scope, SpawnResource,
    SpawnResult, TYPE_FRAME_COMPRESSED, TombstoneReason, ViewportInfo,
};
use crate::wire::info::SessionSnapshot;
use crate::wire::ssh_origin::{SshOrigin, decode_ssh_origin};

/// Repository-relative path of the schema.
const SCHEMA_PATH: &str = "docs/spec/wire-schema.json";

/// Set to regenerate the schema from the codec instead of checking it.
const UPDATE_ENV: &str = "PHUX_UPDATE_WIRE_SCHEMA";

/// The field-id table the codec encodes and decodes by.
const FIELD_RS: &str = include_str!("../field.rs");

/// `field.rs` modules that number a field-tagged record nested inside a
/// positional value, not a frame body. The schema covers frame bodies only.
const NESTED_RECORD_MODULES: [&str; 6] = [
    "event_journal_gap",
    "event_source_gap",
    "snapshot_extension",
    "resource_state",
    "event_asked",
    "event_pane_spawned",
];

/// Placeholder `type` the regenerator writes for a field it has not seen.
const UNSET_TYPE: &str = "?";

// ---------------------------------------------------------------------------
// Schema file model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Schema {
    description: Vec<String>,
    types: BTreeMap<String, String>,
    frames: Vec<FrameEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameEntry {
    name: String,
    type_byte: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retired: Vec<u32>,
    fields: Vec<FieldEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldEntry {
    id: u32,
    name: String,
    #[serde(rename = "type")]
    ty: String,
    presence: Presence,
}

/// Whether a decoder accepts the frame without the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Presence {
    /// Present in every encoding; every decode without it fails.
    Required,
    /// May be absent: some encoding omits it, or a decode without it passes.
    Optional,
}

/// Compact JSON for one leaf of the rendered schema.
fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap()
}

/// Render `schema` with one field per line, so a field change is a one-line
/// diff. Leaves go through `serde_json` for escaping.
fn render(schema: &Schema) -> String {
    let mut out = String::from("{\n  \"description\": [\n");
    push_lines(
        &mut out,
        schema
            .description
            .iter()
            .map(|line| format!("    {}", json(line))),
    );
    out.push_str("  ],\n  \"types\": {\n");
    push_lines(
        &mut out,
        schema
            .types
            .iter()
            .map(|(name, doc)| format!("    {}: {}", json(name), json(doc))),
    );
    out.push_str("  },\n  \"frames\": [\n");
    push_lines(&mut out, schema.frames.iter().map(render_frame));
    out.push_str("  ]\n}\n");
    out
}

fn render_frame(frame: &FrameEntry) -> String {
    let mut out = String::from("    {\n");
    let _ = writeln!(out, "      \"name\": {},", json(&frame.name));
    let _ = writeln!(out, "      \"type_byte\": {},", frame.type_byte);
    if !frame.retired.is_empty() {
        let _ = writeln!(out, "      \"retired\": {},", json(&frame.retired));
    }
    if frame.fields.is_empty() {
        out.push_str("      \"fields\": []\n    }");
        return out;
    }
    out.push_str("      \"fields\": [\n");
    push_lines(
        &mut out,
        frame
            .fields
            .iter()
            .map(|field| format!("        {}", json(field))),
    );
    out.push_str("      ]\n    }");
    out
}

/// Append `lines` joined by `,\n`, ending with a newline.
fn push_lines(out: &mut String, lines: impl Iterator<Item = String>) {
    let lines: Vec<String> = lines.collect();
    out.push_str(&lines.join(",\n"));
    if !lines.is_empty() {
        out.push('\n');
    }
}

// ---------------------------------------------------------------------------
// Frame catalog: names, field modules, variant coverage
// ---------------------------------------------------------------------------

/// Number of [`FrameKind`] variants; [`frame_entry`] indexes them `0..`.
const FRAME_VARIANTS: usize = 49;

/// `(variant index, spec name, field.rs module)` for one frame. Exhaustive,
/// so a new variant is a compile error here until it is named.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per FrameKind variant, kept as one flat table"
)]
const fn frame_entry(frame: &FrameKind) -> (usize, &'static str, &'static str) {
    match frame {
        FrameKind::Hello { .. } => (0, "HELLO", "hello"),
        FrameKind::HelloOk { .. } => (1, "HELLO_OK", "hello_ok"),
        FrameKind::Ping { .. } => (2, "PING", "ping"),
        FrameKind::Pong { .. } => (3, "PONG", "ping"),
        FrameKind::ResourceOutput { .. } => (4, "RESOURCE_OUTPUT", "terminal_output"),
        FrameKind::Attach { .. } => (5, "ATTACH", "attach"),
        FrameKind::Detach => (6, "DETACH", ""),
        FrameKind::InputKey { .. } => (7, "INPUT_KEY", "input_key"),
        FrameKind::InputMouse { .. } => (8, "INPUT_MOUSE", "input_mouse"),
        FrameKind::InputFocus { .. } => (9, "INPUT_FOCUS", "input_focus"),
        FrameKind::InputPaste { .. } => (10, "INPUT_PASTE", "input_paste"),
        FrameKind::InputTerminalReply { .. } => {
            (11, "INPUT_TERMINAL_REPLY", "input_terminal_reply")
        }
        FrameKind::FrameAck { .. } => (12, "FRAME_ACK", "frame_ack"),
        FrameKind::ViewportResize { .. } => (13, "VIEWPORT_RESIZE", "viewport_resize"),
        FrameKind::Attached { .. } => (14, "ATTACHED", "attached"),
        FrameKind::AttachReady { .. } => (15, "ATTACH_READY", "attach_ready"),
        FrameKind::Detached { .. } => (16, "DETACHED", "detached"),
        FrameKind::BootstrapBegin { .. } => (17, "BOOTSTRAP_BEGIN", "bootstrap_begin"),
        FrameKind::BootstrapChunk { .. } => (18, "BOOTSTRAP_CHUNK", "bootstrap_chunk"),
        FrameKind::BootstrapReady { .. } => (19, "BOOTSTRAP_READY", "bootstrap_ready"),
        FrameKind::HistoryRequest { .. } => (20, "HISTORY_REQUEST", "history_request"),
        FrameKind::HistoryPage { .. } => (21, "HISTORY_PAGE", "history_page"),
        FrameKind::BootstrapTombstone { .. } => (22, "BOOTSTRAP_TOMBSTONE", "bootstrap_tombstone"),
        FrameKind::HistoryTombstone { .. } => (23, "HISTORY_TOMBSTONE", "history_tombstone"),
        FrameKind::HistoryRejected { .. } => (24, "HISTORY_REJECTED", "history_rejected"),
        FrameKind::Bell { .. } => (25, "BELL", "bell"),
        FrameKind::Error { .. } => (26, "ERROR", "error"),
        FrameKind::GetMetadata { .. } => (27, "GET_METADATA", "get_metadata"),
        FrameKind::SetMetadata { .. } => (28, "SET_METADATA", "set_metadata"),
        FrameKind::DeleteMetadata { .. } => (29, "DELETE_METADATA", "get_metadata"),
        FrameKind::ListMetadata { .. } => (30, "LIST_METADATA", "list_metadata"),
        FrameKind::SubscribeMetadata { .. } => (31, "SUBSCRIBE_METADATA", "subscribe_metadata"),
        FrameKind::MetadataChanged { .. } => (32, "METADATA_CHANGED", "metadata_changed"),
        FrameKind::MetadataValue { .. } => (33, "METADATA_VALUE", "metadata_value"),
        FrameKind::MetadataKeys { .. } => (34, "METADATA_KEYS", "metadata_keys"),
        FrameKind::ListDirectory { .. } => (35, "LIST_DIRECTORY", "list_directory"),
        FrameKind::DirectoryListing { .. } => (36, "DIRECTORY_LISTING", "directory_listing"),
        FrameKind::PathQuery { .. } => (37, "PATH_QUERY", "path_query"),
        FrameKind::PathResults { .. } => (38, "PATH_RESULTS", "path_results"),
        FrameKind::SpawnResource { .. } => (39, "SPAWN_RESOURCE", "spawn_terminal"),
        FrameKind::ResourceSpawned { .. } => (40, "RESOURCE_SPAWNED", "terminal_spawned"),
        FrameKind::MoveResource { .. } => (41, "MOVE_RESOURCE", "move_terminal"),
        FrameKind::ResourceMoved { .. } => (42, "RESOURCE_MOVED", "terminal_moved"),
        FrameKind::ResourceClosed { .. } => (43, "RESOURCE_CLOSED", "terminal_closed"),
        FrameKind::ResizeTerminal { .. } => (44, "RESIZE_TERMINAL", "terminal_resize"),
        FrameKind::Command { .. } => (45, "COMMAND", "command"),
        FrameKind::CommandResult { .. } => (46, "COMMAND_RESULT", "command_result"),
        FrameKind::SubscribeEvents { .. } => (47, "SUBSCRIBE_EVENTS", "subscribe_events"),
        FrameKind::Event { .. } => (48, "EVENT", "event"),
    }
}

/// `FRAME_COMPRESSED` is no `FrameKind` variant: the envelope is sampled by
/// compressing one, so it is named here.
const FRAME_COMPRESSED: (&str, &str) = ("FRAME_COMPRESSED", "frame_compressed");

// ---------------------------------------------------------------------------
// Samples
// ---------------------------------------------------------------------------

fn terminal() -> ResourceId {
    ResourceId::local(7)
}

fn stream() -> StreamId {
    StreamId::new(3).unwrap()
}

fn generation() -> BootstrapId {
    BootstrapId::new(5).unwrap()
}

fn key() -> IdempotencyKey {
    IdempotencyKey::new([0x5A; 16]).unwrap()
}

fn actor() -> ActorRef {
    ActorRef::new(ClientId::new(7))
        .with_credential_id(Some("cred".to_owned()))
        .with_client_name(Some("cli".to_owned()))
}

/// Every [`FrameKind`] variant at least once; every optional field both
/// present and absent where the encoder can omit it.
fn frame_samples() -> Vec<FrameKind> {
    let mut samples = connection_samples();
    samples.extend(input_samples());
    samples.extend(stream_samples());
    samples.extend(metadata_samples());
    samples.extend(host_samples());
    samples.extend(spawn_samples());
    samples.extend(lifecycle_samples());
    samples
}

fn hello(client_caps: ClientCapabilities) -> FrameKind {
    FrameKind::Hello {
        client_name: "schema".to_owned(),
        protocol_major: 0,
        protocol_minor: 9,
        protocol_patch: 0,
        client_caps,
    }
}

fn hello_ok(server_caps: ServerCapabilities) -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: 0,
        protocol_minor: 9,
        protocol_patch: 0,
        server_caps,
        server_id: b"server".to_vec(),
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    }
}

fn attach(role_policy: Option<RolePolicy>) -> FrameKind {
    FrameKind::Attach {
        attach_id: 1,
        target: AttachTarget::ByName("work".to_owned()),
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: true,
        scrollback_limit_lines: 100,
        role_policy,
    }
}

fn connection_samples() -> Vec<FrameKind> {
    let origin = SshOrigin {
        client: "192.0.2.1:50000".parse().unwrap(),
        server: Some("192.0.2.2:22".parse().unwrap()),
    };
    vec![
        hello(ClientCapabilities::new()),
        hello(
            ClientCapabilities::new()
                .with_compression(CompressionSet::from_bits(CompressionSet::DEFLATE))
                .with_ssh_origin(origin)
                .with_quic_streams(true),
        ),
        hello_ok(ServerCapabilities::new()),
        hello_ok(ServerCapabilities::new().with_compression(Compression::Deflate)),
        FrameKind::Ping { nonce: 1 },
        FrameKind::Pong { nonce: 1 },
        attach(None),
        attach(Some(RolePolicy::PRIMARY)),
        FrameKind::Detach,
        FrameKind::Attached {
            attach_id: 1,
            snapshot: SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal()),
            initial_client_id: ClientId::new(7),
        },
        FrameKind::AttachReady { attach_id: 1 },
        FrameKind::Detached {
            reason: None,
            message: String::new(),
        },
        FrameKind::Detached {
            reason: Some(DetachReason::ServerShutdown),
            message: "stopping".to_owned(),
        },
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::InternalError,
            message: String::new(),
        },
        FrameKind::Error {
            request_id: Some(9),
            code: ErrorCode::InvalidCommand,
            message: "bad".to_owned(),
        },
    ]
}

fn input_samples() -> Vec<FrameKind> {
    vec![
        FrameKind::InputKey {
            terminal_id: terminal(),
            event: KeyEvent {
                action: KeyAction::Press,
                key: PhysicalKey::A,
                mods: ModSet::empty(),
                consumed_mods: ModSet::empty(),
                composing: false,
                text: Some("a".to_owned()),
                unshifted_codepoint: Some(u32::from('a')),
            },
        },
        FrameKind::InputMouse {
            terminal_id: terminal(),
            event: MouseEvent {
                action: MouseAction::Press,
                button: MouseButton::Left,
                mods: ModSet::empty(),
                x: 1.0,
                y: 2.0,
            },
        },
        FrameKind::InputFocus {
            terminal_id: terminal(),
            event: FocusEvent::Gained,
        },
        FrameKind::InputPaste {
            terminal_id: terminal(),
            event: PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"paste".to_vec(),
            },
        },
        FrameKind::InputTerminalReply {
            terminal_id: terminal(),
            bytes: Bytes::from_static(b"\x1b[0n"),
        },
        FrameKind::ViewportResize {
            viewport: ViewportInfo::new(80, 24),
        },
        FrameKind::ResizeTerminal {
            terminal_id: terminal(),
            cols: 80,
            rows: 24,
            cell_px: None,
        },
        FrameKind::ResizeTerminal {
            terminal_id: terminal(),
            cols: 80,
            rows: 24,
            cell_px: Some((8, 16)),
        },
    ]
}

#[allow(
    clippy::too_many_lines,
    reason = "one sample per bootstrap/history frame shape, kept as one flat table"
)]
fn stream_samples() -> Vec<FrameKind> {
    let cursor = Bytes::from_static(b"cursor");
    vec![
        FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            seq: 1,
            bytes: Bytes::from_static(b"hi"),
        },
        FrameKind::FrameAck {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            seq: 1,
        },
        FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            chunk_seq: 0,
            payload: Bytes::from_static(b"chunk"),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            history_cursor: None,
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            history_cursor: Some(cursor.clone()),
        },
        FrameKind::HistoryRequest {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            cursor: cursor.clone(),
            max_bytes: 1024,
            max_rows: 10,
        },
        history_page(None),
        history_page(Some(Bytes::from_static(b"older"))),
        FrameKind::BootstrapTombstone {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            reason: TombstoneReason::Resize,
            last_valid_seq: 4,
        },
        FrameKind::HistoryTombstone {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            cursor: cursor.clone(),
            reason: HistoryTombstoneReason::Pruned,
        },
        FrameKind::HistoryRejected {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            cursor,
            reason: HistoryRejectionReason::TooSmall,
            required_bytes: 2048,
            required_rows: 20,
        },
    ]
}

fn history_page(next_cursor: Option<Bytes>) -> FrameKind {
    FrameKind::HistoryPage {
        terminal_id: terminal(),
        stream_id: stream(),
        bootstrap_id: generation(),
        page_seq: 1,
        cursor: Bytes::from_static(b"cursor"),
        next_cursor,
        payload: Bytes::from_static(b"rows"),
        rows: 2,
    }
}

fn metadata_samples() -> Vec<FrameKind> {
    let key_name = || "phux.example/v1".to_owned();
    vec![
        FrameKind::GetMetadata {
            request_id: 1,
            scope: Scope::Global,
            key: key_name(),
        },
        FrameKind::SetMetadata {
            request_id: 1,
            scope: Scope::Resource(terminal()),
            key: key_name(),
            value: b"{}".to_vec(),
        },
        FrameKind::DeleteMetadata {
            request_id: 1,
            scope: Scope::Group(GroupId::new(1)),
            key: key_name(),
        },
        FrameKind::ListMetadata {
            request_id: 1,
            scope: Scope::Global,
        },
        FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: key_name(),
        },
        FrameKind::MetadataChanged {
            scope: Scope::Global,
            key: key_name(),
            value: None,
            actor: None,
        },
        FrameKind::MetadataChanged {
            scope: Scope::Global,
            key: key_name(),
            value: Some(b"{}".to_vec()),
            actor: Some(actor()),
        },
        FrameKind::MetadataValue {
            request_id: 1,
            value: None,
        },
        FrameKind::MetadataValue {
            request_id: 1,
            value: Some(b"{}".to_vec()),
        },
        FrameKind::MetadataKeys {
            request_id: 1,
            keys: vec![key_name()],
        },
    ]
}

#[allow(
    clippy::too_many_lines,
    reason = "success and refusal shapes of the host-path frames, kept as one flat table"
)]
fn host_samples() -> Vec<FrameKind> {
    let edge = || Some(SatelliteHost::new("edge"));
    vec![
        FrameKind::ListDirectory {
            request_id: 1,
            path: String::new(),
            host: None,
        },
        FrameKind::ListDirectory {
            request_id: 1,
            path: "/home".to_owned(),
            host: edge(),
        },
        FrameKind::DirectoryListing {
            request_id: 1,
            result: Ok(DirectoryListing {
                path: "/".to_owned(),
                parent: None,
                entries: Vec::new(),
                truncated: false,
            }),
        },
        FrameKind::DirectoryListing {
            request_id: 1,
            result: Ok(DirectoryListing {
                path: "/home/u".to_owned(),
                parent: Some("/home".to_owned()),
                entries: vec![DirectoryEntry {
                    name: "src".to_owned(),
                    is_symlink: true,
                }],
                truncated: true,
            }),
        },
        FrameKind::DirectoryListing {
            request_id: 1,
            result: Err(DirectoryListingError {
                path: "/root".to_owned(),
                code: DirectoryErrorCode::PermissionDenied,
                message: "denied".to_owned(),
            }),
        },
        FrameKind::PathQuery {
            request_id: 1,
            root: "/".to_owned(),
            query: String::new(),
            recursive: false,
            host: None,
        },
        FrameKind::PathQuery {
            request_id: 1,
            root: "/home".to_owned(),
            query: "src".to_owned(),
            recursive: true,
            host: edge(),
        },
        FrameKind::PathResults {
            request_id: 1,
            result: Ok(crate::wire::frame::PathResults {
                root: "/".to_owned(),
                parent: None,
                rows: Vec::new(),
                status: PathStatus::Complete,
            }),
        },
        FrameKind::PathResults {
            request_id: 1,
            result: Ok(crate::wire::frame::PathResults {
                root: "/home".to_owned(),
                parent: Some("/".to_owned()),
                rows: vec![PathRow {
                    path: "/home/u".to_owned(),
                    kind: PathKind::Directory,
                }],
                status: PathStatus::Truncated,
            }),
        },
        FrameKind::PathResults {
            request_id: 1,
            result: Err(PathQueryError {
                root: "/nope".to_owned(),
                code: PathErrorCode::NotFound,
                message: "missing".to_owned(),
            }),
        },
    ]
}

fn spawn(resource: Option<SpawnResource>) -> FrameKind {
    FrameKind::SpawnResource {
        request_id: 1,
        group: GroupId::new(1),
        command: None,
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session: None,
        initial_size: None,
        resource: resource.map(Box::new),
    }
}

fn spawn_samples() -> Vec<FrameKind> {
    let full_terminal = FrameKind::SpawnResource {
        request_id: 2,
        group: GroupId::new(1),
        command: Some(vec!["zsh".to_owned(), "-i".to_owned()]),
        cwd: Some("/home/u".to_owned()),
        env: Some(vec![("LANG".to_owned(), "C".to_owned())]),
        term: Some("ghostty".to_owned()),
        satellite: Some(SatelliteHost::new("edge")),
        owner_terminal: Some(ResourceId::local(8)),
        agent_session: Some(b"{}".to_vec()),
        initial_size: Some((132, 43)),
        resource: Some(Box::new(
            SpawnResource::default()
                .with_bind_instance(true)
                .with_retain_secs(Some(600))
                .with_idempotency_key(Some(key())),
        )),
    };
    let agent_session = SpawnResource::agent_session(terminal(), "claude")
        .with_native_id(Some("session-42".to_owned()));
    let instance = ServerInstance::new([0xA5; 16]);
    vec![
        spawn(None),
        full_terminal,
        spawn(Some(agent_session)),
        FrameKind::ResourceSpawned {
            request_id: 1,
            result: SpawnResult::Ok(terminal()),
        },
        FrameKind::ResourceSpawned {
            request_id: 1,
            result: SpawnResult::OkBound {
                id: terminal(),
                instance,
            },
        },
        FrameKind::ResourceSpawned {
            request_id: 1,
            result: SpawnResult::Replayed {
                id: terminal(),
                instance: Some(instance),
            },
        },
        FrameKind::MoveResource {
            request_id: 1,
            terminal: terminal(),
            owner_terminal: ResourceId::local(8),
        },
        FrameKind::ResourceMoved {
            request_id: 1,
            result: MoveResult::Ok(terminal()),
        },
    ]
}

#[allow(
    clippy::too_many_lines,
    reason = "every optional-field shape of the lifecycle frames, kept as one flat table"
)]
fn lifecycle_samples() -> Vec<FrameKind> {
    let stamp = EventStamp::new(0x0102, 0x0189)
        .with_actor(Some(actor()))
        .with_operation_id(Some(key()));
    vec![
        FrameKind::Bell {
            terminal_id: terminal(),
        },
        FrameKind::ResourceClosed {
            terminal_id: terminal(),
            exit_status: None,
            reason: CloseReason::Unknown,
            signal: None,
        },
        FrameKind::ResourceClosed {
            terminal_id: terminal(),
            exit_status: Some(-9),
            reason: CloseReason::Killed,
            signal: Some(9),
        },
        FrameKind::Command {
            request_id: 1,
            command: Command::Upgrade,
        },
        FrameKind::CommandResult {
            request_id: 1,
            result: CommandResult::Ok,
        },
        FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: None,
        },
        FrameKind::SubscribeEvents {
            terminal: Some(terminal()),
            after_seq: Some(41),
        },
        FrameKind::Event {
            terminal: None,
            event: AgentEvent::SourceGap { dropped: 3 },
            stamp: None,
        },
        FrameKind::Event {
            terminal: Some(terminal()),
            event: AgentEvent::SourceGap { dropped: 3 },
            stamp: Some(Box::new(stamp)),
        },
    ]
}

/// One sampled frame on the wire, with the spec name and field module its
/// type byte belongs to.
struct Sample {
    name: &'static str,
    module: &'static str,
    bytes: Vec<u8>,
}

fn encoded(frame: &FrameKind) -> Vec<u8> {
    let mut out = BytesMut::new();
    frame.encode(&mut out);
    out.to_vec()
}

/// Every sample on the wire, `FRAME_COMPRESSED` included.
fn samples() -> Vec<Sample> {
    let mut out: Vec<Sample> = frame_samples()
        .iter()
        .map(|frame| {
            let (_, name, module) = frame_entry(frame);
            Sample {
                name,
                module,
                bytes: encoded(frame),
            }
        })
        .collect();
    // Repetitive enough that deflate shrinks it, so the envelope is emitted.
    let inner = FrameKind::ResourceOutput {
        terminal_id: terminal(),
        stream_id: stream(),
        bootstrap_id: generation(),
        seq: 1,
        bytes: Bytes::from(vec![b'x'; 4096]),
    };
    let (mut scratch, mut bytes) = (BytesMut::new(), BytesMut::new());
    inner.encode_compressed(Compression::Deflate, &mut scratch, &mut bytes);
    out.push(Sample {
        name: FRAME_COMPRESSED.0,
        module: FRAME_COMPRESSED.1,
        bytes: bytes.to_vec(),
    });
    // Presence is inferred from what decodes, so a sample the decoder
    // rejects would be evidence of nothing.
    for sample in &out {
        assert!(
            decodes(&sample.bytes),
            "{} sample does not decode",
            sample.name
        );
    }
    out
}

// ---------------------------------------------------------------------------
// TLV body walk
// ---------------------------------------------------------------------------

/// One body-level field: its id and where it sits in the frame bytes.
struct Tlv {
    id: u32,
    wire_type: u8,
    span: std::ops::Range<usize>,
    value: std::ops::Range<usize>,
}

/// Offset of the body after the length prefix and type byte.
const BODY_START: usize = 5;

fn type_byte(bytes: &[u8]) -> u8 {
    bytes[BODY_START - 1]
}

/// Walk a frame body as TLV, asserting it parses to the last byte.
fn fields(bytes: &[u8]) -> Vec<Tlv> {
    let body = &bytes[BODY_START..];
    let mut dec = Decoder::new(body);
    let mut out = Vec::new();
    while !dec.at_body_end() {
        let start = dec.position();
        let id = u32::try_from(dec.read_varint().unwrap()).unwrap();
        let wire_type = dec.read_u8().unwrap();
        let len = usize::try_from(dec.read_varint().unwrap()).unwrap();
        let value_start = dec.position();
        dec.take(len).unwrap();
        out.push(Tlv {
            id,
            wire_type,
            span: BODY_START + start..BODY_START + dec.position(),
            value: BODY_START + value_start..BODY_START + value_start + len,
        });
    }
    out
}

/// `bytes` with one field cut out and the length prefix rewritten.
fn without(bytes: &[u8], cut: &std::ops::Range<usize>) -> Vec<u8> {
    let mut out = bytes[..cut.start].to_vec();
    out.extend_from_slice(&bytes[cut.end..]);
    let len = u32::try_from(out.len() - 4).unwrap();
    out[..4].copy_from_slice(&len.to_be_bytes());
    out
}

fn decodes(bytes: &[u8]) -> bool {
    FrameKind::decode(bytes).is_ok()
}

// ---------------------------------------------------------------------------
// Derivation
// ---------------------------------------------------------------------------

/// `module -> [(CONST_NAME, id)]` parsed from `wire/field.rs`.
fn field_modules() -> BTreeMap<&'static str, Vec<(&'static str, u32)>> {
    let mut out: BTreeMap<&str, Vec<(&str, u32)>> = BTreeMap::new();
    let mut current = None;
    for line in FIELD_RS.lines() {
        if let Some(name) = line.strip_prefix("pub mod ") {
            let name = name.trim_end_matches(" {");
            out.entry(name).or_default();
            current = Some(name);
        } else if line == "}" {
            current = None;
        } else if let Some(decl) = line.trim().strip_prefix("pub const ") {
            let module = current.expect("field constants live inside a module");
            let (name, id) = decl
                .strip_suffix(';')
                .and_then(|decl| decl.split_once(": u32 = "))
                .unwrap_or_else(|| panic!("unparsed field constant: {line}"));
            out.entry(module)
                .or_default()
                .push((name, id.parse().unwrap()));
        }
    }
    out
}

/// What the samples of one frame show about one field.
struct Observed {
    present_in_all: bool,
    strip_always_fails: bool,
}

/// Presence of each field id the samples of one frame carry.
fn observe(samples: &[&Sample]) -> BTreeMap<u32, Observed> {
    let mut out: BTreeMap<u32, Observed> = BTreeMap::new();
    let per_sample: Vec<Vec<Tlv>> = samples.iter().map(|s| fields(&s.bytes)).collect();
    let ids: BTreeSet<u32> = per_sample.iter().flatten().map(|tlv| tlv.id).collect();
    for id in ids {
        let entry = out.entry(id).or_insert(Observed {
            present_in_all: true,
            strip_always_fails: true,
        });
        for (sample, tlvs) in samples.iter().zip(&per_sample) {
            let Some(tlv) = tlvs.iter().find(|tlv| tlv.id == id) else {
                entry.present_in_all = false;
                continue;
            };
            if decodes(&without(&sample.bytes, &tlv.span)) {
                entry.strip_always_fails = false;
            }
        }
    }
    out
}

impl Observed {
    const fn presence(&self) -> Presence {
        if self.present_in_all && self.strip_always_fails {
            Presence::Required
        } else {
            Presence::Optional
        }
    }
}

/// Ids in `1..=max` that no field holds: retired, never reused.
fn retired(ids: &[u32]) -> Vec<u32> {
    let max = ids.iter().copied().max().unwrap_or(0);
    (1..=max).filter(|id| !ids.contains(id)).collect()
}

/// The schema as the codec defines it, carrying `types` and `description`
/// and each field's declared `type` over from `current`.
fn derive(current: &Schema) -> Schema {
    let modules = field_modules();
    let samples = samples();
    let mut by_type: BTreeMap<u8, Vec<&Sample>> = BTreeMap::new();
    for sample in &samples {
        by_type
            .entry(type_byte(&sample.bytes))
            .or_default()
            .push(sample);
    }
    let frames = by_type
        .into_iter()
        .map(|(type_byte, group)| derive_frame(type_byte, &group, &modules, current))
        .collect();
    Schema {
        description: current.description.clone(),
        types: current.types.clone(),
        frames,
    }
}

fn derive_frame(
    type_byte: u8,
    samples: &[&Sample],
    modules: &BTreeMap<&str, Vec<(&str, u32)>>,
    current: &Schema,
) -> FrameEntry {
    let name = samples[0].name;
    let module = samples[0].module;
    let consts = if module.is_empty() {
        &[][..]
    } else {
        modules
            .get(module)
            .unwrap_or_else(|| panic!("{name}: no `{module}` module in field.rs"))
    };
    let observed = observe(samples);
    for id in observed.keys() {
        assert!(
            consts.iter().any(|(_, known)| known == id),
            "{name} emits field {id}, which field.rs `{module}` does not name"
        );
    }
    let declared = current.frames.iter().find(|frame| frame.name == name);
    let fields = consts
        .iter()
        .map(|&(field, id)| {
            let seen = observed.get(&id).unwrap_or_else(|| {
                panic!("{name}.{field} (id {id}) appears in no sample; add one that sets it")
            });
            let ty = declared
                .and_then(|frame| frame.fields.iter().find(|f| f.id == id))
                .map_or_else(|| UNSET_TYPE.to_owned(), |f| f.ty.clone());
            FieldEntry {
                id,
                name: field.to_owned(),
                ty,
                presence: seen.presence(),
            }
        })
        .collect::<Vec<_>>();
    let ids: Vec<u32> = fields.iter().map(|field| field.id).collect();
    FrameEntry {
        name: name.to_owned(),
        type_byte,
        retired: retired(&ids),
        fields,
    }
}

// ---------------------------------------------------------------------------
// Value types
// ---------------------------------------------------------------------------

/// Whether `value` decodes as `ty` and nothing is left over; `None` for a
/// type this checker does not know.
fn type_matches(ty: &str, value: &[u8]) -> Option<bool> {
    let fixed = |width: usize| value.len() == width;
    Some(match ty {
        "u8" => fixed(1),
        "bool" => value == [0] || value == [1],
        "u16" => fixed(2),
        "u32" | "i32" | "GridSize" => fixed(4),
        "u64" => fixed(8),
        "bytes16" => fixed(16),
        "str" => std::str::from_utf8(value).is_ok(),
        "bytes" => true,
        "SshOrigin" => decode_ssh_origin(value).is_some(),
        "DirectoryEntryList" | "PathRowList" => whole(value, str_u8_list),
        _ => return positional_matches(ty, value),
    })
}

/// Named positional records, each checked by the codec's own decoder.
fn positional_matches(ty: &str, value: &[u8]) -> Option<bool> {
    Some(match ty {
        "ResourceId" => whole(value, decode_terminal_id),
        "StringList" => whole(value, decode_string_list),
        "Env" => whole(value, decode_env),
        "Scope" => whole(value, decode_scope),
        "ActorRef" => whole(value, decode_actor_ref),
        "ViewportInfo" => whole(value, decode_viewport_info),
        "AttachTarget" => whole(value, decode_attach_target),
        "KeyEvent" => whole(value, decode_key_event),
        "MouseEvent" => whole(value, decode_mouse_event),
        "PasteEvent" => whole(value, decode_paste_event),
        "ClientCapabilities" => whole(value, decode_client_capabilities),
        "ServerCapabilities" => whole(value, decode_server_capabilities),
        "BootstrapProfile" => whole(value, decode_bootstrap_profile),
        "BootstrapCodec" => whole(value, decode_bootstrap_codec),
        "SessionSnapshot" => whole(value, decode_session_snapshot),
        "SpawnResult" => whole(value, decode_spawn_result),
        "MoveResult" => whole(value, decode_move_result),
        "Command" => whole(value, decode_command),
        "CommandResult" => whole(value, decode_command_result),
        "AgentEvent" => whole(value, decode_agent_event),
        _ => return None,
    })
}

/// Every type name [`type_matches`] knows.
const KNOWN_TYPES: &[&str] = &[
    "u8",
    "bool",
    "u16",
    "u32",
    "i32",
    "GridSize",
    "u64",
    "bytes16",
    "str",
    "bytes",
    "SshOrigin",
    "DirectoryEntryList",
    "PathRowList",
    "ResourceId",
    "StringList",
    "Env",
    "Scope",
    "ActorRef",
    "ViewportInfo",
    "AttachTarget",
    "KeyEvent",
    "MouseEvent",
    "PasteEvent",
    "ClientCapabilities",
    "ServerCapabilities",
    "BootstrapProfile",
    "BootstrapCodec",
    "SessionSnapshot",
    "SpawnResult",
    "MoveResult",
    "Command",
    "CommandResult",
    "AgentEvent",
];

fn whole<T>(
    value: &[u8],
    decode: impl FnOnce(&mut Decoder<'_>) -> Result<T, crate::wire::DecodeError>,
) -> bool {
    let mut dec = Decoder::new(value);
    decode(&mut dec).is_ok() && dec.remaining().is_empty()
}

/// `u32` count, then that many `(str, u8)` rows: directory entries and path
/// rows share this shape.
fn str_u8_list(dec: &mut Decoder<'_>) -> Result<(), crate::wire::DecodeError> {
    for _ in 0..dec.read_u32_be()? {
        dec.read_str()?;
        dec.read_u8()?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

fn schema_path() -> PathBuf {
    // Read at run time, not `env!`: a baked checkout path defeats the shared
    // build cache (scripts/check-cache-portable.sh).
    PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("the test runner sets CARGO_MANIFEST_DIR"),
    )
    .join("../..")
    .join(SCHEMA_PATH)
}

fn committed() -> (String, Schema) {
    let text = std::fs::read_to_string(schema_path())
        .unwrap_or_else(|err| panic!("cannot read {SCHEMA_PATH}: {err}"));
    let schema = serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("{SCHEMA_PATH} is not a valid schema: {err}"));
    (text, schema)
}

/// The first line where `a` and `b` differ, for a readable failure.
fn first_difference(a: &str, b: &str) -> String {
    a.lines()
        .zip(b.lines())
        .enumerate()
        .find(|(_, (x, y))| x != y)
        .map_or_else(
            || "the files differ in length".to_owned(),
            |(n, (x, y))| format!("line {}:\n  committed: {x}\n  codec:     {y}", n + 1),
        )
}

#[test]
fn wire_schema_matches_the_codec() {
    let (text, current) = committed();
    let rendered = render(&derive(&current));
    if std::env::var_os(UPDATE_ENV).is_some() {
        std::fs::write(schema_path(), &rendered).unwrap();
        return;
    }
    assert!(
        text == rendered,
        "{SCHEMA_PATH} disagrees with the codec at {}\n\
         If the codec change is intended, regenerate with \
         `{UPDATE_ENV}=1 cargo nextest run -p phux-protocol wire_schema_matches`, set the `type` of \
         any new field, update the spec prose, and review the diff.",
        first_difference(&text, &rendered)
    );
}

#[test]
fn every_variant_has_a_sample() {
    let covered: BTreeSet<usize> = frame_samples()
        .iter()
        .map(|frame| frame_entry(frame).0)
        .collect();
    assert_eq!(
        covered,
        (0..FRAME_VARIANTS).collect(),
        "every FrameKind variant needs a sample in wire_schema.rs"
    );
}

#[test]
fn samples_carry_only_length_delimited_unrepeated_fields() {
    for sample in samples() {
        let tlvs = fields(&sample.bytes);
        for tlv in &tlvs {
            assert_eq!(
                tlv.wire_type,
                wire_type::BYTES,
                "{}.{}",
                sample.name,
                tlv.id
            );
        }
        let ids: BTreeSet<u32> = tlvs.iter().map(|tlv| tlv.id).collect();
        assert_eq!(ids.len(), tlvs.len(), "{} repeats a field", sample.name);
    }
    let compressed = samples()
        .into_iter()
        .any(|sample| type_byte(&sample.bytes) == TYPE_FRAME_COMPRESSED);
    assert!(compressed, "the FRAME_COMPRESSED sample was not compressed");
}

#[test]
fn declared_types_match_every_sampled_value() {
    let (_, schema) = committed();
    for sample in samples() {
        let frame = schema
            .frames
            .iter()
            .find(|frame| frame.name == sample.name)
            .unwrap_or_else(|| panic!("{} missing from {SCHEMA_PATH}", sample.name));
        for tlv in fields(&sample.bytes) {
            let field = frame.fields.iter().find(|f| f.id == tlv.id).unwrap();
            let value = &sample.bytes[tlv.value.clone()];
            let matches = type_matches(&field.ty, value).unwrap_or_else(|| {
                panic!("{}.{}: unknown type `{}`", frame.name, field.name, field.ty)
            });
            assert!(
                matches,
                "{}.{} is declared `{}` but carries {value:02x?}",
                frame.name, field.name, field.ty
            );
        }
    }
}

#[test]
fn declared_types_are_exactly_the_checked_ones() {
    let (_, schema) = committed();
    let known: BTreeSet<&str> = KNOWN_TYPES.iter().copied().collect();
    let documented: BTreeSet<&str> = schema.types.keys().map(String::as_str).collect();
    let used: BTreeSet<&str> = schema
        .frames
        .iter()
        .flat_map(|frame| &frame.fields)
        .map(|field| field.ty.as_str())
        .collect();
    assert_eq!(
        documented, known,
        "`types` must document exactly the checked types"
    );
    assert_eq!(
        used, known,
        "every checked type must be used, and nothing else"
    );
    for name in known {
        assert!(type_matches(name, &[]).is_some(), "{name} has no checker");
    }
}

#[test]
fn every_field_module_is_a_frame_or_a_nested_record() {
    let mut frames: BTreeSet<&str> = frame_samples()
        .iter()
        .map(|frame| frame_entry(frame).2)
        .filter(|module| !module.is_empty())
        .collect();
    frames.insert(FRAME_COMPRESSED.1);
    let nested: BTreeSet<&str> = NESTED_RECORD_MODULES.into_iter().collect();
    let modules: BTreeSet<&str> = field_modules().into_keys().collect();
    assert!(frames.is_disjoint(&nested));
    assert_eq!(
        modules,
        frames.union(&nested).copied().collect(),
        "a field.rs module is neither a sampled frame nor a listed nested record"
    );
}
