//! `COMMAND` variants, `GET_SCREEN` compatibility, and input routing.

use super::{
    BytesMut, Command, FileUploadId, FocusEvent, FrameKind, InputEvent, InputMode, KeyAction,
    KeyEvent, ListenerTransport, ModSet, MouseAction, MouseButton, MouseEvent, PasteEvent,
    PasteTrust, PerfOp, PhysicalKey, ReportedAgentState, ResourceId, StateScope, TerminalSignal,
    assert_round_trip, framed_tlv, tlv_field,
};

/// Command verbs share one wire shape (tag + positional body); one table
/// covers every variant `arb_frame_kind` does not, with Local and Satellite
/// ids, empty/singleton/multi id lists, and every presence byte.
#[test]
#[allow(clippy::too_many_lines, reason = "one flat table of command samples")]
fn command_variants_round_trip() {
    let id_lists = || {
        [
            Vec::new(),
            vec![ResourceId::local(7)],
            vec![
                ResourceId::local(1),
                ResourceId::local(2),
                ResourceId::satellite("peer-a", 9),
            ],
        ]
    };
    let mut commands = Vec::new();
    for terminal_id in [ResourceId::local(7), ResourceId::satellite("devbox", 7)] {
        commands.push(Command::AttachResource {
            terminal_id: terminal_id.clone(),
            role_policy: None,
            agent_after_seq: None,
        });
        commands.push(Command::DetachResource { terminal_id });
    }
    for mode in [InputMode::Cooperative, InputMode::Seize] {
        commands.push(Command::AcquireInput {
            terminal_id: ResourceId::local(7),
            mode,
            ttl_ms: 30_000,
        });
    }
    for signal in [
        TerminalSignal::Interrupt,
        TerminalSignal::Freeze,
        TerminalSignal::Resume,
        TerminalSignal::Terminate,
        TerminalSignal::Kill,
    ] {
        commands.push(Command::SignalTerminal {
            terminal_id: ResourceId::local(3),
            signal,
            operation_id: None,
        });
    }
    for request_scrollback in [None, Some(0), Some(42)] {
        for cells in [false, true] {
            for format in [0, 1, 2] {
                commands.push(Command::GetScreen {
                    terminal_id: ResourceId::local(5),
                    request_scrollback,
                    cells,
                    format,
                });
            }
        }
    }
    for ids in id_lists() {
        commands.push(Command::KillResources {
            ids,
            operation_id: None,
        });
    }
    for ids in id_lists() {
        commands.push(Command::CloseTabResources { ids });
    }
    for session in [None, Some("work".to_owned()), Some(String::new())] {
        commands.push(Command::DetachClients { session });
    }
    commands.extend([
        Command::GetState {
            scope: StateScope::Server,
        },
        Command::Upgrade,
        Command::Shutdown,
        Command::ReleaseInput {
            terminal_id: ResourceId::local(7),
        },
        Command::ReportAsked {
            terminal_id: ResourceId::local(7),
            id: "q1".to_owned(),
            question: "Deploy to prod?".to_owned(),
            suggestions: vec!["Yes".to_owned(), "No".to_owned(), "Hold".to_owned()],
            elapsed_seconds: Some(9),
        },
        Command::ReportAgentState {
            terminal_id: ResourceId::local(7),
            state: ReportedAgentState::Done,
        },
        Command::GetPerf { reset: false },
        Command::GetPerf { reset: true },
        Command::PerfControl {
            op: PerfOp::TraceStart,
        },
        Command::PerfControl {
            op: PerfOp::TraceStop,
        },
        Command::PerfControl {
            op: PerfOp::AllocOn,
        },
        Command::PerfControl {
            op: PerfOp::AllocOff,
        },
        // An undefined op still round-trips; the server refuses it by name.
        Command::PerfControl {
            op: PerfOp::Unknown(0xEE),
        },
        Command::OpenListener {
            transport: ListenerTransport::Quic,
            port_range: None,
            linger_secs: 0,
        },
        Command::OpenListener {
            transport: ListenerTransport::Quic,
            port_range: Some((60000, 61000)),
            linger_secs: 120,
        },
        // An undefined transport and an inverted range still round-trip:
        // refusing them is the server's job, with a message.
        Command::OpenListener {
            transport: ListenerTransport::Unknown(7),
            port_range: Some((9, 3)),
            linger_secs: u32::MAX,
        },
        Command::Transcribe {
            upload_id: FileUploadId::new([9; 16]).expect("non-zero upload id"),
            terminal_id: ResourceId::local(7),
        },
    ]);
    for command in commands {
        assert_round_trip(&FrameKind::Command {
            request_id: 7,
            command,
        });
    }
}

#[test]
fn get_screen_without_format_is_byte_identical() {
    // Backward-compat (D9): a GET_SCREEN frame encoded after `cells`
    // landed but before `format` existed has a body that ends after
    // `cells`. A current decoder must read the missing `format` as `0`
    // (no rendering), not error on EOF — the same `at_body_end` guard
    // `cells` itself relies on for the pre-`cells` shape below.
    let expected = FrameKind::Command {
        request_id: 7,
        command: Command::GetScreen {
            terminal_id: ResourceId::local(9),
            request_scrollback: Some(3),
            cells: true,
            format: 0,
        },
    };

    // Command::GetScreen positional value, minus the trailing format byte.
    let mut get_screen = vec![0x07u8]; // COMMAND_TAG_GET_SCREEN
    get_screen.push(0x00); // RESOURCE_ID_TAG_LOCAL
    get_screen.extend_from_slice(&9u32.to_be_bytes());
    get_screen.push(0x01); // request_scrollback = Some
    get_screen.extend_from_slice(&3u32.to_be_bytes());
    get_screen.push(0x01); // cells = true
    // no format byte

    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &7u32.to_be_bytes()); // field::command::REQUEST_ID
    tlv_field(&mut fields, 2, &get_screen); // field::command::COMMAND
    let buf = framed_tlv(0x31, &fields);

    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, expected, "absent format byte must decode as 0");
    assert!(tail.is_empty());
}

#[test]
fn command_get_screen_decodes_pre_cells_body_as_false() {
    // Backward-compat (phux-8yl): a GET_SCREEN frame encoded *before* the
    // trailing `cells` bool existed has a body that ends after
    // `request_scrollback`, with a length header one byte shorter. A
    // current decoder must read the missing `cells` as `false`, not error
    // on EOF. `cells` is a trailing positional field *inside* the COMMAND
    // field's value (the Command::GetScreen body), bounded by that field's
    // length. Build the COMMAND field value with a GetScreen body that ends
    // after `request_scrollback` (the pre-cells shape); the Command sub-decoder
    // sees `at_body_end` and defaults `cells = false`.
    let expected = FrameKind::Command {
        request_id: 7,
        command: Command::GetScreen {
            terminal_id: ResourceId::local(9),
            request_scrollback: Some(3),
            cells: false,
            format: 0,
        },
    };

    // Command::GetScreen positional value, minus the trailing cells byte.
    let mut get_screen = vec![0x07u8]; // COMMAND_TAG_GET_SCREEN
    get_screen.push(0x00); // RESOURCE_ID_TAG_LOCAL
    get_screen.extend_from_slice(&9u32.to_be_bytes());
    get_screen.push(0x01); // request_scrollback = Some
    get_screen.extend_from_slice(&3u32.to_be_bytes());
    // no cells byte

    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &7u32.to_be_bytes()); // field::command::REQUEST_ID
    tlv_field(&mut fields, 2, &get_screen); // field::command::COMMAND
    let buf = framed_tlv(0x31, &fields);

    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, expected, "absent cells byte must decode as false");
    assert!(tail.is_empty());
}

#[test]
fn command_get_screen_back_to_back_frames_dont_bleed_cells() {
    // Two GET_SCREEN frames concatenated in one buffer: decoding the first
    // (whose COMMAND field value omits the trailing `cells` byte, the pre-cells
    // shape of an old peer) must NOT consume the *second* frame's leading byte
    // as its `cells`. Under TLV the outer frame is length-delimited and the
    // COMMAND field value is too, so the boundary holds at both levels (phux-8yl).
    let mut get_screen = vec![0x07u8]; // COMMAND_TAG_GET_SCREEN
    get_screen.push(0x00); // RESOURCE_ID_TAG_LOCAL
    get_screen.extend_from_slice(&1u32.to_be_bytes());
    get_screen.push(0x00); // request_scrollback = None
    // no cells byte
    let mut first_fields = Vec::new();
    tlv_field(&mut first_fields, 1, &1u32.to_be_bytes()); // REQUEST_ID
    tlv_field(&mut first_fields, 2, &get_screen); // COMMAND
    let first = framed_tlv(0x31, &first_fields);

    let second = FrameKind::Command {
        request_id: 2,
        command: Command::GetScreen {
            terminal_id: ResourceId::local(2),
            request_scrollback: None,
            cells: true,
            format: 0,
        },
    };
    let mut second_buf = BytesMut::new();
    second.encode(&mut second_buf);

    let mut buf = first;
    buf.extend_from_slice(&second_buf);

    let (decoded_first, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(
        decoded_first,
        FrameKind::Command {
            request_id: 1,
            command: Command::GetScreen {
                terminal_id: ResourceId::local(1),
                request_scrollback: None,
                cells: false,
                format: 0,
            },
        },
        "first frame's absent cells must default false, not steal frame 2's byte",
    );
    // The remainder must decode as the intact second frame.
    let (decoded_second, rest) = FrameKind::decode(tail).unwrap();
    assert_eq!(decoded_second, second);
    assert!(rest.is_empty());
}

#[test]
fn command_route_input_round_trips() {
    // ROUTE_INPUT (tag 0x08): ResourceId + an InputEvent tagged union.
    // Exercise all four atom variants so each InputEvent tag round-trips.
    let key = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Z,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: Some("z".to_owned()),
        unshifted_codepoint: Some(u32::from('z')),
    };
    let mouse = MouseEvent {
        action: MouseAction::Press,
        button: MouseButton::Left,
        mods: ModSet::empty(),
        x: 12.0,
        y: 7.0,
    };
    let paste = PasteEvent {
        trust: PasteTrust::Trusted,
        data: b"hello".to_vec(),
    };
    for event in [
        InputEvent::Key(key),
        InputEvent::Mouse(mouse),
        InputEvent::Focus(FocusEvent::Gained),
        InputEvent::Paste(paste),
    ] {
        assert_round_trip(&FrameKind::Command {
            request_id: 21,
            command: Command::RouteInput {
                terminal_id: ResourceId::local(5),
                event,
            },
        });
    }
}
