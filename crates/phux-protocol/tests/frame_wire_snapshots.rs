//! Snapshot tests for stable protocol frames outside the protocol-0.7
//! bootstrap surface. Bootstrap and handshake codecs use focused semantic
//! round-trip/malformed tests because their negotiated fields are better
//! defended as values than as duplicated hex fixtures.
//!
//! One table-driven test encodes each stable fixture and compares its hex dump
//! with the named golden under `tests/snapshots/`. Fixture names are
//! load-bearing: renaming one or changing its bytes must surface in review.

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::{
    BootstrapId, ClientId, GroupId, ResourceId, ResourceKind, SatelliteHost, SessionId, StreamId,
    WindowId,
};
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    AgentEvent, CloseReason, Command, CommandResult, CommandValue, DetachReason, ErrorCode,
    FrameKind, MoveError, MoveResult, Scope, SpawnError, SpawnResource, SpawnResult, ViewportInfo,
};
use phux_protocol::wire::info::{
    AgentFacet, HostInventory, HostSessionInfo, ResourceInfo, SessionInfo, SessionSnapshot,
};

/// Render `bytes` as an `xxd`-style hex dump: 16 cols per row,
/// `OFFSET | HEX HEX HEX ... | ASCII`.
fn hex_dump(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    if bytes.is_empty() {
        out.push_str("(empty)\n");
        return out;
    }
    for (chunk_idx, chunk) in bytes.chunks(16).enumerate() {
        let offset = chunk_idx * 16;
        let _ = write!(out, "{offset:08x} |");
        for (i, b) in chunk.iter().enumerate() {
            if i == 8 {
                out.push(' ');
            }
            let _ = write!(out, " {b:02x}");
        }
        let pad_cells = 16 - chunk.len();
        for i in 0..pad_cells {
            if chunk.len() + i == 8 {
                out.push(' ');
            }
            out.push_str("   ");
        }
        out.push_str(" |");
        for b in chunk {
            let c = if (0x20..=0x7e).contains(b) {
                *b as char
            } else {
                '.'
            };
            out.push(c);
        }
        out.push('\n');
    }
    out
}

fn dump_frame(frame: &FrameKind) -> String {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    hex_dump(&buf)
}

/// Stable frame fixtures in protocol order. Profile-bound output, attach
/// bootstrap, and handshake frames are covered by semantic wire tests.
#[allow(clippy::too_many_lines)]
fn frame_fixtures() -> Vec<(&'static str, FrameKind)> {
    vec![
        // DETACH is a unit message; DETACHED carries two optional-absent
        // fields, so the reason-less shape must stay byte-identical to the
        // empty body every 0.7.0 peer already emits.
        ("snap_detach", FrameKind::Detach),
        (
            "snap_detached",
            FrameKind::Detached {
                reason: None,
                message: String::new(),
            },
        ),
        (
            "snap_detached_reason",
            FrameKind::Detached {
                reason: Some(DetachReason::ServerShutdown),
                message: "server is stopping".to_owned(),
            },
        ),
        // INPUT_*
        (
            "snap_input_key_letter_a_press",
            FrameKind::InputKey {
                terminal_id: ResourceId::local(0x0000_0007),
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
        ),
        (
            "snap_input_key_no_text",
            FrameKind::InputKey {
                terminal_id: ResourceId::local(0x0000_0001),
                event: KeyEvent {
                    action: KeyAction::Release,
                    key: PhysicalKey::Escape,
                    mods: ModSet::CTRL | ModSet::SHIFT,
                    consumed_mods: ModSet::empty(),
                    composing: false,
                    text: None,
                    unshifted_codepoint: None,
                },
            },
        ),
        (
            "snap_input_mouse_left_click",
            FrameKind::InputMouse {
                terminal_id: ResourceId::local(0x0000_0042),
                event: MouseEvent {
                    action: MouseAction::Press,
                    button: MouseButton::Left,
                    mods: ModSet::empty(),
                    x: 120.0,
                    y: 40.5,
                },
            },
        ),
        (
            "snap_input_focus_gained",
            FrameKind::InputFocus {
                terminal_id: ResourceId::local(0x0000_0003),
                event: FocusEvent::Gained,
            },
        ),
        (
            "snap_input_focus_lost",
            FrameKind::InputFocus {
                terminal_id: ResourceId::local(0x0000_0003),
                event: FocusEvent::Lost,
            },
        ),
        (
            "snap_input_paste_trusted_ascii",
            FrameKind::InputPaste {
                terminal_id: ResourceId::local(0x0000_0005),
                event: PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: b"hello world".to_vec(),
                },
            },
        ),
        (
            "snap_bell",
            FrameKind::Bell {
                terminal_id: ResourceId::local(0x0000_00BE),
            },
        ),
        // VIEWPORT_RESIZE — cell-only and pixel-augmented viewports.
        (
            "snap_viewport_resize_cells_only",
            FrameKind::ViewportResize {
                viewport: ViewportInfo::new(120, 40),
            },
        ),
        (
            "snap_viewport_resize_with_pixels",
            FrameKind::ViewportResize {
                viewport: ViewportInfo::new(120, 40).with_pixels(Some(1920), Some(1080)),
            },
        ),
        // ERROR — server-emitted structured error frames; sibling refusal
        // paths share the wire shape. The internal fixture exercises the
        // u16::MAX (=65535) wire value to lock in the high end of the
        // ErrorCode encoding alongside SPEC §14's `INTERNAL_ERROR = 65535`.
        (
            "snap_error_session_not_found",
            FrameKind::Error {
                request_id: None,
                code: ErrorCode::SessionNotFound,
                message: "no such session: 'work'".to_owned(),
            },
        ),
        (
            "snap_error_with_request_id_invalid_command",
            FrameKind::Error {
                request_id: Some(0x0000_002A),
                code: ErrorCode::InvalidCommand,
                message: "missing field: terminal_id".to_owned(),
            },
        ),
        (
            "snap_error_internal_max_code",
            FrameKind::Error {
                request_id: None,
                code: ErrorCode::InternalError,
                message: String::new(),
            },
        ),
        // L3 metadata frames.
        (
            "snap_get_metadata_global",
            FrameKind::GetMetadata {
                request_id: 0x0000_0001,
                scope: Scope::Global,
                key: "phux.example/v1".to_owned(),
            },
        ),
        (
            "snap_get_metadata_group",
            FrameKind::GetMetadata {
                request_id: 0x0000_0007,
                scope: Scope::Group(GroupId::new(1)),
                key: "phux.tui.layout/v1".to_owned(),
            },
        ),
        (
            "snap_get_metadata_terminal",
            FrameKind::GetMetadata {
                request_id: 0x0000_0042,
                scope: Scope::Resource(ResourceId::local(0x0000_0009)),
                key: "phux.tui.title-override/v1".to_owned(),
            },
        ),
        (
            "snap_set_metadata_group_layout",
            FrameKind::SetMetadata {
                request_id: 0x0000_0010,
                scope: Scope::Group(GroupId::new(1)),
                key: "phux.tui.layout/v1".to_owned(),
                value: b"\xa2\x01\x01\x02\x82\x00\x01".to_vec(), // arbitrary CBOR-looking bytes
            },
        ),
        (
            "snap_delete_metadata_global",
            FrameKind::DeleteMetadata {
                request_id: 0x0000_0011,
                scope: Scope::Global,
                key: "phux.example/v1".to_owned(),
            },
        ),
        (
            "snap_list_metadata_group",
            FrameKind::ListMetadata {
                request_id: 0x0000_0012,
                scope: Scope::Group(GroupId::new(1)),
            },
        ),
        (
            "snap_subscribe_metadata_group_layout",
            FrameKind::SubscribeMetadata {
                scope: Scope::Group(GroupId::new(1)),
                key: "phux.tui.layout/v1".to_owned(),
            },
        ),
        (
            "snap_metadata_changed_set_group",
            FrameKind::MetadataChanged {
                scope: Scope::Group(GroupId::new(1)),
                key: "phux.tui.layout/v1".to_owned(),
                value: Some(b"\xa2\x01\x01\x02\x82\x00\x01".to_vec()),
            },
        ),
        (
            "snap_metadata_changed_tombstone",
            FrameKind::MetadataChanged {
                scope: Scope::Global,
                key: "phux.example/v1".to_owned(),
                value: None,
            },
        ),
        // L3 metadata reply frames.
        (
            "snap_metadata_value_present",
            FrameKind::MetadataValue {
                request_id: 0x0000_0007,
                value: Some(b"\xa2\x01\x01\x02\x82\x00\x01".to_vec()),
            },
        ),
        (
            "snap_metadata_value_absent",
            FrameKind::MetadataValue {
                request_id: 0x0000_0042,
                value: None,
            },
        ),
        (
            "snap_metadata_keys_empty",
            FrameKind::MetadataKeys {
                request_id: 0x0000_0012,
                keys: Vec::new(),
            },
        ),
        (
            "snap_metadata_keys_populated",
            FrameKind::MetadataKeys {
                request_id: 0x0000_0012,
                keys: vec![
                    "phux.tui.layout/v1".to_owned(),
                    "phux.tui.window_order/v1".to_owned(),
                ],
            },
        ),
        // L3 host query: LIST_DIRECTORY and its DIRECTORY_LISTING reply.
        (
            "snap_list_directory_home",
            FrameKind::ListDirectory {
                request_id: 0x0000_0021,
                path: String::new(),
            },
        ),
        (
            "snap_directory_listing_ok",
            FrameKind::DirectoryListing {
                request_id: 0x0000_0021,
                result: Ok(phux_protocol::wire::frame::DirectoryListing {
                    path: "/home/u".to_owned(),
                    parent: Some("/home".to_owned()),
                    entries: vec![
                        phux_protocol::wire::frame::DirectoryEntry {
                            name: "src".to_owned(),
                            is_symlink: false,
                        },
                        phux_protocol::wire::frame::DirectoryEntry {
                            name: "www".to_owned(),
                            is_symlink: true,
                        },
                    ],
                    truncated: true,
                }),
            },
        ),
        (
            "snap_directory_listing_denied",
            FrameKind::DirectoryListing {
                request_id: 0x0000_0022,
                result: Err(phux_protocol::wire::frame::DirectoryListingError {
                    path: "/root".to_owned(),
                    code: phux_protocol::wire::frame::DirectoryErrorCode::PermissionDenied,
                    message: "denied".to_owned(),
                }),
            },
        ),
        // L1 Terminal lifecycle frames.
        (
            // The minimum SPAWN_RESOURCE: request_id, default group, every
            // optional field absent. Reads as "spawn the server's default
            // shell in its default cwd, inheriting its env."
            "snap_spawn_terminal_minimal",
            FrameKind::SpawnResource {
                request_id: 0x0000_0001,
                group: GroupId::new(1),
                command: None,
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
                initial_size: None,
                resource: None,
            },
        ),
        (
            // All optional fields populated; exercises the env-pair encoding
            // and length-prefixed command list.
            "snap_spawn_terminal_full",
            FrameKind::SpawnResource {
                request_id: 0x0000_0002,
                group: GroupId::new(1),
                command: Some(vec!["zsh".to_owned(), "-i".to_owned()]),
                cwd: Some("/home/u/src".to_owned()),
                env: Some(vec![
                    ("TERM".to_owned(), "xterm-256color".to_owned()),
                    ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
                ]),
                term: None,
                satellite: None,
                owner_terminal: Some(ResourceId::local(42)),
                agent_session: Some(
                    br#"{"plugin_id":"com.phux.agents","native_id":"session-42"}"#.to_vec(),
                ),
                initial_size: Some((132, 43)),
                resource: None,
            },
        ),
        (
            // The first-class `term` field (phux-ign): field id 6, a bare
            // UTF-8 string. Distinct from the `TERM` env pair above — this is
            // the typed per-spawn override.
            "snap_spawn_terminal_term_field",
            FrameKind::SpawnResource {
                request_id: 0x0000_0003,
                group: GroupId::new(1),
                command: None,
                cwd: None,
                env: None,
                term: Some("ghostty".to_owned()),
                satellite: None,
                owner_terminal: None,
                agent_session: None,
                initial_size: None,
                resource: None,
            },
        ),
        (
            // An AgentSession spawn: fields 11 (kind), 12 (parent), 13
            // (provider), 14 (native_id) and none of the PTY-shape fields.
            "snap_spawn_terminal_agent_session",
            FrameKind::SpawnResource {
                request_id: 0x0000_0004,
                group: GroupId::new(1),
                command: None,
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
                initial_size: None,
                resource: Some(Box::new(
                    SpawnResource::agent_session(ResourceId::local(0x0000_002A), "claude")
                        .with_native_id(Some("session-42".to_owned())),
                )),
            },
        ),
        // pane_spawned events: a root Terminal keeps the empty body; an
        // AgentSession child carries its kind and parent as TLV fields.
        (
            "snap_event_pane_spawned_terminal",
            FrameKind::Event {
                terminal: Some(ResourceId::local(0x0000_002A)),
                event: AgentEvent::ResourceSpawned {
                    kind: ResourceKind::Terminal,
                    parent: None,
                },
            },
        ),
        (
            "snap_event_pane_spawned_agent_session",
            FrameKind::Event {
                terminal: Some(ResourceId::local(0x0000_002B)),
                event: AgentEvent::ResourceSpawned {
                    kind: ResourceKind::AgentSession,
                    parent: Some(ResourceId::local(0x0000_002A)),
                },
            },
        ),
        (
            "snap_terminal_spawned_err_unsupported_kind",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_000C,
                result: SpawnResult::Err(SpawnError::UnsupportedKind),
            },
        ),
        (
            "snap_terminal_spawned_err_parent_not_found",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_000D,
                result: SpawnResult::Err(SpawnError::ParentNotFound),
            },
        ),
        (
            "snap_terminal_spawned_err_parent_kind_mismatch",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_000E,
                result: SpawnResult::Err(SpawnError::ParentKindMismatch),
            },
        ),
        (
            "snap_terminal_spawned_ok",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_0001,
                result: SpawnResult::Ok(ResourceId::local(0x0000_002A)),
            },
        ),
        (
            "snap_terminal_spawned_err_group_not_found",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_0007,
                result: SpawnResult::Err(SpawnError::GroupNotFound),
            },
        ),
        (
            "snap_terminal_spawned_err_spawn_failed",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_0008,
                result: SpawnResult::Err(SpawnError::SpawnFailed("no pty available".to_owned())),
            },
        ),
        (
            "snap_move_terminal",
            FrameKind::MoveResource {
                request_id: 0x0000_0009,
                terminal: ResourceId::local(0x0000_002A),
                owner_terminal: ResourceId::local(0x0000_0007),
            },
        ),
        (
            "snap_terminal_moved_ok",
            FrameKind::ResourceMoved {
                request_id: 0x0000_0009,
                result: MoveResult::Ok(ResourceId::local(0x0000_002A)),
            },
        ),
        (
            "snap_terminal_moved_err_move_failed",
            FrameKind::ResourceMoved {
                request_id: 0x0000_000A,
                result: MoveResult::Err(MoveError::MoveFailed("no such terminal".to_owned())),
            },
        ),
        (
            "snap_terminal_moved_err_unsupported_satellite_route",
            FrameKind::ResourceMoved {
                request_id: 0x0000_000B,
                result: MoveResult::Err(MoveError::UnsupportedSatelliteRoute),
            },
        ),
        (
            "snap_terminal_closed_with_exit_code",
            FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(0x0000_002A),
                exit_status: Some(0),
                reason: CloseReason::Unknown,
            },
        ),
        (
            // `exit_status = None` covers "killed by signal / unknown cause".
            "snap_terminal_closed_signal_unknown",
            FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(0x0000_002A),
                exit_status: None,
                reason: CloseReason::Unknown,
            },
        ),
        (
            // A stated reason rides as additive field 3; `Unknown` above is
            // the absent-field shape the two goldens before it pin.
            "snap_terminal_closed_parent_closed",
            FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(0x0000_002B),
                exit_status: None,
                reason: CloseReason::ParentClosed,
            },
        ),
        (
            "snap_terminal_closed_killed_with_exit_code",
            FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(0x0000_002A),
                exit_status: Some(-9),
                reason: CloseReason::Killed,
            },
        ),
        // APPEND_RESOURCE_OUTPUT (tag 0x1a): the producer verb, one complete
        // AgentEventsJsonlV1 record.
        (
            "snap_command_append_resource_output",
            FrameKind::Command {
                request_id: 0x0000_0010,
                command: Command::AppendResourceOutput {
                    terminal_id: ResourceId::local(0x0000_002B),
                    bytes: b"{\"type\":\"prompt\",\"data\":{\"len\":12}}\n".to_vec(),
                },
            },
        ),
        // COMMAND_RESULT error codes minted for producer-fed resources.
        (
            "snap_command_result_error_wrong_resource_kind",
            FrameKind::CommandResult {
                request_id: 0x0000_0010,
                result: CommandResult::Error {
                    code: ErrorCode::WrongResourceKind,
                    message: "terminals are fed by their pty".to_owned(),
                },
            },
        ),
        (
            "snap_command_result_error_not_producer",
            FrameKind::CommandResult {
                request_id: 0x0000_0011,
                result: CommandResult::Error {
                    code: ErrorCode::NotProducer,
                    message: String::new(),
                },
            },
        ),
        (
            "snap_command_result_error_record_invalid",
            FrameKind::CommandResult {
                request_id: 0x0000_0012,
                result: CommandResult::Error {
                    code: ErrorCode::RecordInvalid,
                    message: "unknown record type".to_owned(),
                },
            },
        ),
        (
            "snap_command_result_error_overflow",
            FrameKind::CommandResult {
                request_id: 0x0000_0013,
                result: CommandResult::Error {
                    code: ErrorCode::Overflow,
                    message: String::new(),
                },
            },
        ),
        // ATTACHED carrying the trailing resource-facet list: one Terminal
        // and one AgentSession child bound to it.
        (
            "snap_attached_with_agent_session_facets",
            FrameKind::Attached {
                attach_id: 1,
                initial_client_id: ClientId::new(7),
                snapshot: SessionSnapshot::new(
                    SessionId::new(1),
                    WindowId::new(10),
                    ResourceId::local(0x2A),
                )
                .with_resources(vec![
                    ResourceInfo::new(ResourceId::local(0x2A), WindowId::new(10), 80, 24),
                    ResourceInfo::resource(ResourceId::local(0x2B), ResourceKind::AgentSession)
                        .with_parent(Some(ResourceId::local(0x2A)))
                        .with_agent(Some(
                            AgentFacet::new("claude", "working")
                                .with_native_id(Some("session-42".to_owned())),
                        )),
                ]),
            },
        ),
        // GET_STATE reply from a federation hub carrying the trailing
        // host-session inventory: no resource facets (a zero-count facet
        // list anchors the position), one reachable satellite with one
        // session, one unreachable satellite.
        (
            "snap_command_result_state_with_host_inventory",
            FrameKind::CommandResult {
                request_id: 0x0000_0014,
                result: CommandResult::OkWith(CommandValue::State(
                    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
                        .with_sessions(vec![
                            SessionInfo::new(SessionId::new(1), "work").with_window_count(1),
                        ])
                        .with_hosts(vec![
                            HostInventory::reachable(
                                SatelliteHost::new("edge"),
                                vec![
                                    HostSessionInfo::new(SessionId::new(1), "build")
                                        .with_window_count(2)
                                        .with_pane_count(3)
                                        .with_attached_client_count(1)
                                        .with_active_resource(Some(ResourceId::satellite(
                                            SatelliteHost::new("edge"),
                                            7,
                                        ))),
                                ],
                            ),
                            HostInventory::unreachable(SatelliteHost::new("down"), "link is down"),
                        ]),
                )),
            },
        ),
        // BOOTSTRAP_BEGIN for an AgentSession stream: codec tag 3, raw
        // output mode, no grid.
        (
            "snap_bootstrap_begin_agent_events_jsonl_v1",
            FrameKind::BootstrapBegin {
                terminal_id: ResourceId::local(0x2B),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                profile: BootstrapStreamProfile::AgentEventsJsonlV1,
                cols: 0,
                rows: 0,
                base_seq: 0,
            },
        ),
        (
            "snap_terminal_resize_standard",
            FrameKind::ResizeTerminal {
                terminal_id: ResourceId::local(0x0000_002A),
                cols: 80,
                rows: 24,
            },
        ),
    ]
}

/// Every fixture's hex dump matches its committed golden byte-for-byte.
#[test]
fn frame_wire_snapshots_match_goldens() {
    for (name, frame) in frame_fixtures() {
        insta::assert_snapshot!(name, dump_frame(&frame));
    }
}
