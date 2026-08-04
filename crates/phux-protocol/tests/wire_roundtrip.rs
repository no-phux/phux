//! Wire-codec round-trip and malformed-input tests.
//!
//! Proptest exercises the encoder and decoder on arbitrary `FrameKind`
//! values. Hand-rolled cases cover known-bad inputs and confirm the decoder
//! returns `DecodeError` rather than panicking.
//!
//! Under ADR-0013 the structured-diff codec is gone; the strategies here
//! cover `TerminalOutput` (raw VT bytes) and the new `TerminalSnapshot` (bytes
//! body) in place of the deleted `PaneDiff` strategies.
//!
//! The TLV byte-building helpers (`docs/spec/appendix-encoding.md`) live in
//! `tests/common/mod.rs`, shared with the other wire test files.

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use phux_protocol::caps::{
    ClientCapabilities, ColorSupport, ImageProtocol, ImageProtocolSet, KeyboardProtocol,
    KeyboardProtocolSet, Layer, LayerSet, OutputMode, ServerCapabilities, ServerFeature,
    ServerFeatureSet, TerminalColor, TerminalDefaultColors,
};
use phux_protocol::ids::{
    ClientId, FileUploadId, GroupId, InputOperationId, SessionId, TerminalId, WindowId,
};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, CommandValue, ControlAction, ErrorCode,
    FileUploadAck, InputMode, MAX_APPLY_INPUT_COMMAND_BODY, MAX_APPLY_INPUT_EVENTS,
    MAX_FILE_UPLOAD_CHUNK, MAX_FILE_UPLOAD_SIZE, MoveError, MoveResult, Scope, SpawnError,
    SpawnResult, StateScope, TerminalLifecycle, TerminalSignal, ViewportInfo,
};
use phux_protocol::wire::info::{
    LayoutNode, SessionInfo, SessionSnapshot, SplitDir, TerminalInfo, WindowInfo,
};
use phux_protocol::wire::{DecodeError, decode::Decoder, frame::FrameKind};
use proptest::prelude::*;

mod common;
use common::{attached_with_layout, framed_tlv, tlv_field};

/// The shared body of every round-trip test in this file: encoding then
/// decoding `frame` is the identity, and the decoder consumes the whole
/// buffer. (`FrameKind::decode` delegates to `Decoder::read_frame`, so both
/// entry points are one path.) Inside `proptest!` closures a plain panic is
/// caught and shrunk like a `prop_assert!` failure.
fn assert_round_trip(frame: &FrameKind) {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(&decoded, frame);
    assert!(tail.is_empty());
}

// -----------------------------------------------------------------------------
// Strategies
// -----------------------------------------------------------------------------

fn arb_attach_target() -> impl Strategy<Value = AttachTarget> {
    prop_oneof![
        Just(AttachTarget::Last),
        ".{0,64}".prop_map(AttachTarget::ByName),
        any::<u32>().prop_map(|id| AttachTarget::ById(SessionId::new(id))),
        (
            ".{0,32}",
            proptest::option::of(proptest::collection::vec(".{0,16}", 0..4)),
            proptest::option::of(".{0,32}"),
        )
            .prop_map(|(name, command, cwd)| AttachTarget::CreateIfMissing {
                name,
                command,
                cwd,
            }),
    ]
}

fn arb_viewport_info() -> impl Strategy<Value = ViewportInfo> {
    (
        any::<u16>(),
        any::<u16>(),
        proptest::option::of(any::<u16>()),
        proptest::option::of(any::<u16>()),
    )
        .prop_map(|(cols, rows, pixel_w, pixel_h)| {
            ViewportInfo::new(cols, rows).with_pixels(pixel_w, pixel_h)
        })
}

fn arb_split_dir() -> impl Strategy<Value = SplitDir> {
    prop_oneof![Just(SplitDir::Horizontal), Just(SplitDir::Vertical)]
}

/// Bounded recursion: at most depth 4 keeps prop-test work tractable while
/// still exercising recursive split-tree encoding/decoding.
fn arb_layout_node() -> impl Strategy<Value = LayoutNode> {
    let leaf = any::<u32>().prop_map(|id| LayoutNode::Leaf(TerminalId::local(id)));
    leaf.prop_recursive(4, 32, 2, |inner| {
        (arb_split_dir(), 0.0001f32..0.9999f32, inner.clone(), inner).prop_map(
            |(dir, ratio, left, right)| LayoutNode::Split {
                dir,
                ratio,
                left: Box::new(left),
                right: Box::new(right),
            },
        )
    })
}

fn arb_session_info() -> impl Strategy<Value = SessionInfo> {
    (
        any::<u32>(),
        ".{0,32}",
        proptest::option::of(any::<u32>()),
        any::<i64>(),
        any::<u16>(),
        any::<u16>(),
    )
        .prop_map(
            |(
                id,
                name,
                active_window,
                created_at_unix_secs,
                window_count,
                attached_client_count,
            )| {
                SessionInfo::new(SessionId::new(id), name)
                    .with_active_window(active_window.map(WindowId::new))
                    .with_created_at_unix_secs(created_at_unix_secs)
                    .with_window_count(window_count)
                    .with_attached_client_count(attached_client_count)
            },
        )
}

fn arb_window_info() -> impl Strategy<Value = WindowInfo> {
    (
        any::<u32>(),
        any::<u32>(),
        any::<u16>(),
        ".{0,32}",
        proptest::option::of(any::<u32>()),
        proptest::option::of(arb_layout_node()),
    )
        .prop_map(|(id, session_id, index, name, active_pane, layout)| {
            WindowInfo::new(WindowId::new(id), SessionId::new(session_id), name)
                .with_index(index)
                .with_active_pane(active_pane.map(TerminalId::local))
                .with_layout(layout)
        })
}

fn arb_pane_info() -> impl Strategy<Value = TerminalInfo> {
    (
        any::<u32>(),
        any::<u32>(),
        any::<u16>(),
        any::<u16>(),
        proptest::option::of(".{0,32}"),
        proptest::option::of(".{0,32}"),
    )
        .prop_map(|(id, window_id, cols, rows, title, cwd)| {
            TerminalInfo::new(TerminalId::local(id), WindowId::new(window_id), cols, rows)
                .with_title(title)
                .with_cwd(cwd)
        })
}

fn arb_session_snapshot() -> impl Strategy<Value = SessionSnapshot> {
    (
        proptest::collection::vec(arb_session_info(), 0..3),
        proptest::collection::vec(arb_window_info(), 0..4),
        proptest::collection::vec(arb_pane_info(), 0..5),
        any::<u32>(),
        any::<u32>(),
        any::<u32>(),
    )
        .prop_map(|(sessions, windows, panes, fs, fw, fp)| {
            SessionSnapshot::new(SessionId::new(fs), WindowId::new(fw), TerminalId::new(fp))
                .with_sessions(sessions)
                .with_windows(windows)
                .with_panes(panes)
        })
}

/// Strategy producing one of the simple-payload `FrameKind` variants. The
/// structured variants (`ATTACH`, `ATTACHED`, `TERMINAL_SNAPSHOT`, `TERMINAL_OUTPUT`,
/// input frames) have dedicated proptests below.
fn arb_color_support() -> impl Strategy<Value = ColorSupport> {
    prop_oneof![
        Just(ColorSupport::TrueColor),
        Just(ColorSupport::Indexed256),
        Just(ColorSupport::Indexed16),
        Just(ColorSupport::Mono),
    ]
}

fn arb_frame_kind() -> impl Strategy<Value = FrameKind> {
    prop_oneof![
        (
            ".{0,128}",
            any::<u16>(),
            any::<u16>(),
            any::<u16>(),
            arb_color_support(),
        )
            .prop_map(|(client_name, major, minor, patch, color_support)| {
                FrameKind::Hello {
                    client_name,
                    protocol_major: major,
                    protocol_minor: minor,
                    protocol_patch: patch,
                    client_caps: ClientCapabilities::new().with_color_support(color_support),
                }
            },),
        any::<u64>().prop_map(|nonce| FrameKind::Ping { nonce }),
        Just(FrameKind::Detach),
        Just(FrameKind::Detached),
        arb_terminal_id().prop_map(|terminal_id| FrameKind::Bell { terminal_id }),
    ]
}

/// Strategy producing both `Local` and `Satellite` variants of [`TerminalId`].
/// v0.1 servers only emit `Local`, but v0.1 decoders MUST round-trip both
/// shapes (the dispatch layer is what rejects `Satellite` ids with
/// `UnsupportedSatelliteRoute`).
fn arb_terminal_id() -> impl Strategy<Value = TerminalId> {
    prop_oneof![
        any::<u32>().prop_map(TerminalId::local),
        (".{0,32}", any::<u32>()).prop_map(|(host, id)| TerminalId::satellite(host, id)),
    ]
}

fn arb_focus_event() -> impl Strategy<Value = FocusEvent> {
    prop_oneof![Just(FocusEvent::Gained), Just(FocusEvent::Lost)]
}

fn arb_key_action() -> impl Strategy<Value = KeyAction> {
    prop_oneof![
        Just(KeyAction::Press),
        Just(KeyAction::Release),
        Just(KeyAction::Repeat),
    ]
}

fn arb_physical_key() -> impl Strategy<Value = PhysicalKey> {
    prop_oneof![
        Just(PhysicalKey::Unidentified),
        Just(PhysicalKey::A),
        Just(PhysicalKey::Enter),
        Just(PhysicalKey::Escape),
        Just(PhysicalKey::ArrowUp),
        Just(PhysicalKey::F1),
        Just(PhysicalKey::Numpad7),
    ]
}

fn arb_mod_set() -> impl Strategy<Value = ModSet> {
    any::<u16>().prop_map(|bits| ModSet::from_bits_truncate(bits & ModSet::all().bits()))
}

fn arb_key_event() -> impl Strategy<Value = KeyEvent> {
    (
        arb_key_action(),
        arb_physical_key(),
        arb_mod_set(),
        arb_mod_set(),
        any::<bool>(),
        proptest::option::of(prop::string::string_regex("[a-zA-Z0-9 ]{0,8}").unwrap()),
        proptest::option::of(any::<u32>()),
    )
        .prop_map(
            |(action, key, mods, consumed_mods, composing, text, unshifted_codepoint)| KeyEvent {
                action,
                key,
                mods,
                consumed_mods,
                composing,
                text,
                unshifted_codepoint,
            },
        )
}

fn arb_mouse_action() -> impl Strategy<Value = MouseAction> {
    prop_oneof![
        Just(MouseAction::Press),
        Just(MouseAction::Release),
        Just(MouseAction::Motion),
    ]
}

fn arb_mouse_button() -> impl Strategy<Value = MouseButton> {
    prop_oneof![
        Just(MouseButton::Unknown),
        Just(MouseButton::Left),
        Just(MouseButton::Right),
        Just(MouseButton::Middle),
        Just(MouseButton::Four),
        Just(MouseButton::Eleven),
    ]
}

fn arb_mouse_event() -> impl Strategy<Value = MouseEvent> {
    (
        arb_mouse_action(),
        arb_mouse_button(),
        arb_mod_set(),
        prop::num::f64::NORMAL | prop::num::f64::ZERO | prop::num::f64::SUBNORMAL,
        prop::num::f64::NORMAL | prop::num::f64::ZERO | prop::num::f64::SUBNORMAL,
    )
        .prop_map(|(action, button, mods, x, y)| MouseEvent {
            action,
            button,
            mods,
            x,
            y,
        })
}

/// One of every `ErrorCode` known to SPEC §14. The decoder must round-trip
/// every wire value defined by the spec.
fn arb_error_code() -> impl Strategy<Value = ErrorCode> {
    prop_oneof![
        Just(ErrorCode::VersionIncompatible),
        Just(ErrorCode::UnknownMessageType),
        Just(ErrorCode::MalformedMessage),
        Just(ErrorCode::FrameTooLarge),
        Just(ErrorCode::NotAttached),
        Just(ErrorCode::AlreadyAttached),
        Just(ErrorCode::SessionNotFound),
        Just(ErrorCode::WindowNotFound),
        Just(ErrorCode::TerminalNotFound),
        Just(ErrorCode::ClientNotFound),
        Just(ErrorCode::UnsupportedSatelliteRoute),
        Just(ErrorCode::SatelliteUnreachable),
        Just(ErrorCode::InvalidCommand),
        Just(ErrorCode::PermissionDenied),
        Just(ErrorCode::ResourceExhausted),
        Just(ErrorCode::UnsafePaste),
        Just(ErrorCode::InputDeliveryUnknown),
        Just(ErrorCode::InputLeaseHeld),
        Just(ErrorCode::InternalError),
    ]
}

fn arb_paste_event() -> impl Strategy<Value = PasteEvent> {
    (
        prop_oneof![Just(PasteTrust::Trusted), Just(PasteTrust::Untrusted)],
        proptest::collection::vec(any::<u8>(), 0..64),
    )
        .prop_map(|(trust, data)| PasteEvent { trust, data })
}

/// VT byte stream, capped at 4 KiB for test speed. Empty payloads are
/// legal — `TERMINAL_OUTPUT` carries whatever the PTY produced, including
/// zero bytes (which the rate-limiter just won't emit, but the codec
/// must round-trip).
fn arb_vt_bytes() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..4096)
}

proptest! {
    /// Encoding then decoding any supported `FrameKind` is the identity.
    #[test]
    fn roundtrip_frame_kind(frame in arb_frame_kind()) {
        assert_round_trip(&frame);
    }

    /// Decoding never panics on arbitrary byte input. The result is either
    /// a successful parse (rare but possible by luck) or a `DecodeError`.
    #[test]
    fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let _ = Decoder::new(&bytes).read_frame();
    }

    #[test]
    fn roundtrip_pane_output(
        terminal_id in arb_terminal_id(),
        seq in any::<u64>(),
        bytes in arb_vt_bytes(),
    ) {
        assert_round_trip(&FrameKind::TerminalOutput { terminal_id, seq, bytes: bytes.into() });
    }

    #[test]
    fn roundtrip_pane_snapshot(
        terminal_id in arb_terminal_id(),
        cols in any::<u16>(),
        rows in any::<u16>(),
        vt_replay_bytes in arb_vt_bytes(),
        scrollback_bytes in proptest::option::of(arb_vt_bytes()),
    ) {
        assert_round_trip(&FrameKind::TerminalSnapshot {
            terminal_id,
            cols,
            rows,
            vt_replay_bytes,
            scrollback_bytes,
        });
    }
}

/// HELLO round-trips across every capability shape a client can advertise:
/// the default caps, each `ColorSupport` variant, image/kbd/hyperlink
/// combinations, `OutputMode::StateSync` (phux-fseo), and outer terminal
/// default colors.
#[test]
fn hello_round_trips_across_capability_shapes() {
    let mut shapes = vec![
        ClientCapabilities::default(),
        ClientCapabilities::new()
            .with_image_protocols(ImageProtocolSet::with(&[ImageProtocol::Sixel]))
            .with_kbd_protocols(KeyboardProtocolSet::with(&[KeyboardProtocol::Kitty]))
            .with_hyperlinks(false),
        ClientCapabilities::new().with_output_mode(OutputMode::StateSync),
        ClientCapabilities::new().with_default_colors(TerminalDefaultColors {
            foreground: TerminalColor {
                r: 0xd0,
                g: 0xd0,
                b: 0xd0,
            },
            background: TerminalColor {
                r: 0x12,
                g: 0x18,
                b: 0x1b,
            },
        }),
    ];
    for color in [
        ColorSupport::TrueColor,
        ColorSupport::Indexed256,
        ColorSupport::Indexed16,
        ColorSupport::Mono,
    ] {
        shapes.push(ClientCapabilities::new().with_color_support(color));
    }
    for client_caps in shapes {
        assert_round_trip(&FrameKind::Hello {
            client_name: "phux-client".to_owned(),
            protocol_major: 0,
            protocol_minor: 2,
            protocol_patch: 0,
            client_caps,
        });
    }
}

/// A truncated `HELLO_OK` (version only, no trailing caps / `server_id` —
/// the shape a pre-capabilities server might emit) must still decode,
/// falling back to `ServerCapabilities::default()` (L1) and an empty
/// `server_id` per the SPEC §6 "skip them by length" rule.
#[test]
fn hello_ok_round_trip_version_only_trailing_defaults() {
    // Forward-compat under TLV: a HELLO_OK carrying only the version-triple
    // fields (SERVER_CAPS and SERVER_ID fields absent) decodes with
    // ServerCapabilities::default() and an empty server_id.
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &0u16.to_be_bytes()); // PROTOCOL_MAJOR
    tlv_field(&mut fields, 2, &2u16.to_be_bytes()); // PROTOCOL_MINOR
    tlv_field(&mut fields, 3, &0u16.to_be_bytes()); // PROTOCOL_PATCH
    let framed = framed_tlv(0x80, &fields);

    let (decoded, tail) = FrameKind::decode(&framed).unwrap();
    assert_eq!(
        decoded,
        FrameKind::HelloOk {
            protocol_major: 0,
            protocol_minor: 2,
            protocol_patch: 0,
            server_caps: ServerCapabilities::default(),
            server_id: Vec::new(),
        }
    );
    assert!(tail.is_empty());
}

#[test]
fn hello_decoder_defaults_output_mode_raw_when_absent() {
    // A CLIENT_CAPS field (id 5) whose blob stops before the output_mode byte
    // (a pre-fseo client encodes only the first five caps bytes) decodes to the
    // safe interactive default, OutputMode::Raw.
    let caps_blob = [
        ColorSupport::TrueColor.as_wire(),
        LayerSet::new().as_wire(),
        ImageProtocolSet::default().as_wire(),
        KeyboardProtocolSet::default().as_wire(),
        1u8, // hyperlinks = true (ClientCapabilities::new default)
             // no output_mode byte
    ];
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, b"x"); // CLIENT_NAME
    tlv_field(&mut fields, 2, &0u16.to_be_bytes());
    tlv_field(&mut fields, 3, &2u16.to_be_bytes());
    tlv_field(&mut fields, 4, &0u16.to_be_bytes());
    tlv_field(&mut fields, 5, &caps_blob);
    let buf = framed_tlv(0x01, &fields);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert!(tail.is_empty());
    let FrameKind::Hello { client_caps, .. } = decoded else {
        panic!("expected Hello");
    };
    assert_eq!(client_caps.output_mode, OutputMode::Raw);
    assert_eq!(client_caps.default_colors, None);
}

#[test]
fn hello_decoder_accepts_legacy_body_without_caps() {
    // Forward-compat under TLV: a HELLO whose CLIENT_CAPS field (id 5) is
    // simply absent decodes with ClientCapabilities::default() — the
    // field-tagged counterpart of the old "shorter positional body, trailing
    // caps default" rule. Only the version-triple fields plus client_name are
    // present.
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, b"x"); // CLIENT_NAME
    tlv_field(&mut fields, 2, &0u16.to_be_bytes()); // PROTOCOL_MAJOR
    tlv_field(&mut fields, 3, &1u16.to_be_bytes()); // PROTOCOL_MINOR
    tlv_field(&mut fields, 4, &0u16.to_be_bytes()); // PROTOCOL_PATCH
    let framed = framed_tlv(0x01, &fields);
    let (decoded, tail) = FrameKind::decode(&framed).unwrap();
    assert!(tail.is_empty());
    match decoded {
        FrameKind::Hello {
            client_caps,
            client_name,
            ..
        } => {
            assert_eq!(client_name, "x");
            // Absent caps field defaults to TrueColor.
            assert_eq!(client_caps.color_support, ColorSupport::TrueColor);
        }
        other => panic!("expected Hello, got {other:?}"),
    }
}

#[test]
fn hello_decoder_treats_unknown_color_support_tag_as_truecolor() {
    // A CLIENT_CAPS field (id 5) whose first (color_support) byte is an unknown
    // tag (0xFF) maps to TrueColor per the `#[non_exhaustive]` contract.
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, b"x"); // CLIENT_NAME
    tlv_field(&mut fields, 2, &0u16.to_be_bytes()); // PROTOCOL_MAJOR
    tlv_field(&mut fields, 3, &1u16.to_be_bytes()); // PROTOCOL_MINOR
    tlv_field(&mut fields, 4, &0u16.to_be_bytes()); // PROTOCOL_PATCH
    tlv_field(&mut fields, 5, &[0xFF]); // CLIENT_CAPS: unknown color_support tag
    let framed = framed_tlv(0x01, &fields);
    let (decoded, _) = FrameKind::decode(&framed).unwrap();
    match decoded {
        FrameKind::Hello { client_caps, .. } => {
            assert_eq!(client_caps.color_support, ColorSupport::TrueColor);
        }
        other => panic!("expected Hello, got {other:?}"),
    }
}

/// Fixed-value fixtures for the simple frames: `HELLO_OK`, PING, DETACH /
/// DETACHED, `TERMINAL_OUTPUT`, and both `TERMINAL_SNAPSHOT` shapes.
#[test]
fn fixed_frames_round_trip() {
    for frame in [
        FrameKind::HelloOk {
            protocol_major: 0,
            protocol_minor: 2,
            protocol_patch: 0,
            server_caps: ServerCapabilities::new().with_layers(LayerSet::all()),
            server_id: vec![0xDE, 0xAD, 0xBE, 0xEF],
        },
        FrameKind::Ping {
            nonce: 0xDEAD_BEEF_CAFE_F00D,
        },
        FrameKind::Detach,
        FrameKind::Detached,
        FrameKind::TerminalOutput {
            terminal_id: TerminalId::local(1),
            seq: 0,
            bytes: bytes::Bytes::from_static(b"hello world\r\n"),
        },
        FrameKind::TerminalSnapshot {
            terminal_id: TerminalId::new(100),
            cols: 80,
            rows: 24,
            vt_replay_bytes: b"\x1b[!p\x1b[2J\x1b[H".to_vec(),
            scrollback_bytes: None,
        },
        FrameKind::TerminalSnapshot {
            terminal_id: TerminalId::new(100),
            cols: 80,
            rows: 24,
            vt_replay_bytes: b"vt".to_vec(),
            scrollback_bytes: Some(b"sb".to_vec()),
        },
    ] {
        assert_round_trip(&frame);
    }
}

#[test]
fn truncated_length_header_is_eof() {
    let bytes = [0u8, 0, 0];
    let err = Decoder::new(&bytes).read_frame().unwrap_err();
    assert_eq!(err, DecodeError::UnexpectedEof);
}

#[test]
fn zero_length_is_rejected() {
    let bytes = [0u8, 0, 0, 0];
    let err = Decoder::new(&bytes).read_frame().unwrap_err();
    assert_eq!(err, DecodeError::LengthOverflow);
}

#[test]
fn length_exceeds_protocol_cap() {
    let mut bytes = vec![];
    bytes.extend_from_slice(&0x0200_0000u32.to_be_bytes());
    bytes.push(0x7F);
    let err = Decoder::new(&bytes).read_frame().unwrap_err();
    assert_eq!(err, DecodeError::LengthOverflow);
}

#[test]
fn length_exceeds_buffer() {
    let mut bytes = vec![];
    bytes.extend_from_slice(&100u32.to_be_bytes());
    bytes.push(0x7F);
    let err = Decoder::new(&bytes).read_frame().unwrap_err();
    assert_eq!(err, DecodeError::UnexpectedEof);
}

#[test]
fn unknown_frame_kind_is_rejected() {
    let mut bytes = vec![];
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.push(0x42);
    let err = Decoder::new(&bytes).read_frame().unwrap_err();
    assert_eq!(err, DecodeError::UnknownFrameKind { tag: 0x42 });
}

#[test]
fn unknown_field_id_is_skipped_forward_compat() {
    // Forward-compat (`docs/spec/appendix-encoding.md`): a decoder MUST skip a
    // field id it does not recognise, by that field's declared length, and
    // decode the rest of the message normally. Encode a real PING (nonce field
    // id 1), then splice in an unknown field id (99) carrying junk *before* the
    // known field; the nonce must still decode and the unknown field is ignored.
    let real = {
        let mut buf = BytesMut::new();
        FrameKind::Ping {
            nonce: 0x0102_0304_0506_0708,
        }
        .encode(&mut buf);
        buf.to_vec()
    };
    // Reconstruct the body with an extra unknown field prepended.
    let type_byte = real[4];
    let known_fields = &real[5..]; // the PING nonce field
    let mut fields = Vec::new();
    tlv_field(&mut fields, 99, &[0xDE, 0xAD, 0xBE, 0xEF]); // unknown field, skipped
    fields.extend_from_slice(known_fields);
    let bytes = framed_tlv(type_byte, &fields);

    let (decoded, tail) = FrameKind::decode(&bytes).unwrap();
    assert_eq!(
        decoded,
        FrameKind::Ping {
            nonce: 0x0102_0304_0506_0708,
        },
        "the unknown field must be skipped and the known field still decode",
    );
    assert!(tail.is_empty());
}

#[test]
fn unknown_trailing_field_id_is_skipped_forward_compat() {
    // The same skip-by-length rule for an unknown field appended *after* the
    // known fields — the shape a newer peer produces when it adds a field an
    // older decoder does not know.
    let real = {
        let mut buf = BytesMut::new();
        FrameKind::Bell {
            terminal_id: TerminalId::local(0x2A),
        }
        .encode(&mut buf);
        buf.to_vec()
    };
    let type_byte = real[4];
    let mut fields = real[5..].to_vec(); // the Bell terminal_id field
    tlv_field(&mut fields, 250, &[1, 2, 3, 4, 5, 6]); // unknown trailing field
    let bytes = framed_tlv(type_byte, &fields);

    let (decoded, tail) = FrameKind::decode(&bytes).unwrap();
    assert_eq!(
        decoded,
        FrameKind::Bell {
            terminal_id: TerminalId::local(0x2A),
        },
    );
    assert!(tail.is_empty());
}

#[test]
fn retired_pane_diff_discriminant_is_rejected() {
    // The pre-ADR-0013 `PANE_DIFF` discriminant (0x40) is no longer
    // recognised. A frame carrying it must surface as UnknownFrameKind.
    let mut body = vec![0x40u8];
    // Pad some plausible-looking diff bytes; doesn't matter, decoder
    // refuses on the type byte.
    body.extend_from_slice(&[0u8; 8]);
    let mut bytes = vec![];
    bytes.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(&body);
    let err = FrameKind::decode(&bytes).unwrap_err();
    assert_eq!(err, DecodeError::UnknownFrameKind { tag: 0x40 });
}

#[test]
fn invalid_utf8_in_hello_client_name() {
    // HELLO (0x01) whose CLIENT_NAME field (id 1) holds non-UTF-8 bytes must
    // surface InvalidUtf8. The client_name value rides as raw bytes inside the
    // length-delimited field (no inner length prefix under TLV).
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &[0xFFu8, 0xFE, 0xFD]); // field::hello::CLIENT_NAME
    let bytes = framed_tlv(0x01, &fields);

    let err = Decoder::new(&bytes).read_frame().unwrap_err();
    assert_eq!(err, DecodeError::InvalidUtf8);
}

#[test]
fn truncated_ping_body() {
    let mut bytes = vec![];
    bytes.extend_from_slice(&9u32.to_be_bytes());
    bytes.push(0x7F);
    bytes.extend_from_slice(&[0, 0, 0]);
    let err = Decoder::new(&bytes).read_frame().unwrap_err();
    assert_eq!(err, DecodeError::UnexpectedEof);
}

#[test]
fn tail_is_returned_after_single_frame() {
    let frame = FrameKind::Ping { nonce: 7 };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.extend_from_slice(&[0xAA, 0xBB, 0xCC]);

    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert_eq!(tail, &[0xAA, 0xBB, 0xCC]);
}

// -----------------------------------------------------------------------------
// SPEC §13 conformance: ATTACH / ATTACHED / TERMINAL_SNAPSHOT envelope.
// -----------------------------------------------------------------------------

proptest! {
    #[test]
    fn roundtrip_attach_full(
        target in arb_attach_target(),
        viewport in arb_viewport_info(),
        request_scrollback in any::<bool>(),
        scrollback_limit_lines in any::<u32>(),
    ) {
        assert_round_trip(&FrameKind::Attach {
            target,
            viewport,
            request_scrollback,
            scrollback_limit_lines,
        });
    }

    #[test]
    fn roundtrip_input_key(terminal_id in arb_terminal_id(), event in arb_key_event()) {
        assert_round_trip(&FrameKind::InputKey { terminal_id, event });
    }

    #[test]
    fn roundtrip_input_mouse(terminal_id in arb_terminal_id(), event in arb_mouse_event()) {
        assert_round_trip(&FrameKind::InputMouse { terminal_id, event });
    }

    #[test]
    fn roundtrip_input_focus(terminal_id in arb_terminal_id(), event in arb_focus_event()) {
        assert_round_trip(&FrameKind::InputFocus { terminal_id, event });
    }

    #[test]
    fn roundtrip_input_paste(terminal_id in arb_terminal_id(), event in arb_paste_event()) {
        assert_round_trip(&FrameKind::InputPaste { terminal_id, event });
    }

    #[test]
    fn roundtrip_session_info(info in arb_session_info()) {
        let snap = SessionSnapshot::new(info.id, WindowId::new(0), TerminalId::new(0))
            .with_sessions(vec![info]);
        assert_round_trip(&FrameKind::Attached {
            snapshot: snap,
            initial_client_id: ClientId::new(0),
        });
    }

    #[test]
    fn roundtrip_window_info(info in arb_window_info()) {
        let snap = SessionSnapshot::new(info.session_id, info.id, TerminalId::new(0))
            .with_windows(vec![info]);
        assert_round_trip(&FrameKind::Attached {
            snapshot: snap,
            initial_client_id: ClientId::new(0),
        });
    }

    #[test]
    fn roundtrip_pane_info(info in arb_pane_info()) {
        let snap = SessionSnapshot::new(SessionId::new(0), info.window_id, info.id.clone())
            .with_panes(vec![info]);
        assert_round_trip(&FrameKind::Attached {
            snapshot: snap,
            initial_client_id: ClientId::new(0),
        });
    }

    #[test]
    fn roundtrip_layout_node(layout in arb_layout_node()) {
        let win = WindowInfo::new(WindowId::new(1), SessionId::new(1), "w")
            .with_layout(Some(layout));
        let snap = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), TerminalId::new(0))
            .with_windows(vec![win]);
        assert_round_trip(&FrameKind::Attached {
            snapshot: snap,
            initial_client_id: ClientId::new(0),
        });
    }

    #[test]
    fn roundtrip_attached(
        snapshot in arb_session_snapshot(),
        client_id in any::<u32>(),
    ) {
        assert_round_trip(&FrameKind::Attached {
            snapshot,
            initial_client_id: ClientId::new(client_id),
        });
    }

    #[test]
    fn roundtrip_bell(terminal_id in arb_terminal_id()) {
        assert_round_trip(&FrameKind::Bell { terminal_id });
    }

    #[test]
    fn roundtrip_error(
        request_id in proptest::option::of(any::<u32>()),
        code in arb_error_code(),
        message in ".{0,256}",
    ) {
        assert_round_trip(&FrameKind::Error { request_id, code, message });
    }

    #[test]
    fn roundtrip_viewport_resize(viewport in arb_viewport_info()) {
        assert_round_trip(&FrameKind::ViewportResize { viewport });
    }

    #[test]
    fn roundtrip_frame_ack(terminal_id in arb_terminal_id(), seq in any::<u64>()) {
        assert_round_trip(&FrameKind::FrameAck { terminal_id, seq });
    }
}

#[test]
fn attach_unknown_target_tag_is_rejected() {
    // ATTACH (0x02) carrying a TARGET field (id 1) whose value is an
    // AttachTarget with an unknown tag byte (0xFF) must surface
    // UnknownEnumValue from the nested positional decoder.
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &[0xFF]); // field::attach::TARGET
    let bytes = framed_tlv(0x02, &fields);
    let err = FrameKind::decode(&bytes).unwrap_err();
    assert_eq!(
        err,
        DecodeError::UnknownEnumValue {
            field: "AttachTarget",
            value: 0xFF,
        }
    );
}

#[test]
fn input_focus_unknown_kind_is_rejected() {
    // INPUT_FOCUS (0x14): TERMINAL_ID (id 1) = local{0}, then an EVENT field
    // (id 2) carrying an unknown focus-kind byte (0xAB).
    let mut term = vec![0x00u8]; // TERMINAL_ID_TAG_LOCAL
    term.extend_from_slice(&0u32.to_be_bytes());
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &term); // field::input_focus::TERMINAL_ID
    tlv_field(&mut fields, 2, &[0xAB]); // field::input_focus::EVENT
    let bytes = framed_tlv(0x14, &fields);

    let err = FrameKind::decode(&bytes).unwrap_err();
    assert_eq!(
        err,
        DecodeError::UnknownEnumValue {
            field: "FocusEvent",
            value: 0xAB,
        }
    );
}

#[test]
fn error_unknown_code_is_rejected() {
    // A TYPE_ERROR (0xC1) frame whose CODE field (id 2) carries a code the
    // v0.1 decoder does not recognise MUST surface UnknownEnumValue rather
    // than silently mapping to a placeholder variant. (request_id is omitted
    // — an absent optional field.)
    let mut fields = Vec::new();
    tlv_field(&mut fields, 2, &0x9999u16.to_be_bytes()); // field::error::CODE
    tlv_field(&mut fields, 3, b""); // field::error::MESSAGE (empty)
    let bytes = framed_tlv(0xC1, &fields);

    let err = FrameKind::decode(&bytes).unwrap_err();
    assert_eq!(
        err,
        DecodeError::UnknownEnumValue {
            field: "ErrorCode",
            value: 0x9999,
        }
    );
}

#[test]
fn error_code_wire_values_match_spec() {
    // SPEC §14 names these wire values; lock them in so a refactor cannot
    // silently renumber the enum.
    assert_eq!(ErrorCode::VersionIncompatible.as_wire(), 1);
    assert_eq!(ErrorCode::UnknownMessageType.as_wire(), 2);
    assert_eq!(ErrorCode::MalformedMessage.as_wire(), 3);
    assert_eq!(ErrorCode::FrameTooLarge.as_wire(), 4);
    assert_eq!(ErrorCode::NotAttached.as_wire(), 100);
    assert_eq!(ErrorCode::AlreadyAttached.as_wire(), 101);
    assert_eq!(ErrorCode::SessionNotFound.as_wire(), 102);
    assert_eq!(ErrorCode::WindowNotFound.as_wire(), 103);
    assert_eq!(ErrorCode::TerminalNotFound.as_wire(), 104);
    assert_eq!(ErrorCode::ClientNotFound.as_wire(), 105);
    assert_eq!(ErrorCode::UnsupportedSatelliteRoute.as_wire(), 106);
    assert_eq!(ErrorCode::SatelliteUnreachable.as_wire(), 107);
    assert_eq!(ErrorCode::InvalidCommand.as_wire(), 200);
    assert_eq!(ErrorCode::PermissionDenied.as_wire(), 201);
    assert_eq!(ErrorCode::ResourceExhausted.as_wire(), 202);
    assert_eq!(ErrorCode::UnsafePaste.as_wire(), 203);
    assert_eq!(ErrorCode::InputLeaseHeld.as_wire(), 204);
    assert_eq!(ErrorCode::InputDeliveryUnknown.as_wire(), 205);
    assert_eq!(ErrorCode::InternalError.as_wire(), 65535);
}

// -----------------------------------------------------------------------------
// Layout ratio validation — SPEC §13 leaves bounds implicit; phux rejects
// NaN, infinite, and out-of-range values on decode.
// -----------------------------------------------------------------------------

fn encode_split_with_ratio(ratio: f32) -> Vec<u8> {
    // Positional LayoutNode bytes: a Split carrying `ratio` over two local
    // leaves; `common::attached_with_layout` wraps them in the hand-rolled
    // ATTACHED frame (whose SessionSnapshot value stays positional under TLV).
    let mut layout = vec![1u8]; // LayoutNode::Split
    layout.push(0); // SplitDir::Horizontal
    layout.extend_from_slice(&ratio.to_be_bytes());
    for leaf_id in [1u32, 2] {
        layout.push(0); // LAYOUT_TAG_LEAF
        layout.push(0); // TERMINAL_ID_TAG_LOCAL
        layout.extend_from_slice(&leaf_id.to_be_bytes());
    }
    attached_with_layout(&layout)
}

/// The layout-ratio bounds table: NaN, infinite, and out-of-[0, 1] ratios are
/// rejected with `MalformedLayoutRatio`; the inclusive endpoints decode.
/// Bit-level comparison covers NaN uniformly.
#[test]
fn layout_ratio_bounds_are_enforced_on_decode() {
    for (ratio, accepted) in [
        (f32::NAN, false),
        (f32::INFINITY, false),
        (1.5, false),
        (-0.1, false),
        (0.0, true),
        (1.0, true),
    ] {
        let bytes = encode_split_with_ratio(ratio);
        if accepted {
            let (decoded, _tail) = FrameKind::decode(&bytes).unwrap();
            let FrameKind::Attached { snapshot, .. } = decoded else {
                panic!("expected Attached frame for ratio {ratio}");
            };
            match snapshot.windows[0].layout.as_ref().unwrap() {
                LayoutNode::Split { ratio: got, .. } => {
                    assert_eq!(got.to_bits(), ratio.to_bits());
                }
                other => panic!("expected Split, got {other:?}"),
            }
        } else {
            match FrameKind::decode(&bytes).unwrap_err() {
                DecodeError::MalformedLayoutRatio { ratio: got } => {
                    assert_eq!(got.to_bits(), ratio.to_bits());
                }
                other => panic!("expected MalformedLayoutRatio, got {other:?}"),
            }
        }
    }
}

// -----------------------------------------------------------------------------
// L3 metadata frames — SPEC §7.4 / §11.L3 (phux-4li.2).
// -----------------------------------------------------------------------------

fn arb_scope() -> impl Strategy<Value = Scope> {
    prop_oneof![
        arb_terminal_id().prop_map(Scope::Terminal),
        any::<u32>().prop_map(|id| Scope::Group(GroupId::new(id))),
        Just(Scope::Global),
    ]
}

fn arb_metadata_value() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..512)
}

fn arb_layer_set() -> impl Strategy<Value = LayerSet> {
    prop_oneof![
        Just(LayerSet::new()),
        Just(LayerSet::with(&[Layer::L2])),
        Just(LayerSet::with(&[Layer::L3])),
        Just(LayerSet::all()),
    ]
}

proptest! {
    #[test]
    fn roundtrip_get_metadata(
        request_id in any::<u32>(),
        scope in arb_scope(),
        key in ".{0,64}",
    ) {
        assert_round_trip(&FrameKind::GetMetadata { request_id, scope, key });
    }

    #[test]
    fn roundtrip_set_metadata(
        request_id in any::<u32>(),
        scope in arb_scope(),
        key in ".{0,64}",
        value in arb_metadata_value(),
    ) {
        assert_round_trip(&FrameKind::SetMetadata { request_id, scope, key, value });
    }

    #[test]
    fn roundtrip_delete_metadata(
        request_id in any::<u32>(),
        scope in arb_scope(),
        key in ".{0,64}",
    ) {
        assert_round_trip(&FrameKind::DeleteMetadata { request_id, scope, key });
    }

    #[test]
    fn roundtrip_list_metadata(
        request_id in any::<u32>(),
        scope in arb_scope(),
    ) {
        assert_round_trip(&FrameKind::ListMetadata { request_id, scope });
    }

    #[test]
    fn roundtrip_subscribe_metadata(
        scope in arb_scope(),
        key in ".{0,64}",
    ) {
        assert_round_trip(&FrameKind::SubscribeMetadata { scope, key });
    }

    #[test]
    fn roundtrip_metadata_changed(
        scope in arb_scope(),
        key in ".{0,64}",
        value in proptest::option::of(arb_metadata_value()),
    ) {
        assert_round_trip(&FrameKind::MetadataChanged { scope, key, value });
    }

    /// METADATA_VALUE — reply to GET_METADATA (phux-4li.8). Carries the
    /// request_id verbatim and an optional value (None = key absent).
    #[test]
    fn roundtrip_metadata_value(
        request_id in any::<u32>(),
        value in proptest::option::of(arb_metadata_value()),
    ) {
        assert_round_trip(&FrameKind::MetadataValue { request_id, value });
    }

    /// METADATA_KEYS — reply to LIST_METADATA (phux-4li.8). Carries the
    /// request_id verbatim and a (possibly empty) list of key names.
    #[test]
    fn roundtrip_metadata_keys(
        request_id in any::<u32>(),
        keys in proptest::collection::vec(".{0,32}", 0..8),
    ) {
        assert_round_trip(&FrameKind::MetadataKeys { request_id, keys });
    }

    /// HELLO carries `client_caps.layers` as a trailing byte (phux-4li.2).
    /// The encoder always emits it; the decoder accepts every prefix shape
    /// per SPEC §6.
    #[test]
    fn roundtrip_hello_layers(
        layers in arb_layer_set(),
    ) {
        assert_round_trip(&FrameKind::Hello {
            client_name: "phux-client/test".to_owned(),
            protocol_major: 0,
            protocol_minor: 2,
            protocol_patch: 0,
            client_caps: ClientCapabilities::new()
                .with_color_support(ColorSupport::TrueColor)
                .with_layers(layers),
        });
    }
}

#[test]
fn hello_decoder_accepts_legacy_body_with_color_but_no_layers() {
    // A 7lf-era HELLO ends right after the ColorSupport byte; a 4li.2+
    // decoder must accept it and substitute the default LayerSet.
    // A CLIENT_CAPS field (id 5) carrying only the color_support byte (no
    // layers byte) decodes with L1 implied and no L3.
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, b"x"); // CLIENT_NAME
    tlv_field(&mut fields, 2, &0u16.to_be_bytes());
    tlv_field(&mut fields, 3, &2u16.to_be_bytes());
    tlv_field(&mut fields, 4, &0u16.to_be_bytes());
    tlv_field(&mut fields, 5, &[0x00]); // CLIENT_CAPS: ColorSupport::TrueColor; no layers
    let framed = framed_tlv(0x01, &fields);
    let (decoded, tail) = FrameKind::decode(&framed).unwrap();
    assert!(tail.is_empty());
    match decoded {
        FrameKind::Hello { client_caps, .. } => {
            // L1 always implied even when the byte is missing.
            assert!(client_caps.layers.contains(Layer::L1));
            assert!(!client_caps.layers.contains(Layer::L3));
        }
        other => panic!("expected Hello, got {other:?}"),
    }
}

#[test]
fn layer_set_wire_round_trips() {
    for ls in [
        LayerSet::new(),
        LayerSet::with(&[Layer::L2]),
        LayerSet::with(&[Layer::L3]),
        LayerSet::all(),
    ] {
        let wire = ls.as_wire();
        let back = LayerSet::from_wire(wire);
        assert_eq!(back, ls);
        // L1 invariant: always set after round-trip.
        assert!(back.contains(Layer::L1));
    }
}

#[test]
fn layer_set_unknown_bits_are_dropped_but_l1_forced_on() {
    // A future encoder sets a yet-unknown bit (0x80) plus L3.
    let ls = LayerSet::from_wire(0x80 | 0x04);
    assert!(ls.contains(Layer::L1));
    assert!(ls.contains(Layer::L3));
    assert!(!ls.contains(Layer::L2));
}

#[test]
fn scope_unknown_tag_is_rejected() {
    // A SET_METADATA whose SCOPE field (id 2) carries an unknown Scope tag must
    // surface UnknownEnumValue, not silently coerce.
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &0u32.to_be_bytes()); // field::set_metadata::REQUEST_ID
    tlv_field(&mut fields, 2, &[0xFE]); // field::set_metadata::SCOPE (unknown tag)
    let bytes = framed_tlv(phux_protocol::wire::frame::TYPE_SET_METADATA, &fields);

    let err = FrameKind::decode(&bytes).unwrap_err();
    assert_eq!(
        err,
        DecodeError::UnknownEnumValue {
            field: "Scope",
            value: 0xFE,
        }
    );
}

// -----------------------------------------------------------------------------
// L1 Terminal lifecycle frames — SPEC §7.2 / §10.1 (phux-4li.10).
//
// Wire substrate for split-pane / kill-pane (phux-4li.5) and post-SIGWINCH
// per-pane `ioctl(TIOCSWINSZ)` (phux-4li.9). Server-side handler + client-
// side emission land in follow-up tickets; the codec lands here.
// -----------------------------------------------------------------------------

fn arb_env_pair() -> impl Strategy<Value = (String, String)> {
    (".{0,16}", ".{0,32}")
}

fn arb_spawn_error() -> impl Strategy<Value = SpawnError> {
    prop_oneof![
        Just(SpawnError::GroupNotFound),
        ".{0,128}".prop_map(SpawnError::SpawnFailed),
        Just(SpawnError::UnsupportedSatelliteRoute),
        ".{0,128}".prop_map(SpawnError::SatelliteUnreachable),
    ]
}

fn arb_spawn_result() -> impl Strategy<Value = SpawnResult> {
    prop_oneof![
        arb_terminal_id().prop_map(SpawnResult::Ok),
        arb_spawn_error().prop_map(SpawnResult::Err),
    ]
}

fn arb_move_result() -> impl Strategy<Value = MoveResult> {
    prop_oneof![
        arb_terminal_id().prop_map(MoveResult::Ok),
        ".{0,64}".prop_map(|msg| MoveResult::Err(MoveError::MoveFailed(msg))),
        Just(MoveResult::Err(MoveError::UnsupportedSatelliteRoute)),
    ]
}

proptest! {
    /// The `Some(vec![])` shapes matter here: `command = Some(vec![])` is
    /// distinct from `command = None`, and `env = Some(vec![])` ("start with
    /// empty environment") is distinct from `env = None` ("inherit server's
    /// env") — the 0..4 collection bounds and `option::of` wrappers keep all
    /// of those in the generated space, as are `term` = None / Some("").
    #[test]
    fn roundtrip_spawn_terminal(
        request_id in any::<u32>(),
        group in any::<u32>(),
        command in proptest::option::of(proptest::collection::vec(".{0,16}", 0..4)),
        cwd in proptest::option::of(".{0,32}"),
        env in proptest::option::of(proptest::collection::vec(arb_env_pair(), 0..4)),
        term in proptest::option::of(".{0,16}"),
        satellite in proptest::option::of(".{0,16}"),
        owner_terminal in proptest::option::of(any::<u32>()),
        agent_session in proptest::option::of(proptest::collection::vec(any::<u8>(), 0..64)),
    ) {
        assert_round_trip(&FrameKind::SpawnTerminal {
            request_id,
            group: GroupId::new(group),
            command,
            cwd,
            env,
            term,
            satellite: satellite.map(phux_protocol::ids::SatelliteHost::new),
            owner_terminal: owner_terminal.map(TerminalId::local),
            agent_session,
        });
    }

    #[test]
    fn roundtrip_terminal_spawned(
        request_id in any::<u32>(),
        result in arb_spawn_result(),
    ) {
        assert_round_trip(&FrameKind::TerminalSpawned { request_id, result });
    }

    /// MOVE_TERMINAL / TERMINAL_MOVED (ADR-0056): both TerminalId fields
    /// are required, and the reply's tagged union mirrors SpawnResult.
    #[test]
    fn roundtrip_move_terminal(
        request_id in any::<u32>(),
        terminal in arb_terminal_id(),
        owner_terminal in arb_terminal_id(),
    ) {
        assert_round_trip(&FrameKind::MoveTerminal { request_id, terminal, owner_terminal });
    }

    #[test]
    fn roundtrip_terminal_moved(
        request_id in any::<u32>(),
        result in arb_move_result(),
    ) {
        assert_round_trip(&FrameKind::TerminalMoved { request_id, result });
    }

    /// `exit_status = None` is the wire encoding for "killed by signal /
    /// unknown cause"; negative statuses ride as u32 two's-complement. Both
    /// live inside `option::of(any::<i32>())`.
    #[test]
    fn roundtrip_terminal_closed(
        terminal_id in arb_terminal_id(),
        exit_status in proptest::option::of(any::<i32>()),
    ) {
        assert_round_trip(&FrameKind::TerminalClosed { terminal_id, exit_status });
    }

    /// Zero dims are in-range: SPEC §10.2 leaves them implementation-defined
    /// and the codec round-trips them faithfully.
    #[test]
    fn roundtrip_terminal_resize(
        terminal_id in arb_terminal_id(),
        cols in any::<u16>(),
        rows in any::<u16>(),
        pixel_width in proptest::option::of(any::<u16>()),
        pixel_height in proptest::option::of(any::<u16>()),
    ) {
        assert_round_trip(&FrameKind::TerminalResize {
            terminal_id,
            cols,
            rows,
            pixel_width,
            pixel_height,
        });
    }

    #[test]
    fn roundtrip_command_kill_terminal(
        request_id in any::<u32>(),
        terminal_id in arb_terminal_id(),
    ) {
        assert_round_trip(&FrameKind::Command {
            request_id,
            command: Command::KillTerminal { terminal_id },
        });
    }

    #[test]
    fn roundtrip_command_result_ok_and_error(
        request_id in any::<u32>(),
        message in ".{0,48}",
    ) {
        for result in [
            CommandResult::Ok,
            CommandResult::Error { code: ErrorCode::InvalidCommand, message },
        ] {
            assert_round_trip(&FrameKind::CommandResult { request_id, result });
        }
    }
}

/// The fixed-payload command verbs share one wire shape (tag + positional
/// body); one looped table covers `GET_STATE`, UPGRADE, `RELEASE_INPUT`
/// (ADR-0033), and `REPORT_ASKED`.
#[test]
fn command_simple_variants_round_trip() {
    for command in [
        Command::GetState {
            scope: StateScope::Server,
        },
        Command::Upgrade,
        Command::ReleaseInput {
            terminal_id: TerminalId::local(7),
        },
        Command::ReportAsked {
            terminal_id: TerminalId::local(7),
            id: "q1".to_owned(),
            question: "Deploy to prod?".to_owned(),
            suggestions: vec!["Yes".to_owned(), "No".to_owned(), "Hold".to_owned()],
            elapsed_seconds: Some(9),
        },
    ] {
        assert_round_trip(&FrameKind::Command {
            request_id: 7,
            command,
        });
    }
}

#[test]
fn command_attach_detach_terminal_round_trip() {
    // phux-v45.7: the per-Terminal subscription verbs (SPEC §5.1 tags
    // 0x01/0x02) round-trip with both Local and Satellite ids — the
    // Satellite form is what a hub consumer sends for two-hop attach.
    for terminal_id in [TerminalId::local(7), TerminalId::satellite("devbox", 7)] {
        for command in [
            Command::AttachTerminal {
                terminal_id: terminal_id.clone(),
            },
            Command::DetachTerminal {
                terminal_id: terminal_id.clone(),
            },
        ] {
            assert_round_trip(&FrameKind::Command {
                request_id: 21,
                command,
            });
        }
    }
}

#[test]
fn command_acquire_input_round_trips() {
    // ADR-0033: both acquisition modes round-trip, with the advisory ttl.
    for mode in [InputMode::Cooperative, InputMode::Seize] {
        assert_round_trip(&FrameKind::Command {
            request_id: 11,
            command: Command::AcquireInput {
                terminal_id: TerminalId::local(7),
                mode,
                ttl_ms: 30_000,
            },
        });
    }
}

#[test]
fn command_signal_terminal_round_trips() {
    // ADR-0033: every signal variant round-trips.
    for signal in [
        TerminalSignal::Interrupt,
        TerminalSignal::Freeze,
        TerminalSignal::Resume,
        TerminalSignal::Terminate,
        TerminalSignal::Kill,
    ] {
        assert_round_trip(&FrameKind::Command {
            request_id: 13,
            command: Command::SignalTerminal {
                terminal_id: TerminalId::local(3),
                signal,
            },
        });
    }
}

#[test]
fn command_get_screen_round_trips() {
    // GET_SCREEN (tag 0x07): TerminalId + a trailing optional<u32>
    // `request_scrollback` (phux-o1v) + a trailing bool `cells` (phux-8yl).
    // The reply is OK_WITH(JSON(..)) — covered by the generic
    // CommandValue::Json roundtrip. Exercise every scrollback state crossed
    // with both `cells` values so the presence byte + value + cells bool
    // round-trip.
    for request_scrollback in [None, Some(0), Some(42)] {
        for cells in [false, true] {
            assert_round_trip(&FrameKind::Command {
                request_id: 11,
                command: Command::GetScreen {
                    terminal_id: TerminalId::local(5),
                    request_scrollback,
                    cells,
                },
            });
        }
    }
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
            terminal_id: TerminalId::local(9),
            request_scrollback: Some(3),
            cells: false,
        },
    };

    // Command::GetScreen positional value, minus the trailing cells byte.
    let mut get_screen = vec![0x07u8]; // COMMAND_TAG_GET_SCREEN
    get_screen.push(0x00); // TERMINAL_ID_TAG_LOCAL
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
    get_screen.push(0x00); // TERMINAL_ID_TAG_LOCAL
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
            terminal_id: TerminalId::local(2),
            request_scrollback: None,
            cells: true,
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
                terminal_id: TerminalId::local(1),
                request_scrollback: None,
                cells: false,
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
    // ROUTE_INPUT (tag 0x08): TerminalId + an InputEvent tagged union.
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
                terminal_id: TerminalId::local(5),
                event,
            },
        });
    }
}

#[test]
fn command_apply_input_round_trips_and_rejects_malformed_payloads() {
    let operation_id = InputOperationId::new([0x5a; 16]).unwrap();
    let frame = FrameKind::Command {
        request_id: 0x0102_0304,
        command: Command::ApplyInput {
            operation_id,
            terminal_id: TerminalId::local(5),
            events: vec![
                InputEvent::Focus(FocusEvent::Gained),
                InputEvent::Paste(PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: b"hello".to_vec(),
                }),
            ],
        },
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    assert_eq!(
        encoded.as_ref(),
        &[
            0x00, 0x00, 0x00, 0x30, 0x31, 0x01, 0x04, 0x04, 0x01, 0x02, 0x03, 0x04, 0x02, 0x04,
            0x25, 0x14, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a,
            0x5a, 0x5a, 0x5a, 0x5a, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x02, 0x02, 0x00, 0x03,
            0x00, 0x00, 0x00, 0x00, 0x05, 0x68, 0x65, 0x6c, 0x6c, 0x6f,
        ],
        "APPLY_INPUT wire bytes are stable"
    );
    let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());

    let id_offset = encoded
        .windows(16)
        .position(|window| window == [0x5a; 16])
        .expect("operation id bytes");

    let mut zero_id = encoded.to_vec();
    zero_id[id_offset..id_offset + 16].fill(0);
    assert_eq!(
        FrameKind::decode(&zero_id).unwrap_err(),
        DecodeError::InvalidInputOperationId
    );

    // Local TerminalId is tag + u32, then u16 count; mutate the first event tag.
    let mut unknown_event = encoded.to_vec();
    unknown_event[id_offset + 16 + 5 + 2] = 0xff;
    assert!(matches!(
        FrameKind::decode(&unknown_event),
        Err(DecodeError::UnknownEnumValue {
            field: "InputEvent",
            value: 0xff,
        })
    ));

    let count_offset = id_offset + 16 + 5;
    let mut over_count = encoded.to_vec();
    over_count[count_offset..count_offset + 2].copy_from_slice(
        &u16::try_from(MAX_APPLY_INPUT_EVENTS + 1)
            .unwrap()
            .to_be_bytes(),
    );
    assert_eq!(
        FrameKind::decode(&over_count).unwrap_err(),
        DecodeError::ApplyInputLimitExceeded
    );

    assert!(matches!(
        FrameKind::decode(&encoded[..id_offset + 8]),
        Err(DecodeError::UnexpectedEof)
    ));

    let oversized = FrameKind::Command {
        request_id: 1,
        command: Command::ApplyInput {
            operation_id,
            terminal_id: TerminalId::local(5),
            events: vec![InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: vec![b'x'; MAX_APPLY_INPUT_COMMAND_BODY],
            })],
        },
    };
    let mut oversized_bytes = BytesMut::new();
    oversized.encode(&mut oversized_bytes);
    assert_eq!(
        FrameKind::decode(&oversized_bytes).unwrap_err(),
        DecodeError::ApplyInputLimitExceeded
    );
}

#[test]
fn command_put_file_and_ack_round_trip_with_limits() {
    let upload_id = FileUploadId::new([0x6b; 16]).unwrap();
    let frame = FrameKind::Command {
        request_id: 0x0a0b_0c0d,
        command: Command::PutFile {
            upload_id,
            terminal_id: TerminalId::satellite("mini", 5),
            extension: "png".to_owned(),
            offset: 4,
            data: b"tail".to_vec(),
            final_chunk: true,
            sha256: Some([0x7c; 32]),
        },
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());

    let ack = FrameKind::CommandResult {
        request_id: 0x0a0b_0c0d,
        result: CommandResult::OkWith(CommandValue::FileUpload(FileUploadAck {
            next_offset: 8,
            path: Some("/home/u/.local/share/phux/uploads/phux-upload-id.png".to_owned()),
        })),
    };
    assert_round_trip(&ack);

    let id_offset = encoded
        .windows(16)
        .position(|window| window == [0x6b; 16])
        .expect("upload id bytes");
    let mut zero_id = encoded.to_vec();
    zero_id[id_offset..id_offset + 16].fill(0);
    assert_eq!(
        FrameKind::decode(&zero_id).unwrap_err(),
        DecodeError::InvalidFileUploadId
    );

    let over_total = FrameKind::Command {
        request_id: 1,
        command: Command::PutFile {
            upload_id,
            terminal_id: TerminalId::local(5),
            extension: "png".to_owned(),
            offset: MAX_FILE_UPLOAD_SIZE,
            data: vec![1],
            final_chunk: false,
            sha256: None,
        },
    };
    let mut over_total_bytes = BytesMut::new();
    over_total.encode(&mut over_total_bytes);
    assert_eq!(
        FrameKind::decode(&over_total_bytes).unwrap_err(),
        DecodeError::FileUploadLimitExceeded
    );

    let over_chunk = FrameKind::Command {
        request_id: 2,
        command: Command::PutFile {
            upload_id,
            terminal_id: TerminalId::local(5),
            extension: "jpg".to_owned(),
            offset: 0,
            data: vec![0; MAX_FILE_UPLOAD_CHUNK + 1],
            final_chunk: false,
            sha256: None,
        },
    };
    let mut over_chunk_bytes = BytesMut::new();
    over_chunk.encode(&mut over_chunk_bytes);
    assert_eq!(
        FrameKind::decode(&over_chunk_bytes).unwrap_err(),
        DecodeError::FileUploadLimitExceeded
    );
}

#[test]
fn hello_ok_server_feature_round_trips_and_old_caps_default_empty() {
    let frame = FrameKind::HelloOk {
        protocol_major: 0,
        protocol_minor: 5,
        protocol_patch: 0,
        server_caps: ServerCapabilities::new()
            .with_layers(LayerSet::all())
            .with_features(ServerFeatureSet::with(&[ServerFeature::AcknowledgedInput])),
        server_id: vec![],
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    assert!(
        encoded
            .windows(5)
            .any(|window| window == [LayerSet::all().as_wire(), 0, 0, 0, 0x10]),
        "server feature bits must trail the one-byte layer set"
    );
    let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());

    let old = ServerCapabilities::new().with_layers(LayerSet::all());
    assert!(old.features.is_empty());
}

#[test]
fn command_kill_terminals_round_trips() {
    // KILL_TERMINALS (tag 0x09, the slot freed by the v0.3.0 "Option B"
    // re-tier that dissolved the L2 lifecycle verbs): a u16-count-prefixed
    // list of tagged TerminalIds. Exercise the empty list, a singleton, and a
    // multi-id group so the count prefix and the per-id tagged encoding both
    // round-trip.
    for ids in [
        Vec::new(),
        vec![TerminalId::local(7)],
        vec![
            TerminalId::local(1),
            TerminalId::local(2),
            TerminalId::satellite("peer-a", 9),
        ],
    ] {
        assert_round_trip(&FrameKind::Command {
            request_id: 31,
            command: Command::KillTerminals { ids },
        });
    }
}

#[test]
fn command_detach_clients_round_trips() {
    // DETACH_CLIENTS (tag 0x13): a presence byte + optional session name.
    // Exercise both the `None` (detach all) and `Some(name)` targeting so the
    // presence byte and the string encoding both round-trip.
    for session in [None, Some("work".to_owned()), Some(String::new())] {
        assert_round_trip(&FrameKind::Command {
            request_id: 44,
            command: Command::DetachClients { session },
        });
    }
}

/// `COMMAND_RESULT`'s `OkWith` payloads share one wire shape; the table
/// covers `GET_SCREEN`'s JSON reply, `GET_STATE`'s snapshot reply (the
/// ATTACHED snapshot shape, so a non-trivial snapshot must survive), and the
/// `TerminalId` reply.
#[test]
fn command_result_ok_with_values_round_trip() {
    let info = SessionInfo::new(SessionId::new(1), "work".to_owned());
    let snap = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), TerminalId::local(1))
        .with_sessions(vec![info]);
    for value in [
        CommandValue::Json(
            r#"{"schema_version":1,"pane":5,"cols":80,"rows":24,"cursor":null,"lines":["$ "]}"#
                .to_owned(),
        ),
        CommandValue::State(snap),
        CommandValue::TerminalId(TerminalId::local(42)),
    ] {
        assert_round_trip(&FrameKind::CommandResult {
            request_id: 12,
            result: CommandResult::OkWith(value),
        });
    }
}

#[test]
fn command_unknown_tag_is_rejected() {
    // A COMMAND frame whose COMMAND field (id 2) carries an unallocated command
    // tag (0x7F) must decode-fail rather than silently coerce.
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &1u32.to_be_bytes()); // field::command::REQUEST_ID
    tlv_field(&mut fields, 2, &[0x7F]); // field::command::COMMAND (unallocated tag)
    let buf = framed_tlv(0x31, &fields);
    let err = FrameKind::decode(&buf).unwrap_err();
    assert!(
        matches!(
            err,
            DecodeError::UnknownEnumValue {
                field: "Command",
                ..
            }
        ),
        "expected UnknownEnumValue for Command, got {err:?}",
    );
}

// -----------------------------------------------------------------------------
// Agent-event frames — SPEC §7.5 (phux-y2t).
// -----------------------------------------------------------------------------

fn arb_agent_event() -> impl Strategy<Value = AgentEvent> {
    prop_oneof![
        Just(AgentEvent::CommandStarted),
        proptest::option::of(any::<i32>())
            .prop_map(|exit_code| AgentEvent::CommandFinished { exit_code }),
        ".{0,128}".prop_map(|title| AgentEvent::TitleChanged { title }),
        Just(AgentEvent::Bell),
        Just(AgentEvent::PaneSpawned),
        proptest::option::of(any::<i32>())
            .prop_map(|exit_status| AgentEvent::PaneClosed { exit_status }),
        Just(AgentEvent::Dirty),
        Just(AgentEvent::Idle),
        // ADR-0033 TerminalControl: exercise the full lifecycle × action
        // space plus both `Option<ClientId>` slots.
        (
            0u8..3,
            proptest::option::of(any::<i32>()),
            proptest::option::of(any::<u32>()),
            0u8..9,
            proptest::option::of(any::<u32>()),
        )
            .prop_map(
                |(lc, exit_status, holder, ac, actor)| AgentEvent::TerminalControl {
                    lifecycle: TerminalLifecycle::from_u8(lc).unwrap(),
                    exit_status,
                    input_holder: holder.map(ClientId::new),
                    action: ControlAction::from_u8(ac).unwrap(),
                    actor: actor.map(ClientId::new),
                }
            ),
        (
            ".{0,64}",
            ".{0,256}",
            proptest::collection::vec(".{0,64}", 0..4),
            proptest::option::of(any::<u64>()),
        )
            .prop_map(
                |(id, question, suggestions, elapsed_seconds)| AgentEvent::Asked {
                    id,
                    question,
                    suggestions,
                    elapsed_seconds,
                }
            ),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `SUBSCRIBE_EVENTS` round-trips for both per-Terminal and
    /// server-scoped (`None`) subscriptions.
    #[test]
    fn roundtrip_subscribe_events(terminal in proptest::option::of(arb_terminal_id())) {
        assert_round_trip(&FrameKind::SubscribeEvents { terminal });
    }

    /// `EVENT` round-trips across the full event taxonomy and both scope
    /// shapes.
    #[test]
    fn roundtrip_event(
        terminal in proptest::option::of(arb_terminal_id()),
        event in arb_agent_event(),
    ) {
        assert_round_trip(&FrameKind::Event { terminal, event });
    }
}

/// Named EVENT fixtures the proptest taxonomy doesn't pin by name: `Asked`
/// with every field populated and with the minimal shape (empty suggestions,
/// no elapsed counter), `CwdChanged` (phux-foz.4, tag 0x0a), and `Unknown` —
/// a relay that decodes an unknown event and re-encodes it MUST produce
/// byte-identical output (lossless passthrough).
#[test]
fn event_fixture_variants_round_trip() {
    for (terminal, event) in [
        (
            None,
            AgentEvent::Asked {
                id: "q-7f3a".to_string(),
                question: "Which transport should the bridge use?".to_string(),
                suggestions: vec![
                    "WebSocket".to_string(),
                    "gRPC".to_string(),
                    "raw TCP".to_string(),
                ],
                elapsed_seconds: Some(42),
            },
        ),
        (
            None,
            AgentEvent::Asked {
                id: "q-0".to_string(),
                question: "Proceed?".to_string(),
                suggestions: Vec::new(),
                elapsed_seconds: None,
            },
        ),
        (
            Some(TerminalId::local(7)),
            AgentEvent::CwdChanged {
                cwd: "/Users/phall/workspace/phux".to_string(),
            },
        ),
        (
            None,
            AgentEvent::Unknown {
                tag: 0x55,
                body: vec![1, 2, 3, 4, 5],
            },
        ),
    ] {
        assert_round_trip(&FrameKind::Event { terminal, event });
    }
}

#[test]
fn event_unknown_tag_decodes_as_unknown_and_skips() {
    // Forward-compat: an EVENT frame whose event tag this version does not
    // know MUST decode as `AgentEvent::Unknown` (preserving the body verbatim)
    // rather than failing the frame parse — so an older client skips a newer
    // server's event kinds cleanly. The terminal scope is an absent field
    // (server-scoped None); the EVENT field (id 2) holds the positional
    // AgentEvent: unknown tag 0x7F + a length-prefixed body.
    let body_bytes = [0xDEu8, 0xAD, 0xBE, 0xEF];
    let mut agent_event = vec![0x7Fu8]; // unknown event tag
    agent_event.extend_from_slice(&u32::try_from(body_bytes.len()).unwrap().to_be_bytes());
    agent_event.extend_from_slice(&body_bytes);
    let mut fields = Vec::new();
    tlv_field(&mut fields, 2, &agent_event); // field::event::EVENT
    let bytes = framed_tlv(0xB3, &fields);

    let (decoded, tail) = FrameKind::decode(&bytes).unwrap();
    assert_eq!(
        decoded,
        FrameKind::Event {
            terminal: None,
            event: AgentEvent::Unknown {
                tag: 0x7F,
                body: body_bytes.to_vec(),
            },
        }
    );
    assert!(tail.is_empty());
}

#[test]
fn event_asked_decodes_as_unknown_for_an_older_decoder() {
    // Forward-compat guard: prove the unknown-event-tag skip path. The
    // highest allocated tag is CWD_CHANGED at `0x0a` (phux-foz.4), so we
    // build an event with tag `0x0b` — a tag THIS version does not know —
    // carrying an opaque body, and assert an older-style decoder skips it by
    // its outer length prefix to `AgentEvent::Unknown` (body preserved
    // verbatim) rather than failing the frame parse. This pins the additive
    // forward-compat contract.
    let body_bytes = [0x01u8, 0x02, 0x03];
    let mut agent_event = vec![0x0bu8]; // a tag this version does not know
    agent_event.extend_from_slice(&u32::try_from(body_bytes.len()).unwrap().to_be_bytes());
    agent_event.extend_from_slice(&body_bytes);
    let mut fields = Vec::new();
    tlv_field(&mut fields, 2, &agent_event); // field::event::EVENT
    let bytes = framed_tlv(0xB3, &fields);

    let (decoded, tail) = FrameKind::decode(&bytes).unwrap();
    assert_eq!(
        decoded,
        FrameKind::Event {
            terminal: None,
            event: AgentEvent::Unknown {
                tag: 0x0b,
                body: body_bytes.to_vec(),
            },
        }
    );
    assert!(tail.is_empty());
}

#[test]
fn terminal_spawned_unknown_result_tag_is_rejected() {
    // A `TERMINAL_SPAWNED` whose RESULT field (id 2) carries an unknown
    // `SpawnResult` tag MUST surface as `UnknownEnumValue`, not silently coerce.
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &7u32.to_be_bytes()); // field::terminal_spawned::REQUEST_ID
    tlv_field(&mut fields, 2, &[0xFE]); // field::terminal_spawned::RESULT (unknown tag)
    let bytes = framed_tlv(0xA2, &fields);

    let err = FrameKind::decode(&bytes).unwrap_err();
    assert_eq!(
        err,
        DecodeError::UnknownEnumValue {
            field: "SpawnResult",
            value: 0xFE,
        }
    );
}

#[test]
fn terminal_spawned_unknown_spawn_error_tag_is_rejected() {
    // Inside the `Err` arm of `SpawnResult`, an unknown `SpawnError` tag
    // MUST also surface as `UnknownEnumValue`. The RESULT field value is the
    // positional SpawnResult: tag 0x01 (Err) then a bogus SpawnError tag.
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &7u32.to_be_bytes()); // field::terminal_spawned::REQUEST_ID
    tlv_field(&mut fields, 2, &[0x01, 0xFE]); // RESULT = Err + unknown SpawnError tag
    let bytes = framed_tlv(0xA2, &fields);

    let err = FrameKind::decode(&bytes).unwrap_err();
    assert_eq!(
        err,
        DecodeError::UnknownEnumValue {
            field: "SpawnError",
            value: 0xFE,
        }
    );
}
