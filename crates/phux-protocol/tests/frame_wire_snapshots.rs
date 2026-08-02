//! Snapshot tests for the SPEC §13-conformant wire frames.
//!
//! Each test encodes a representative fixture of an `ATTACH` / `ATTACHED` /
//! `TERMINAL_SNAPSHOT` / `TERMINAL_OUTPUT` / `DETACH` / `DETACHED` / `INPUT_*` /
//! `BELL` frame, hex-dumps the bytes, and compares against a committed
//! `.snap` file under `tests/snapshots/`. The wire format is a
//! cross-implementation contract — any change MUST surface as a visible
//! diff in pull-request review.

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, Layer, LayerSet, ServerCapabilities};
use phux_protocol::ids::{ClientId, GroupId, SessionId, TerminalId, WindowId};
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    AttachTarget, ErrorCode, FrameKind, Scope, SpawnError, SpawnResult, ViewportInfo,
};
use phux_protocol::wire::info::{
    LayoutNode, SessionInfo, SessionSnapshot, SplitDir, TerminalInfo, WindowInfo,
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
// ATTACH — SPEC §13. The four AttachTarget variants plus viewport pixel-dim
// presence both ways.
// -----------------------------------------------------------------------------

const fn vp_no_pixels() -> ViewportInfo {
    ViewportInfo::new(80, 24)
}

const fn vp_with_pixels() -> ViewportInfo {
    ViewportInfo::new(80, 24).with_pixels(Some(1280), Some(720))
}

#[test]
fn snap_attach_target_last() {
    let frame = FrameKind::Attach {
        target: AttachTarget::Last,
        viewport: vp_no_pixels(),
        request_scrollback: false,
        scrollback_limit_lines: 0,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_attach_target_by_name() {
    let frame = FrameKind::Attach {
        target: AttachTarget::ByName("default".to_owned()),
        viewport: vp_no_pixels(),
        request_scrollback: false,
        scrollback_limit_lines: 0,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_attach_target_by_id() {
    let frame = FrameKind::Attach {
        target: AttachTarget::ById(SessionId::new(7)),
        viewport: vp_no_pixels(),
        request_scrollback: false,
        scrollback_limit_lines: 0,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_attach_target_create_if_missing_minimal() {
    let frame = FrameKind::Attach {
        target: AttachTarget::CreateIfMissing {
            name: "dev".to_owned(),
            command: None,
            cwd: None,
        },
        viewport: vp_no_pixels(),
        request_scrollback: false,
        scrollback_limit_lines: 0,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_attach_target_create_if_missing_full() {
    let frame = FrameKind::Attach {
        target: AttachTarget::CreateIfMissing {
            name: "dev".to_owned(),
            command: Some(vec!["zsh".to_owned()]),
            cwd: Some("/tmp".to_owned()),
        },
        viewport: vp_no_pixels(),
        request_scrollback: true,
        scrollback_limit_lines: 10_000,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_attach_viewport_with_pixels() {
    let frame = FrameKind::Attach {
        target: AttachTarget::ByName("default".to_owned()),
        viewport: vp_with_pixels(),
        request_scrollback: false,
        scrollback_limit_lines: 0,
    };
    insta::assert_snapshot!(dump_frame(&frame));
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

// -----------------------------------------------------------------------------
// ATTACHED — SPEC §13 full SessionSnapshot, with a non-trivial layout tree.
// -----------------------------------------------------------------------------

#[test]
fn snap_attached_empty_graph() {
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(0), TerminalId::local(0))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "default")
                .with_created_at_unix_secs(1_700_000_000)
                .with_attached_client_count(1),
        ]);
    let frame = FrameKind::Attached {
        snapshot,
        initial_client_id: ClientId::new(42),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_attached_realistic_graph() {
    let sessions = vec![
        SessionInfo::new(SessionId::new(1), "work")
            .with_active_window(Some(WindowId::new(10)))
            .with_created_at_unix_secs(1_700_000_000)
            .with_window_count(2)
            .with_attached_client_count(1),
        SessionInfo::new(SessionId::new(2), "personal")
            .with_active_window(Some(WindowId::new(30)))
            .with_created_at_unix_secs(1_700_000_500)
            .with_window_count(1),
    ];

    let windows = vec![
        WindowInfo::new(WindowId::new(10), SessionId::new(1), "code")
            .with_active_pane(Some(TerminalId::local(100)))
            .with_layout(Some(LayoutNode::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.5,
                left: Box::new(LayoutNode::Leaf(TerminalId::local(100))),
                right: Box::new(LayoutNode::Leaf(TerminalId::local(101))),
            })),
        WindowInfo::new(WindowId::new(20), SessionId::new(1), "logs")
            .with_index(1)
            .with_active_pane(Some(TerminalId::local(102)))
            .with_layout(Some(LayoutNode::Leaf(TerminalId::local(102)))),
        WindowInfo::new(WindowId::new(30), SessionId::new(2), "scratch")
            .with_active_pane(Some(TerminalId::local(103)))
            .with_layout(Some(LayoutNode::Leaf(TerminalId::local(103)))),
    ];

    let panes = vec![
        TerminalInfo::new(TerminalId::local(100), WindowId::new(10), 80, 24)
            .with_title(Some("editor".to_owned()))
            .with_cwd(Some("/home/u/src".to_owned())),
        TerminalInfo::new(TerminalId::local(101), WindowId::new(10), 80, 24)
            .with_cwd(Some("/home/u/src".to_owned())),
        TerminalInfo::new(TerminalId::local(102), WindowId::new(20), 160, 48),
        TerminalInfo::new(TerminalId::local(103), WindowId::new(30), 80, 24)
            .with_cwd(Some("/home/u".to_owned())),
    ];

    let snapshot =
        SessionSnapshot::new(SessionId::new(1), WindowId::new(10), TerminalId::local(100))
            .with_sessions(sessions)
            .with_windows(windows)
            .with_panes(panes);
    let frame = FrameKind::Attached {
        snapshot,
        initial_client_id: ClientId::new(1),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

// -----------------------------------------------------------------------------
// TERMINAL_OUTPUT (SPEC §8.1, ADR-0013) — hot-path bytes-on-wire.
// -----------------------------------------------------------------------------

#[test]
fn snap_terminal_output_hello_world() {
    // A representative TERMINAL_OUTPUT carrying ASCII bytes: "hello world\r\n".
    let frame = FrameKind::TerminalOutput {
        terminal_id: TerminalId::local(1),
        seq: 0,
        bytes: bytes::Bytes::from_static(b"hello world\r\n"),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_terminal_output_empty_bytes() {
    let frame = FrameKind::TerminalOutput {
        terminal_id: TerminalId::local(0x0000_002A),
        seq: 1,
        bytes: bytes::Bytes::new(),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_terminal_output_with_sgr() {
    // A short bold-red sequence: validates the wire envelope is bytes-
    // transparent — the SGR is opaque to the protocol.
    let frame = FrameKind::TerminalOutput {
        terminal_id: TerminalId::local(7),
        seq: 42,
        bytes: bytes::Bytes::from_static(b"\x1b[1;31mERR\x1b[0m"),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

// -----------------------------------------------------------------------------
// TERMINAL_SNAPSHOT — SPEC §8.4, ADR-0013. vt_replay_bytes body shape.
// -----------------------------------------------------------------------------

#[test]
fn snap_terminal_snapshot_empty_vt() {
    let frame = FrameKind::TerminalSnapshot {
        terminal_id: TerminalId::local(100),
        cols: 80,
        rows: 24,
        vt_replay_bytes: Vec::new(),
        scrollback_bytes: None,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_terminal_snapshot_minimal_replay() {
    // Reset + CUP home + a single ASCII char + cursor placement.
    let frame = FrameKind::TerminalSnapshot {
        terminal_id: TerminalId::local(100),
        cols: 80,
        rows: 24,
        vt_replay_bytes: b"\x1b[!p\x1b[2J\x1b[HH\x1b[1;2H".to_vec(),
        scrollback_bytes: None,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_terminal_snapshot_with_scrollback() {
    let frame = FrameKind::TerminalSnapshot {
        terminal_id: TerminalId::local(100),
        cols: 80,
        rows: 24,
        vt_replay_bytes: b"\x1b[!p\x1b[2J\x1b[H".to_vec(),
        scrollback_bytes: Some(b"prior line one\r\nprior line two\r\n".to_vec()),
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

// -----------------------------------------------------------------------------
// FRAME_ACK — SPEC §7.proto.1 / §12.2. Per-Terminal cumulative ack from the
// client; used by the server's per-consumer SnapshotSynthesizer eviction
// (ADR-0018 / phux-q0e.4).
// -----------------------------------------------------------------------------

#[test]
fn snap_frame_ack_zero() {
    let frame = FrameKind::FrameAck {
        terminal_id: TerminalId::local(0x0000_0001),
        seq: 0,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_frame_ack_nonzero() {
    let frame = FrameKind::FrameAck {
        terminal_id: TerminalId::local(0x0000_002A),
        seq: 0x0000_0000_0000_0F42,
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

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
// HELLO — SPEC §6.1 / §6.2. One fixture per `ColorSupport` variant. The
// wire body is `client_name + (major, minor, patch) + color_support_tag +
// layers + image_protocols + kbd_protocols + hyperlinks`; the only byte that
// changes across the four color snapshots is the color tag.
// -----------------------------------------------------------------------------

fn hello_with_color(color: ColorSupport) -> FrameKind {
    FrameKind::Hello {
        client_name: "phux-client/test".to_owned(),
        protocol_major: 0,
        protocol_minor: 2,
        protocol_patch: 0,
        client_caps: ClientCapabilities::new().with_color_support(color),
    }
}

#[test]
fn snap_hello_color_truecolor() {
    insta::assert_snapshot!(dump_frame(&hello_with_color(ColorSupport::TrueColor)));
}

#[test]
fn snap_hello_color_indexed256() {
    insta::assert_snapshot!(dump_frame(&hello_with_color(ColorSupport::Indexed256)));
}

#[test]
fn snap_hello_color_indexed16() {
    insta::assert_snapshot!(dump_frame(&hello_with_color(ColorSupport::Indexed16)));
}

#[test]
fn snap_hello_color_mono() {
    insta::assert_snapshot!(dump_frame(&hello_with_color(ColorSupport::Mono)));
}

#[test]
fn snap_hello_layers_l1_only() {
    // Default LayerSet — agent / recorder consumer (SPEC §16.1).
    let frame = FrameKind::Hello {
        client_name: "phux-agent/test".to_owned(),
        protocol_major: 0,
        protocol_minor: 2,
        protocol_patch: 0,
        client_caps: ClientCapabilities::new()
            .with_color_support(ColorSupport::TrueColor)
            .with_layers(LayerSet::new()),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_hello_layers_l1_l3() {
    // GUI / shared-TUI consumer (SPEC §16.2).
    let frame = FrameKind::Hello {
        client_name: "phux-gui/test".to_owned(),
        protocol_major: 0,
        protocol_minor: 2,
        protocol_patch: 0,
        client_caps: ClientCapabilities::new()
            .with_color_support(ColorSupport::TrueColor)
            .with_layers(LayerSet::with(&[Layer::L3])),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

#[test]
fn snap_hello_layers_all() {
    // Reference TUI — L1 + L2 + L3 (SPEC §16.3).
    let frame = FrameKind::Hello {
        client_name: "phux-tui/test".to_owned(),
        protocol_major: 0,
        protocol_minor: 2,
        protocol_patch: 0,
        client_caps: ClientCapabilities::new()
            .with_color_support(ColorSupport::TrueColor)
            .with_layers(LayerSet::all()),
    };
    insta::assert_snapshot!(dump_frame(&frame));
}

// -----------------------------------------------------------------------------
// HELLO_OK — SPEC §6.1. Server handshake ack. Body is `(major, minor, patch)
// + server_caps.layers + length-prefixed server_id`. The version triple and
// `server_id` are the cross-implementation contract a reconnecting client
// pins; the canonical dump is referenced from `docs/spec/appendix-encoding.md`.
// -----------------------------------------------------------------------------

#[test]
fn snap_hello_ok() {
    // Canonical fixture: the reference server's reply — selected version
    // 0.2.0, full tier set (L1+L2+L3), and a fixed opaque `server_id`.
    let frame = FrameKind::HelloOk {
        protocol_major: 0,
        protocol_minor: 2,
        protocol_patch: 0,
        server_caps: ServerCapabilities::new().with_layers(LayerSet::all()),
        server_id: b"phux-srv".to_vec(),
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
