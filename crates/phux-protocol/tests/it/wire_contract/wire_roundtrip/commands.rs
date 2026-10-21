//! `COMMAND` variants, `GET_SCREEN` compatibility, and input routing.

use super::{
    BytesMut, Command, DecodeError, FileUploadId, FocusEvent, FrameKind, InputEvent, InputMode,
    KeyAction, KeyEvent, ListenerTransport, ModSet, MouseAction, MouseButton, MouseEvent,
    PasteEvent, PasteTrust, PerfOp, PhysicalKey, ReportedAgentState, ResourceId, StateScope,
    TerminalSignal, assert_round_trip, framed_tlv, tlv_field,
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
/// A `COMMAND` frame with `request_id` around this positional body.
fn command_frame(request_id: u32, body: &[u8]) -> Vec<u8> {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &request_id.to_be_bytes()); // REQUEST_ID
    tlv_field(&mut fields, 2, body); // COMMAND
    framed_tlv(0x31, &fields)
}

/// `GET_SCREEN` for local Terminal `id`, `request_scrollback: None`, up to
/// (not including) its `cells` byte.
fn get_screen_head(id: u32) -> Vec<u8> {
    let mut body = vec![0x07u8, 0x00]; // COMMAND_TAG_GET_SCREEN, RESOURCE_ID_TAG_LOCAL
    body.extend_from_slice(&id.to_be_bytes());
    body.push(0x00); // request_scrollback = None
    body
}

#[test]
fn get_screen_cells_and_format_are_required_not_inferred() {
    // Presence is never inferred from the bytes left: a body that stops
    // before `cells` or `format` is truncated, so a field a newer build
    // appends cannot be misread as one of them.
    let mut no_format = get_screen_head(9);
    no_format.push(0x01);
    for body in [get_screen_head(9), no_format] {
        assert_eq!(
            FrameKind::decode(&command_frame(7, &body)).unwrap_err(),
            DecodeError::UnexpectedEof
        );
    }
}

#[test]
fn get_screen_skips_a_tail_without_bleeding_into_the_next_frame() {
    // Bytes a newer build appends after `format` are skipped, bounded by
    // the COMMAND field and the frame, so the next frame decodes intact.
    let mut grown = get_screen_head(1);
    grown.extend_from_slice(&[0x01, 0x02, 0xEE, 0xEE, 0xEE]);
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
    let mut buf = command_frame(1, &grown);
    buf.extend_from_slice(&second_buf);

    let (decoded_first, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(
        decoded_first,
        FrameKind::Command {
            request_id: 1,
            command: Command::GetScreen {
                terminal_id: ResourceId::local(1),
                request_scrollback: None,
                cells: true,
                format: 2,
            },
        },
    );
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
