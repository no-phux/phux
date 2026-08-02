//! Snapshot tests for the SPEC §13-conformant wire frames.
//!
//! One table-driven test encodes a representative fixture of every frame
//! kind, hex-dumps the bytes, and compares against a committed `.snap` file
//! under `tests/snapshots/` via insta *named* snapshots. The wire format is a
//! cross-implementation contract — any change MUST surface as a visible
//! diff in pull-request review.
//!
//! The fixture names below are load-bearing: each is the snapshot name, so
//! `tests/snapshots/frame_wire_snapshots__<name>.snap` must exist and stay
//! byte-identical. Renaming a fixture orphans its golden.

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, Layer, LayerSet, ServerCapabilities};
use phux_protocol::ids::{ClientId, GroupId, SessionId, TerminalId, WindowId};
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    AttachTarget, ErrorCode, FrameKind, MoveError, MoveResult, Scope, SpawnError, SpawnResult,
    ViewportInfo,
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

const fn vp_no_pixels() -> ViewportInfo {
    ViewportInfo::new(80, 24)
}

const fn vp_with_pixels() -> ViewportInfo {
    ViewportInfo::new(80, 24).with_pixels(Some(1280), Some(720))
}

const fn attach(target: AttachTarget) -> FrameKind {
    FrameKind::Attach {
        target,
        viewport: vp_no_pixels(),
        request_scrollback: false,
        scrollback_limit_lines: 0,
    }
}

/// HELLO — SPEC §6.1 / §6.2. The wire body is `client_name + (major, minor,
/// patch) + color_support_tag + layers + image_protocols + kbd_protocols +
/// hyperlinks`; the only byte that changes across the four color fixtures is
/// the color tag.
fn hello_with_color(color: ColorSupport) -> FrameKind {
    FrameKind::Hello {
        client_name: "phux-client/test".to_owned(),
        protocol_major: 0,
        protocol_minor: 2,
        protocol_patch: 0,
        client_caps: ClientCapabilities::new().with_color_support(color),
    }
}

fn hello_with_layers(client_name: &str, layers: LayerSet) -> FrameKind {
    FrameKind::Hello {
        client_name: client_name.to_owned(),
        protocol_major: 0,
        protocol_minor: 2,
        protocol_patch: 0,
        client_caps: ClientCapabilities::new()
            .with_color_support(ColorSupport::TrueColor)
            .with_layers(layers),
    }
}

/// ATTACHED — SPEC §13 full `SessionSnapshot`, with a non-trivial layout tree.
fn attached_realistic_graph() -> FrameKind {
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
    FrameKind::Attached {
        snapshot,
        initial_client_id: ClientId::new(1),
    }
}

/// The (snapshot name, frame) fixture table. Grouped in SPEC order:
/// ATTACH (the four `AttachTarget` variants plus viewport pixel-dim presence
/// both ways), DETACH/DETACHED, `INPUT_*`, ATTACHED, `TERMINAL_OUTPUT` (§8.1,
/// ADR-0013), `TERMINAL_SNAPSHOT` (§8.4), BELL, `FRAME_ACK` (§7.proto.1 /
/// §12.2, ADR-0018 / phux-q0e.4), `VIEWPORT_RESIZE` (§10.5), ERROR (§14; the
/// canonical phux-byc.6.6 case is ATTACH against an unknown session), HELLO
/// (§6.1/§6.2), `HELLO_OK` (§6.1; the canonical dump is referenced from
/// `docs/spec/appendix-encoding.md`), L3 metadata + replies (§7.4 / §11.L3,
/// phux-4li.2/.8), and L1 Terminal lifecycle (§7.2 / §10.1, phux-4li.10).
#[allow(clippy::too_many_lines)]
fn frame_fixtures() -> Vec<(&'static str, FrameKind)> {
    vec![
        // ATTACH
        ("snap_attach_target_last", attach(AttachTarget::Last)),
        (
            "snap_attach_target_by_name",
            attach(AttachTarget::ByName("default".to_owned())),
        ),
        (
            "snap_attach_target_by_id",
            attach(AttachTarget::ById(SessionId::new(7))),
        ),
        (
            "snap_attach_target_create_if_missing_minimal",
            attach(AttachTarget::CreateIfMissing {
                name: "dev".to_owned(),
                command: None,
                cwd: None,
            }),
        ),
        (
            "snap_attach_target_create_if_missing_full",
            FrameKind::Attach {
                target: AttachTarget::CreateIfMissing {
                    name: "dev".to_owned(),
                    command: Some(vec!["zsh".to_owned()]),
                    cwd: Some("/tmp".to_owned()),
                },
                viewport: vp_no_pixels(),
                request_scrollback: true,
                scrollback_limit_lines: 10_000,
            },
        ),
        (
            "snap_attach_viewport_with_pixels",
            FrameKind::Attach {
                target: AttachTarget::ByName("default".to_owned()),
                viewport: vp_with_pixels(),
                request_scrollback: false,
                scrollback_limit_lines: 0,
            },
        ),
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
        // ATTACHED
        (
            "snap_attached_empty_graph",
            FrameKind::Attached {
                snapshot: SessionSnapshot::new(
                    SessionId::new(1),
                    WindowId::new(0),
                    TerminalId::local(0),
                )
                .with_sessions(vec![
                    SessionInfo::new(SessionId::new(1), "default")
                        .with_created_at_unix_secs(1_700_000_000)
                        .with_attached_client_count(1),
                ]),
                initial_client_id: ClientId::new(42),
            },
        ),
        ("snap_attached_realistic_graph", attached_realistic_graph()),
        // TERMINAL_OUTPUT — hot-path bytes-on-wire; the envelope is bytes-
        // transparent (the SGR fixture's escape sequence is opaque to the
        // protocol).
        (
            "snap_terminal_output_hello_world",
            FrameKind::TerminalOutput {
                terminal_id: TerminalId::local(1),
                seq: 0,
                bytes: bytes::Bytes::from_static(b"hello world\r\n"),
            },
        ),
        (
            "snap_terminal_output_empty_bytes",
            FrameKind::TerminalOutput {
                terminal_id: TerminalId::local(0x0000_002A),
                seq: 1,
                bytes: bytes::Bytes::new(),
            },
        ),
        (
            "snap_terminal_output_with_sgr",
            FrameKind::TerminalOutput {
                terminal_id: TerminalId::local(7),
                seq: 42,
                bytes: bytes::Bytes::from_static(b"\x1b[1;31mERR\x1b[0m"),
            },
        ),
        // TERMINAL_SNAPSHOT — vt_replay_bytes body shape.
        (
            "snap_terminal_snapshot_empty_vt",
            FrameKind::TerminalSnapshot {
                terminal_id: TerminalId::local(100),
                cols: 80,
                rows: 24,
                vt_replay_bytes: Vec::new(),
                scrollback_bytes: None,
            },
        ),
        (
            // Reset + CUP home + a single ASCII char + cursor placement.
            "snap_terminal_snapshot_minimal_replay",
            FrameKind::TerminalSnapshot {
                terminal_id: TerminalId::local(100),
                cols: 80,
                rows: 24,
                vt_replay_bytes: b"\x1b[!p\x1b[2J\x1b[HH\x1b[1;2H".to_vec(),
                scrollback_bytes: None,
            },
        ),
        (
            "snap_terminal_snapshot_with_scrollback",
            FrameKind::TerminalSnapshot {
                terminal_id: TerminalId::local(100),
                cols: 80,
                rows: 24,
                vt_replay_bytes: b"\x1b[!p\x1b[2J\x1b[H".to_vec(),
                scrollback_bytes: Some(b"prior line one\r\nprior line two\r\n".to_vec()),
            },
        ),
        (
            "snap_bell",
            FrameKind::Bell {
                terminal_id: TerminalId::local(0x0000_00BE),
            },
        ),
        // FRAME_ACK — per-Terminal cumulative ack from the client.
        (
            "snap_frame_ack_zero",
            FrameKind::FrameAck {
                terminal_id: TerminalId::local(0x0000_0001),
                seq: 0,
            },
        ),
        (
            "snap_frame_ack_nonzero",
            FrameKind::FrameAck {
                terminal_id: TerminalId::local(0x0000_002A),
                seq: 0x0000_0000_0000_0F42,
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
        // HELLO — one fixture per ColorSupport variant, then the LayerSet
        // consumer profiles (SPEC §16.1–§16.3).
        (
            "snap_hello_color_truecolor",
            hello_with_color(ColorSupport::TrueColor),
        ),
        (
            "snap_hello_color_indexed256",
            hello_with_color(ColorSupport::Indexed256),
        ),
        (
            "snap_hello_color_indexed16",
            hello_with_color(ColorSupport::Indexed16),
        ),
        (
            "snap_hello_color_mono",
            hello_with_color(ColorSupport::Mono),
        ),
        (
            // Default LayerSet — agent / recorder consumer.
            "snap_hello_layers_l1_only",
            hello_with_layers("phux-agent/test", LayerSet::new()),
        ),
        (
            // GUI / shared-TUI consumer.
            "snap_hello_layers_l1_l3",
            hello_with_layers("phux-gui/test", LayerSet::with(&[Layer::L3])),
        ),
        (
            // Reference TUI — L1 + L2 + L3.
            "snap_hello_layers_all",
            hello_with_layers("phux-tui/test", LayerSet::all()),
        ),
        // HELLO_OK — canonical fixture: the reference server's reply —
        // selected version 0.2.0, full tier set, a fixed opaque server_id.
        (
            "snap_hello_ok",
            FrameKind::HelloOk {
                protocol_major: 0,
                protocol_minor: 2,
                protocol_patch: 0,
                server_caps: ServerCapabilities::new().with_layers(LayerSet::all()),
                server_id: b"phux-srv".to_vec(),
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
