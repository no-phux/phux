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
use phux_protocol::ids::{GroupId, TerminalId};
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    ErrorCode, FrameKind, MoveError, MoveResult, Scope, SpawnError, SpawnResult, ViewportInfo,
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
        // DETACH / DETACHED — unit messages.
        ("snap_detach", FrameKind::Detach),
        ("snap_detached", FrameKind::Detached),
        // INPUT_*
        (
            "snap_input_key_letter_a_press",
            FrameKind::InputKey {
                terminal_id: TerminalId::local(0x0000_0007),
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
                terminal_id: TerminalId::local(0x0000_0001),
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
                terminal_id: TerminalId::local(0x0000_0042),
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
                terminal_id: TerminalId::local(0x0000_0003),
                event: FocusEvent::Gained,
            },
        ),
        (
            "snap_input_focus_lost",
            FrameKind::InputFocus {
                terminal_id: TerminalId::local(0x0000_0003),
                event: FocusEvent::Lost,
            },
        ),
        (
            "snap_input_paste_trusted_ascii",
            FrameKind::InputPaste {
                terminal_id: TerminalId::local(0x0000_0005),
                event: PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: b"hello world".to_vec(),
                },
            },
        ),
        (
            "snap_bell",
            FrameKind::Bell {
                terminal_id: TerminalId::local(0x0000_00BE),
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
                scope: Scope::Terminal(TerminalId::local(0x0000_0009)),
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
        // L1 Terminal lifecycle frames.
        (
            // The minimum SPAWN_TERMINAL: request_id, default group, every
            // optional field absent. Reads as "spawn the server's default
            // shell in its default cwd, inheriting its env."
            "snap_spawn_terminal_minimal",
            FrameKind::SpawnTerminal {
                request_id: 0x0000_0001,
                group: GroupId::new(1),
                command: None,
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        ),
        (
            // All optional fields populated; exercises the env-pair encoding
            // and length-prefixed command list.
            "snap_spawn_terminal_full",
            FrameKind::SpawnTerminal {
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
                owner_terminal: Some(TerminalId::local(42)),
                agent_session: Some(
                    br#"{"plugin_id":"com.phux.agents","native_id":"session-42"}"#.to_vec(),
                ),
            },
        ),
        (
            // The first-class `term` field (phux-ign): field id 6, a bare
            // UTF-8 string. Distinct from the `TERM` env pair above — this is
            // the typed per-spawn override.
            "snap_spawn_terminal_term_field",
            FrameKind::SpawnTerminal {
                request_id: 0x0000_0003,
                group: GroupId::new(1),
                command: None,
                cwd: None,
                env: None,
                term: Some("ghostty".to_owned()),
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        ),
        (
            "snap_terminal_spawned_ok",
            FrameKind::TerminalSpawned {
                request_id: 0x0000_0001,
                result: SpawnResult::Ok(TerminalId::local(0x0000_002A)),
            },
        ),
        (
            "snap_terminal_spawned_err_group_not_found",
            FrameKind::TerminalSpawned {
                request_id: 0x0000_0007,
                result: SpawnResult::Err(SpawnError::GroupNotFound),
            },
        ),
        (
            "snap_terminal_spawned_err_spawn_failed",
            FrameKind::TerminalSpawned {
                request_id: 0x0000_0008,
                result: SpawnResult::Err(SpawnError::SpawnFailed("no pty available".to_owned())),
            },
        ),
        (
            "snap_move_terminal",
            FrameKind::MoveTerminal {
                request_id: 0x0000_0009,
                terminal: TerminalId::local(0x0000_002A),
                owner_terminal: TerminalId::local(0x0000_0007),
            },
        ),
        (
            "snap_terminal_moved_ok",
            FrameKind::TerminalMoved {
                request_id: 0x0000_0009,
                result: MoveResult::Ok(TerminalId::local(0x0000_002A)),
            },
        ),
        (
            "snap_terminal_moved_err_move_failed",
            FrameKind::TerminalMoved {
                request_id: 0x0000_000A,
                result: MoveResult::Err(MoveError::MoveFailed("no such terminal".to_owned())),
            },
        ),
        (
            "snap_terminal_moved_err_unsupported_satellite_route",
            FrameKind::TerminalMoved {
                request_id: 0x0000_000B,
                result: MoveResult::Err(MoveError::UnsupportedSatelliteRoute),
            },
        ),
        (
            "snap_terminal_closed_with_exit_code",
            FrameKind::TerminalClosed {
                terminal_id: TerminalId::local(0x0000_002A),
                exit_status: Some(0),
            },
        ),
        (
            // `exit_status = None` covers "killed by signal / unknown cause".
            "snap_terminal_closed_signal_unknown",
            FrameKind::TerminalClosed {
                terminal_id: TerminalId::local(0x0000_002A),
                exit_status: None,
            },
        ),
        (
            "snap_terminal_resize_standard",
            FrameKind::TerminalResize {
                terminal_id: TerminalId::local(0x0000_002A),
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
