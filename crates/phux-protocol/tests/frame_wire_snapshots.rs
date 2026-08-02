//! Snapshot tests for stable protocol frames outside the protocol-0.7
//! bootstrap surface. Bootstrap and handshake codecs use focused semantic
//! round-trip/malformed tests because their negotiated fields are better
//! defended as values than as a duplicated hex fixture.

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use phux_protocol::ids::{GroupId, TerminalId};
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    ErrorCode, FrameKind, Scope, SpawnError, SpawnResult, ViewportInfo,
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

// -----------------------------------------------------------------------------
// DETACH / DETACHED — unit messages.
// -----------------------------------------------------------------------------

#[test]
fn snap_detach() {
    insta::assert_snapshot!(dump_frame(&FrameKind::Detach));
}

#[test]
fn snap_detached() {
    insta::assert_snapshot!(dump_frame(&FrameKind::Detached));
}

// -----------------------------------------------------------------------------
// INPUT_*.
// -----------------------------------------------------------------------------

#[test]
fn snap_input_key_letter_a_press() {
    let frame = FrameKind::InputKey {
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
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_input_key_no_text() {
    let frame = FrameKind::InputKey {
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
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_input_mouse_left_click() {
    let frame = FrameKind::InputMouse {
        terminal_id: TerminalId::local(0x0000_0042),
        event: MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: 120.0,
            y: 40.5,
        },
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_input_focus_gained() {
    let frame = FrameKind::InputFocus {
        terminal_id: TerminalId::local(0x0000_0003),
        event: FocusEvent::Gained,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_input_focus_lost() {
    let frame = FrameKind::InputFocus {
        terminal_id: TerminalId::local(0x0000_0003),
        event: FocusEvent::Lost,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_input_paste_trusted_ascii() {
    let frame = FrameKind::InputPaste {
        terminal_id: TerminalId::local(0x0000_0005),
        event: PasteEvent {
            trust: PasteTrust::Trusted,
            data: b"hello world".to_vec(),
        },
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_bell() {
    insta::assert_snapshot!(dump_frame(&FrameKind::Bell {
        terminal_id: TerminalId::local(0x0000_00BE),
    }));
}

// -----------------------------------------------------------------------------
// VIEWPORT_RESIZE — SPEC §10.5. Cell-only and pixel-augmented viewports.
// -----------------------------------------------------------------------------

#[test]
fn snap_viewport_resize_cells_only() {
    let frame = FrameKind::ViewportResize {
        viewport: ViewportInfo::new(120, 40),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_viewport_resize_with_pixels() {
    let frame = FrameKind::ViewportResize {
        viewport: ViewportInfo::new(120, 40).with_pixels(Some(1920), Some(1080)),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

// -----------------------------------------------------------------------------
// ERROR — SPEC §14. Server-emitted structured error frames. The canonical
// case from phux-byc.6.6 is ATTACH against an unknown session, which yields
// ERROR { code: SessionNotFound (=102), request_id: None } — sibling refusal
// paths use ErrorCode::{InvalidCommand, UnsupportedSatelliteRoute, …} with
// the same wire shape.
// -----------------------------------------------------------------------------

#[test]
fn snap_error_session_not_found() {
    let frame = FrameKind::Error {
        request_id: None,
        code: ErrorCode::SessionNotFound,
        message: "no such session: 'work'".to_owned(),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_error_with_request_id_invalid_command() {
    let frame = FrameKind::Error {
        request_id: Some(0x0000_002A),
        code: ErrorCode::InvalidCommand,
        message: "missing field: terminal_id".to_owned(),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_error_internal_max_code() {
    // Exercise the u16::MAX (=65535) wire value to lock in the high end of
    // the ErrorCode encoding alongside SPEC §14's `INTERNAL_ERROR = 65535`.
    let frame = FrameKind::Error {
        request_id: None,
        code: ErrorCode::InternalError,
        message: String::new(),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

// -----------------------------------------------------------------------------
// L3 metadata frames — SPEC §7.4 / §11.L3 (phux-4li.2).
// -----------------------------------------------------------------------------

#[test]
fn snap_get_metadata_global() {
    let frame = FrameKind::GetMetadata {
        request_id: 0x0000_0001,
        scope: Scope::Global,
        key: "phux.example/v1".to_owned(),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_get_metadata_group() {
    let frame = FrameKind::GetMetadata {
        request_id: 0x0000_0007,
        scope: Scope::Group(GroupId::new(1)),
        key: "phux.tui.layout/v1".to_owned(),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_get_metadata_terminal() {
    let frame = FrameKind::GetMetadata {
        request_id: 0x0000_0042,
        scope: Scope::Terminal(TerminalId::local(0x0000_0009)),
        key: "phux.tui.title-override/v1".to_owned(),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_set_metadata_group_layout() {
    let frame = FrameKind::SetMetadata {
        request_id: 0x0000_0010,
        scope: Scope::Group(GroupId::new(1)),
        key: "phux.tui.layout/v1".to_owned(),
        value: b"\xa2\x01\x01\x02\x82\x00\x01".to_vec(), // arbitrary CBOR-looking bytes
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_delete_metadata_global() {
    let frame = FrameKind::DeleteMetadata {
        request_id: 0x0000_0011,
        scope: Scope::Global,
        key: "phux.example/v1".to_owned(),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_list_metadata_group() {
    let frame = FrameKind::ListMetadata {
        request_id: 0x0000_0012,
        scope: Scope::Group(GroupId::new(1)),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_subscribe_metadata_group_layout() {
    let frame = FrameKind::SubscribeMetadata {
        scope: Scope::Group(GroupId::new(1)),
        key: "phux.tui.layout/v1".to_owned(),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_metadata_changed_set_group() {
    let frame = FrameKind::MetadataChanged {
        scope: Scope::Group(GroupId::new(1)),
        key: "phux.tui.layout/v1".to_owned(),
        value: Some(b"\xa2\x01\x01\x02\x82\x00\x01".to_vec()),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_metadata_changed_tombstone() {
    let frame = FrameKind::MetadataChanged {
        scope: Scope::Global,
        key: "phux.example/v1".to_owned(),
        value: None,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

// -----------------------------------------------------------------------------
// L3 metadata reply frames — SPEC §7.4 / §11.L3 (phux-4li.8).
// -----------------------------------------------------------------------------

#[test]
fn snap_metadata_value_present() {
    let frame = FrameKind::MetadataValue {
        request_id: 0x0000_0007,
        value: Some(b"\xa2\x01\x01\x02\x82\x00\x01".to_vec()),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_metadata_value_absent() {
    let frame = FrameKind::MetadataValue {
        request_id: 0x0000_0042,
        value: None,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_metadata_keys_empty() {
    let frame = FrameKind::MetadataKeys {
        request_id: 0x0000_0012,
        keys: Vec::new(),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_metadata_keys_populated() {
    let frame = FrameKind::MetadataKeys {
        request_id: 0x0000_0012,
        keys: vec![
            "phux.tui.layout/v1".to_owned(),
            "phux.tui.window_order/v1".to_owned(),
        ],
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

// -----------------------------------------------------------------------------
// L1 Terminal lifecycle frames — SPEC §7.2 / §10.1 (phux-4li.10).
// -----------------------------------------------------------------------------

#[test]
fn snap_spawn_terminal_minimal() {
    // The minimum SPAWN_TERMINAL: request_id, default group, every
    // optional field absent. Reads as "spawn the server's default shell
    // in its default cwd, inheriting its env."
    let frame = FrameKind::SpawnTerminal {
        request_id: 0x0000_0001,
        group: GroupId::new(1),
        command: None,
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session: None,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_spawn_terminal_full() {
    // All optional fields populated; exercises the env-pair encoding and
    // length-prefixed command list.
    let frame = FrameKind::SpawnTerminal {
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
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_spawn_terminal_term_field() {
    // The first-class `term` field (phux-ign): field id 6, a bare UTF-8
    // string. Distinct from the `TERM` env pair above — this is the typed
    // per-spawn override.
    let frame = FrameKind::SpawnTerminal {
        request_id: 0x0000_0003,
        group: GroupId::new(1),
        command: None,
        cwd: None,
        env: None,
        term: Some("ghostty".to_owned()),
        satellite: None,
        owner_terminal: None,
        agent_session: None,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_terminal_spawned_ok() {
    let frame = FrameKind::TerminalSpawned {
        request_id: 0x0000_0001,
        result: SpawnResult::Ok(TerminalId::local(0x0000_002A)),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_terminal_spawned_err_group_not_found() {
    let frame = FrameKind::TerminalSpawned {
        request_id: 0x0000_0007,
        result: SpawnResult::Err(SpawnError::GroupNotFound),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_terminal_spawned_err_spawn_failed() {
    let frame = FrameKind::TerminalSpawned {
        request_id: 0x0000_0008,
        result: SpawnResult::Err(SpawnError::SpawnFailed("no pty available".to_owned())),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_terminal_closed_with_exit_code() {
    let frame = FrameKind::TerminalClosed {
        terminal_id: TerminalId::local(0x0000_002A),
        exit_status: Some(0),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_terminal_closed_signal_unknown() {
    // `exit_status = None` covers "killed by signal / unknown cause".
    let frame = FrameKind::TerminalClosed {
        terminal_id: TerminalId::local(0x0000_002A),
        exit_status: None,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_terminal_resize_standard() {
    let frame = FrameKind::TerminalResize {
        terminal_id: TerminalId::local(0x0000_002A),
        cols: 80,
        rows: 24,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}
