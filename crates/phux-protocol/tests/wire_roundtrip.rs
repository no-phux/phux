//! Wire-codec round-trip and malformed-input tests.
//!
//! Proptest exercises the encoder and decoder on arbitrary `FrameKind`
//! values. Hand-rolled cases cover known-bad inputs and confirm the decoder
//! returns `DecodeError` rather than panicking.
//!
//! Protocol 0.7 binds `TerminalOutput` to non-zero stream/bootstrap ids; native
//! checkpoint and history frames have focused semantic tests in
//! `bootstrap_wire.rs`.

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ClientCapabilities, ColorSupport, ImageProtocol,
    ImageProtocolSet, KeyboardProtocol, KeyboardProtocolSet, Layer, LayerSet, OutputMode,
    ServerCapabilities, ServerFeature, ServerFeatureSet, TerminalColor, TerminalDefaultColors,
};
use phux_protocol::ids::{
    BootstrapId, ClientId, FileUploadId, GroupId, InputOperationId, SessionId, StreamId,
    TerminalId, WindowId,
};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, Command, CommandResult, CommandValue, ControlAction, ErrorCode,
    FileUploadAck, InputMode, MAX_APPLY_INPUT_COMMAND_BODY, MAX_APPLY_INPUT_EVENTS,
    MAX_FILE_UPLOAD_CHUNK, MAX_FILE_UPLOAD_SIZE, Scope, SpawnError, SpawnResult, StateScope,
    TerminalLifecycle, TerminalSignal, ViewportInfo,
};
use phux_protocol::wire::info::{
    LayoutNode, SessionInfo, SessionSnapshot, SplitDir, TerminalInfo, WindowInfo,
};
use phux_protocol::wire::{DecodeError, decode::Decoder, frame::FrameKind};
use proptest::prelude::*;

// -----------------------------------------------------------------------------
// TLV test helpers (`docs/spec/appendix-encoding.md`).
//
// Message bodies are field-tagged: each field is
// `field_id: varint || wire_type: u8 (4 = BYTES) || varint length || value`.
// These helpers hand-build malformed / partial frames for the
// decoder-rejection and forward-compat tests below, the field-tagged
// counterparts of the old positional byte-builders.
// -----------------------------------------------------------------------------

/// Append an unsigned LEB128 varint.
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

/// Append one TLV field: `field_id || wire_type(4) || len || value`.
fn tlv_field(out: &mut Vec<u8>, field_id: u32, value: &[u8]) {
    put_varint(out, u64::from(field_id));
    out.push(4); // wire_type BYTES
    put_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

/// Wrap a `type_byte` + field-tagged `body` in the outer length frame
/// (`u32 length || type || body`), where `length` covers the type byte + body.
fn framed_tlv(type_byte: u8, fields: &[u8]) -> Vec<u8> {
    let mut body = vec![type_byte];
    body.extend_from_slice(fields);
    let mut frame = Vec::new();
    frame.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
    frame.extend_from_slice(&body);
    frame
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

/// Strategy producing one of the simple-payload `FrameKind` variants. Structured
/// attach, bootstrap/history, generation-bound output, and input frames have
/// dedicated property or focused semantic tests below.
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
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = Decoder::new(&buf).read_frame().unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
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
        let frame = FrameKind::TerminalOutput {
            terminal_id,
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq,
            bytes: bytes.into(),
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

}

#[test]
fn hello_round_trip_minimal() {
    let frame = FrameKind::Hello {
        client_name: "phux-client".to_owned(),
        protocol_major: 0,
        protocol_minor: 1,
        protocol_patch: 0,
        client_caps: ClientCapabilities::default(),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn hello_ok_round_trip() {
    let frame = FrameKind::HelloOk {
        protocol_major: 0,
        protocol_minor: 2,
        protocol_patch: 0,
        server_caps: ServerCapabilities::new().with_layers(LayerSet::all()),
        server_id: vec![0xDE, 0xAD, 0xBE, 0xEF],
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

/// Protocol 0.7 requires an explicit selected profile and negotiated bounds.
#[test]
fn hello_ok_without_profile_is_rejected() {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &0u16.to_be_bytes());
    tlv_field(&mut fields, 2, &7u16.to_be_bytes());
    tlv_field(&mut fields, 3, &0u16.to_be_bytes());
    let framed = framed_tlv(0x80, &fields);
    assert_eq!(
        FrameKind::decode(&framed).unwrap_err(),
        DecodeError::UnexpectedEof
    );
}
#[test]
fn hello_round_trip_each_color_support() {
    for color in [
        ColorSupport::TrueColor,
        ColorSupport::Indexed256,
        ColorSupport::Indexed16,
        ColorSupport::Mono,
    ] {
        let frame = FrameKind::Hello {
            client_name: "phux-client".to_owned(),
            protocol_major: 0,
            protocol_minor: 2,
            protocol_patch: 0,
            client_caps: ClientCapabilities::new().with_color_support(color),
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        assert_eq!(decoded, frame);
        assert!(tail.is_empty());
    }
}

#[test]
fn hello_round_trip_image_kbd_and_hyperlink_caps() {
    let caps = ClientCapabilities::new()
        .with_image_protocols(ImageProtocolSet::with(&[ImageProtocol::Sixel]))
        .with_kbd_protocols(KeyboardProtocolSet::with(&[KeyboardProtocol::Kitty]))
        .with_hyperlinks(false);
    let frame = FrameKind::Hello {
        client_name: "phux-client".to_owned(),
        protocol_major: 0,
        protocol_minor: 2,
        protocol_patch: 0,
        client_caps: caps,
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn hello_round_trip_state_sync_output_mode() {
    // phux-fseo: a consumer advertising OutputMode::StateSync round-trips
    // through the trailing client-caps byte.
    let caps = ClientCapabilities::new().with_output_mode(OutputMode::StateSync);
    let frame = FrameKind::Hello {
        client_name: "phux-agent".to_owned(),
        protocol_major: 0,
        protocol_minor: 2,
        protocol_patch: 0,
        client_caps: caps,
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    let FrameKind::Hello { client_caps, .. } = decoded else {
        panic!("expected Hello");
    };
    assert_eq!(client_caps.output_mode, OutputMode::StateSync);
    assert!(tail.is_empty());
}

#[test]
fn hello_round_trip_outer_terminal_default_colors() {
    let colors = TerminalDefaultColors {
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
    };
    let frame = FrameKind::Hello {
        client_name: "phux-client".to_owned(),
        protocol_major: 0,
        protocol_minor: 2,
        protocol_patch: 0,
        client_caps: ClientCapabilities::new().with_default_colors(colors),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn hello_decoder_rejects_truncated_legacy_caps() {
    let caps_blob = [
        ColorSupport::TrueColor.as_wire(),
        LayerSet::new().as_wire(),
        ImageProtocolSet::default().as_wire(),
        KeyboardProtocolSet::default().as_wire(),
        1u8,
    ];
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, b"x");
    tlv_field(&mut fields, 2, &0u16.to_be_bytes());
    tlv_field(&mut fields, 3, &7u16.to_be_bytes());
    tlv_field(&mut fields, 4, &0u16.to_be_bytes());
    tlv_field(&mut fields, 5, &caps_blob);
    assert_eq!(
        FrameKind::decode(&framed_tlv(0x01, &fields)).unwrap_err(),
        DecodeError::UnexpectedEof
    );
}

#[test]
fn hello_decoder_rejects_legacy_body_without_caps() {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, b"x");
    tlv_field(&mut fields, 2, &0u16.to_be_bytes());
    tlv_field(&mut fields, 3, &7u16.to_be_bytes());
    tlv_field(&mut fields, 4, &0u16.to_be_bytes());
    assert_eq!(
        FrameKind::decode(&framed_tlv(0x01, &fields)).unwrap_err(),
        DecodeError::UnexpectedEof
    );
}

#[test]
fn hello_decoder_rejects_unknown_color_support_tag() {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, b"x");
    tlv_field(&mut fields, 2, &0u16.to_be_bytes());
    tlv_field(&mut fields, 3, &7u16.to_be_bytes());
    tlv_field(&mut fields, 4, &0u16.to_be_bytes());
    tlv_field(&mut fields, 5, &[0xFF]);
    assert_eq!(
        FrameKind::decode(&framed_tlv(0x01, &fields)).unwrap_err(),
        DecodeError::UnknownEnumValue {
            field: "ColorSupport",
            value: 0xFF,
        }
    );
}

#[test]
fn ping_round_trip() {
    let frame = FrameKind::Ping {
        nonce: 0xDEAD_BEEF_CAFE_F00D,
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, _) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
}

#[test]
fn pane_output_round_trip_hello_world() {
    let frame = FrameKind::TerminalOutput {
        stream_id: StreamId::new(1).unwrap(),
        bootstrap_id: BootstrapId::new(1).unwrap(),
        terminal_id: TerminalId::local(1),
        seq: 0,
        bytes: bytes::Bytes::from_static(b"hello world\r\n"),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
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
// ATTACH / ATTACHED generation correlation.
// -----------------------------------------------------------------------------

proptest! {
    #[test]
    fn roundtrip_attach_target(target in arb_attach_target()) {
        let frame = FrameKind::Attach {
            attach_id: 1,
            target,
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_attach_full(
        target in arb_attach_target(),
        viewport in arb_viewport_info(),
        request_scrollback in any::<bool>(),
        scrollback_limit_lines in any::<u32>(),
    ) {
        let frame = FrameKind::Attach {
            attach_id: 1,
            target,
            viewport,
            request_scrollback,
            scrollback_limit_lines,
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_input_key(terminal_id in arb_terminal_id(), event in arb_key_event()) {
        let frame = FrameKind::InputKey { terminal_id, event };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_input_mouse(terminal_id in arb_terminal_id(), event in arb_mouse_event()) {
        let frame = FrameKind::InputMouse { terminal_id, event };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_input_focus(terminal_id in arb_terminal_id(), event in arb_focus_event()) {
        let frame = FrameKind::InputFocus { terminal_id, event };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_input_paste(terminal_id in arb_terminal_id(), event in arb_paste_event()) {
        let frame = FrameKind::InputPaste { terminal_id, event };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_session_info(info in arb_session_info()) {
        let snap = SessionSnapshot::new(info.id, WindowId::new(0), TerminalId::new(0))
            .with_sessions(vec![info]);
        let frame = FrameKind::Attached {
            attach_id: 1,
            snapshot: snap,
            initial_client_id: ClientId::new(0),
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_window_info(info in arb_window_info()) {
        let snap = SessionSnapshot::new(info.session_id, info.id, TerminalId::new(0))
            .with_windows(vec![info]);
        let frame = FrameKind::Attached {
            attach_id: 1,
            snapshot: snap,
            initial_client_id: ClientId::new(0),
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_pane_info(info in arb_pane_info()) {
        let snap = SessionSnapshot::new(SessionId::new(0), info.window_id, info.id.clone())
            .with_panes(vec![info]);
        let frame = FrameKind::Attached {
            attach_id: 1,
            snapshot: snap,
            initial_client_id: ClientId::new(0),
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_layout_node(layout in arb_layout_node()) {
        let win = WindowInfo::new(WindowId::new(1), SessionId::new(1), "w")
            .with_layout(Some(layout));
        let snap = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), TerminalId::new(0))
            .with_windows(vec![win]);
        let frame = FrameKind::Attached {
            attach_id: 1,
            snapshot: snap,
            initial_client_id: ClientId::new(0),
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_attached(
        snapshot in arb_session_snapshot(),
        client_id in any::<u32>(),
    ) {
        let frame = FrameKind::Attached {
            attach_id: 1,
            snapshot,
            initial_client_id: ClientId::new(client_id),
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_bell(terminal_id in arb_terminal_id()) {
        let frame = FrameKind::Bell { terminal_id };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_error(
        request_id in proptest::option::of(any::<u32>()),
        code in arb_error_code(),
        message in ".{0,256}",
    ) {
        let frame = FrameKind::Error { request_id, code, message };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_viewport_resize(viewport in arb_viewport_info()) {
        let frame = FrameKind::ViewportResize { viewport };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_frame_ack(terminal_id in arb_terminal_id(), seq in any::<u64>()) {
        let frame = FrameKind::FrameAck {
            terminal_id,
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq,
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }
}

#[test]
fn detach_round_trip() {
    let frame = FrameKind::Detach;
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn detached_round_trip() {
    let frame = FrameKind::Detached;
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
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
fn error_round_trip_session_not_found() {
    // The canonical case unblocking phux-byc.6.6: the server replies to an
    // ATTACH targeting an unknown session with ERROR { SESSION_NOT_FOUND }.
    let frame = FrameKind::Error {
        request_id: None,
        code: ErrorCode::SessionNotFound,
        message: "no such session: 'work'".to_owned(),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn error_round_trip_with_request_id() {
    let frame = FrameKind::Error {
        request_id: Some(42),
        code: ErrorCode::InvalidCommand,
        message: "missing field: terminal_id".to_owned(),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
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

#[test]
fn bell_round_trip() {
    let frame = FrameKind::Bell {
        terminal_id: TerminalId::local(0x1234_5678),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

// -----------------------------------------------------------------------------
// Layout ratio validation — SPEC §13 leaves bounds implicit; phux rejects
// NaN, infinite, and out-of-range values on decode.
// -----------------------------------------------------------------------------

fn encode_split_with_ratio(ratio: f32) -> Vec<u8> {
    // Hand-roll an ATTACHED frame whose single WindowInfo carries a Split node
    // with `ratio`. Under field-tagged TLV the message body is two fields —
    // SNAPSHOT (id 1) and INITIAL_CLIENT_ID (id 2) — but the SessionSnapshot
    // value is still positional, so the inner bytes mirror
    // `info::encode_session_snapshot` exactly. Keep in sync when the snapshot
    // wire shape changes.
    let mut snap = Vec::new();
    // sessions: empty list
    snap.extend_from_slice(&0u32.to_be_bytes());
    // windows: one item
    snap.extend_from_slice(&1u32.to_be_bytes());
    // WindowInfo
    snap.extend_from_slice(&1u32.to_be_bytes()); // id
    snap.extend_from_slice(&1u32.to_be_bytes()); // session_id
    snap.extend_from_slice(&0u16.to_be_bytes()); // index
    snap.extend_from_slice(&1u32.to_be_bytes()); // name length
    snap.push(b'w'); // name bytes
    snap.push(0); // active_pane: None
    snap.push(1); // layout: Some
    snap.push(1); // LayoutNode::Split
    snap.push(0); // SplitDir::Horizontal
    snap.extend_from_slice(&ratio.to_be_bytes());
    // Left leaf: LAYOUT_TAG_LEAF=0, then TerminalId::Local { id: 1 }
    snap.push(0);
    snap.push(0); // TERMINAL_ID_TAG_LOCAL
    snap.extend_from_slice(&1u32.to_be_bytes());
    // Right leaf: LAYOUT_TAG_LEAF=0, then TerminalId::Local { id: 2 }
    snap.push(0);
    snap.push(0); // TERMINAL_ID_TAG_LOCAL
    snap.extend_from_slice(&2u32.to_be_bytes());
    // panes: empty list
    snap.extend_from_slice(&0u32.to_be_bytes());
    // focused_session, focused_window, focused_pane (tagged TerminalId)
    snap.extend_from_slice(&0u32.to_be_bytes()); // focused_session
    snap.extend_from_slice(&0u32.to_be_bytes()); // focused_window
    snap.push(0); // TERMINAL_ID_TAG_LOCAL
    snap.extend_from_slice(&1u32.to_be_bytes()); // focused_pane id

    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &snap); // field::attached::SNAPSHOT
    tlv_field(&mut fields, 2, &0u32.to_be_bytes()); // field::attached::INITIAL_CLIENT_ID
    tlv_field(&mut fields, 3, &1u32.to_be_bytes()); // field::attached::ATTACH_ID
    framed_tlv(0x81, &fields)
}

#[test]
fn layout_ratio_nan_is_rejected() {
    let bytes = encode_split_with_ratio(f32::NAN);
    let err = FrameKind::decode(&bytes).unwrap_err();
    match err {
        DecodeError::MalformedLayoutRatio { ratio } => assert!(ratio.is_nan()),
        other => panic!("expected MalformedLayoutRatio, got {other:?}"),
    }
}

#[test]
fn layout_ratio_above_one_is_rejected() {
    let bytes = encode_split_with_ratio(1.5);
    let err = FrameKind::decode(&bytes).unwrap_err();
    assert_eq!(err, DecodeError::MalformedLayoutRatio { ratio: 1.5 });
}

#[test]
fn layout_ratio_negative_is_rejected() {
    let bytes = encode_split_with_ratio(-0.1);
    let err = FrameKind::decode(&bytes).unwrap_err();
    assert_eq!(err, DecodeError::MalformedLayoutRatio { ratio: -0.1 });
}

#[test]
fn layout_ratio_infinite_is_rejected() {
    let bytes = encode_split_with_ratio(f32::INFINITY);
    let err = FrameKind::decode(&bytes).unwrap_err();
    assert_eq!(
        err,
        DecodeError::MalformedLayoutRatio {
            ratio: f32::INFINITY
        }
    );
}

#[test]
#[allow(clippy::float_cmp)]
fn layout_ratio_zero_is_accepted() {
    let bytes = encode_split_with_ratio(0.0);
    let (decoded, _tail) = FrameKind::decode(&bytes).unwrap();
    if let FrameKind::Attached { snapshot, .. } = decoded {
        let win = &snapshot.windows[0];
        match win.layout.as_ref().unwrap() {
            LayoutNode::Split { ratio, .. } => assert_eq!(*ratio, 0.0),
            other => panic!("expected Split, got {other:?}"),
        }
    } else {
        panic!("expected Attached frame");
    }
}

#[test]
#[allow(clippy::float_cmp)]
fn layout_ratio_one_is_accepted() {
    let bytes = encode_split_with_ratio(1.0);
    let (decoded, _tail) = FrameKind::decode(&bytes).unwrap();
    if let FrameKind::Attached { snapshot, .. } = decoded {
        let win = &snapshot.windows[0];
        match win.layout.as_ref().unwrap() {
            LayoutNode::Split { ratio, .. } => assert_eq!(*ratio, 1.0),
            other => panic!("expected Split, got {other:?}"),
        }
    } else {
        panic!("expected Attached frame");
    }
}

#[allow(dead_code)]
const _SPLIT_DIR_TYPE_CHECK: fn() -> SplitDir = || SplitDir::Horizontal;

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
        let frame = FrameKind::GetMetadata { request_id, scope, key };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_set_metadata(
        request_id in any::<u32>(),
        scope in arb_scope(),
        key in ".{0,64}",
        value in arb_metadata_value(),
    ) {
        let frame = FrameKind::SetMetadata { request_id, scope, key, value };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_delete_metadata(
        request_id in any::<u32>(),
        scope in arb_scope(),
        key in ".{0,64}",
    ) {
        let frame = FrameKind::DeleteMetadata { request_id, scope, key };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_list_metadata(
        request_id in any::<u32>(),
        scope in arb_scope(),
    ) {
        let frame = FrameKind::ListMetadata { request_id, scope };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_subscribe_metadata(
        scope in arb_scope(),
        key in ".{0,64}",
    ) {
        let frame = FrameKind::SubscribeMetadata { scope, key };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_metadata_changed(
        scope in arb_scope(),
        key in ".{0,64}",
        value in proptest::option::of(arb_metadata_value()),
    ) {
        let frame = FrameKind::MetadataChanged { scope, key, value };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    /// METADATA_VALUE — reply to GET_METADATA (phux-4li.8). Carries the
    /// request_id verbatim and an optional value (None = key absent).
    #[test]
    fn roundtrip_metadata_value(
        request_id in any::<u32>(),
        value in proptest::option::of(arb_metadata_value()),
    ) {
        let frame = FrameKind::MetadataValue { request_id, value };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    /// METADATA_KEYS — reply to LIST_METADATA (phux-4li.8). Carries the
    /// request_id verbatim and a (possibly empty) list of key names.
    #[test]
    fn roundtrip_metadata_keys(
        request_id in any::<u32>(),
        keys in proptest::collection::vec(".{0,32}", 0..8),
    ) {
        let frame = FrameKind::MetadataKeys { request_id, keys };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    /// HELLO carries the complete protocol-0.7 capability record, including
    /// layer and bootstrap negotiation fields.
    #[test]
    fn roundtrip_hello_layers(
        layers in arb_layer_set(),
    ) {
        let frame = FrameKind::Hello {
            client_name: "phux-client/test".to_owned(),
            protocol_major: 0,
            protocol_minor: 7,
            protocol_patch: 0,
            client_caps: ClientCapabilities::new()
                .with_color_support(ColorSupport::TrueColor)
                .with_layers(layers),
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }
}

#[test]
fn hello_decoder_rejects_legacy_body_with_color_but_no_layers() {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, b"x");
    tlv_field(&mut fields, 2, &0u16.to_be_bytes());
    tlv_field(&mut fields, 3, &7u16.to_be_bytes());
    tlv_field(&mut fields, 4, &0u16.to_be_bytes());
    tlv_field(&mut fields, 5, &[0x00]);
    assert_eq!(
        FrameKind::decode(&framed_tlv(0x01, &fields)).unwrap_err(),
        DecodeError::UnexpectedEof
    );
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

proptest! {
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
        let frame = FrameKind::SpawnTerminal {
            request_id,
            group: GroupId::new(group),
            command,
            cwd,
            env,
            term,
            satellite: satellite.map(phux_protocol::ids::SatelliteHost::new),
            owner_terminal: owner_terminal.map(TerminalId::local),
            agent_session,
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_terminal_spawned(
        request_id in any::<u32>(),
        result in arb_spawn_result(),
    ) {
        let frame = FrameKind::TerminalSpawned { request_id, result };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_terminal_closed(
        terminal_id in arb_terminal_id(),
        exit_status in proptest::option::of(any::<i32>()),
    ) {
        let frame = FrameKind::TerminalClosed { terminal_id, exit_status };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_terminal_resize(
        terminal_id in arb_terminal_id(),
        cols in any::<u16>(),
        rows in any::<u16>(),
    ) {
        let frame = FrameKind::TerminalResize { terminal_id, cols, rows };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    #[test]
    fn roundtrip_command_kill_terminal(
        request_id in any::<u32>(),
        terminal_id in arb_terminal_id(),
    ) {
        let frame = FrameKind::Command {
            request_id,
            command: Command::KillTerminal { terminal_id },
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
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
            let frame = FrameKind::CommandResult { request_id, result };
            let mut buf = BytesMut::new();
            frame.encode(&mut buf);
            let (decoded, tail) = FrameKind::decode(&buf).unwrap();
            prop_assert_eq!(decoded, frame);
            prop_assert!(tail.is_empty());
        }
    }
}

#[test]
fn command_get_state_round_trips() {
    let frame = FrameKind::Command {
        request_id: 7,
        command: Command::GetState {
            scope: StateScope::Server,
        },
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn command_attach_detach_terminal_round_trip() {
    // phux-v45.7: the per-Terminal subscription verbs (SPEC §5.1 tags
    // 0x01/0x02) round-trip with both Local and Satellite ids — the
    // Satellite form is what a hub consumer sends for two-hop attach.
    for terminal_id in [
        phux_protocol::ids::TerminalId::local(7),
        phux_protocol::ids::TerminalId::satellite("devbox", 7),
    ] {
        for command in [
            Command::AttachTerminal {
                terminal_id: terminal_id.clone(),
            },
            Command::DetachTerminal {
                terminal_id: terminal_id.clone(),
            },
        ] {
            let frame = FrameKind::Command {
                request_id: 21,
                command,
            };
            let mut buf = BytesMut::new();
            frame.encode(&mut buf);
            let (decoded, tail) = FrameKind::decode(&buf).unwrap();
            assert_eq!(decoded, frame);
            assert!(tail.is_empty());
        }
    }
}

#[test]
fn command_upgrade_round_trips() {
    let frame = FrameKind::Command {
        request_id: 9,
        command: Command::Upgrade,
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn command_acquire_input_round_trips() {
    // ADR-0033: both acquisition modes round-trip, with the advisory ttl.
    for mode in [InputMode::Cooperative, InputMode::Seize] {
        let frame = FrameKind::Command {
            request_id: 11,
            command: Command::AcquireInput {
                terminal_id: phux_protocol::ids::TerminalId::local(7),
                mode,
                ttl_ms: 30_000,
            },
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        assert_eq!(decoded, frame);
        assert!(tail.is_empty());
    }
}

#[test]
fn command_release_input_round_trips() {
    let frame = FrameKind::Command {
        request_id: 12,
        command: Command::ReleaseInput {
            terminal_id: phux_protocol::ids::TerminalId::local(7),
        },
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
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
        let frame = FrameKind::Command {
            request_id: 13,
            command: Command::SignalTerminal {
                terminal_id: phux_protocol::ids::TerminalId::local(3),
                signal,
            },
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        assert_eq!(decoded, frame);
        assert!(tail.is_empty());
    }
}

#[test]
fn command_report_asked_round_trips() {
    let frame = FrameKind::Command {
        request_id: 42,
        command: Command::ReportAsked {
            terminal_id: TerminalId::local(7),
            id: "q1".to_owned(),
            question: "Deploy to prod?".to_owned(),
            suggestions: vec!["Yes".to_owned(), "No".to_owned(), "Hold".to_owned()],
            elapsed_seconds: Some(9),
        },
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
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
            let frame = FrameKind::Command {
                request_id: 11,
                command: Command::GetScreen {
                    terminal_id: TerminalId::local(5),
                    request_scrollback,
                    cells,
                },
            };
            let mut buf = BytesMut::new();
            frame.encode(&mut buf);
            let (decoded, tail) = FrameKind::decode(&buf).unwrap();
            assert_eq!(decoded, frame);
            assert!(tail.is_empty());
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
        let frame = FrameKind::Command {
            request_id: 21,
            command: Command::RouteInput {
                terminal_id: TerminalId::local(5),
                event,
            },
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        assert_eq!(decoded, frame);
        assert!(tail.is_empty());
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
    let mut ack_bytes = BytesMut::new();
    ack.encode(&mut ack_bytes);
    let (decoded_ack, tail) = FrameKind::decode(&ack_bytes).unwrap();
    assert_eq!(decoded_ack, ack);
    assert!(tail.is_empty());

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
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
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
        let frame = FrameKind::Command {
            request_id: 31,
            command: Command::KillTerminals { ids: ids.clone() },
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        assert_eq!(decoded, frame);
        assert!(tail.is_empty());
    }
}

#[test]
fn command_detach_clients_round_trips() {
    // DETACH_CLIENTS (tag 0x13): a presence byte + optional session name.
    // Exercise both the `None` (detach all) and `Some(name)` targeting so the
    // presence byte and the string encoding both round-trip.
    for session in [None, Some("work".to_owned()), Some(String::new())] {
        let frame = FrameKind::Command {
            request_id: 44,
            command: Command::DetachClients {
                session: session.clone(),
            },
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        assert_eq!(decoded, frame);
        assert!(tail.is_empty());
    }
}

#[test]
fn command_result_ok_with_json_round_trips() {
    // GET_SCREEN's reply shape: OK_WITH(JSON(serialized ScreenState)).
    let frame = FrameKind::CommandResult {
        request_id: 12,
        result: CommandResult::OkWith(CommandValue::Json(
            r#"{"schema_version":1,"pane":5,"cols":80,"rows":24,"cursor":null,"lines":["$ "]}"#
                .to_owned(),
        )),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn command_result_ok_with_state_snapshot_round_trips() {
    // `GET_STATE`'s reply: OK_WITH(STATE(snapshot)). Reuses the ATTACHED
    // snapshot shape, so a non-trivial snapshot must survive the round trip.
    let info = SessionInfo::new(SessionId::new(1), "work".to_owned());
    let snap = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), TerminalId::local(1))
        .with_sessions(vec![info]);
    let frame = FrameKind::CommandResult {
        request_id: 9,
        result: CommandResult::OkWith(CommandValue::State(snap)),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn command_result_ok_with_terminal_id_round_trips() {
    let frame = FrameKind::CommandResult {
        request_id: 3,
        result: CommandResult::OkWith(CommandValue::TerminalId(TerminalId::local(42))),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
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

#[test]
fn spawn_terminal_empty_command_vec_round_trips() {
    // `command = Some(vec![])` is distinct from `command = None` and the
    // codec must round-trip both faithfully. (Servers MAY treat an empty
    // argv as malformed at the dispatch layer; the wire is agnostic.)
    let frame = FrameKind::SpawnTerminal {
        request_id: 1,
        group: GroupId::new(1),
        command: Some(Vec::new()),
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session: None,
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn spawn_terminal_empty_env_vec_round_trips() {
    // `env = Some(vec![])` is the "start with empty environment" sentinel
    // and is distinct from `env = None` ("inherit server's env"). The
    // codec must preserve the distinction.
    let frame = FrameKind::SpawnTerminal {
        request_id: 2,
        group: GroupId::new(1),
        command: None,
        cwd: None,
        env: Some(Vec::new()),
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session: None,
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn spawn_terminal_term_field_round_trips() {
    // The first-class `term` field (phux-ign) is additive field id 6: a
    // bare optional UTF-8 string distinct from a `TERM` env pair. `None`
    // and `Some(..)` must both round-trip faithfully.
    for term in [None, Some("ghostty".to_owned()), Some(String::new())] {
        let frame = FrameKind::SpawnTerminal {
            request_id: 3,
            group: GroupId::new(1),
            command: None,
            cwd: None,
            env: None,
            term,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        assert_eq!(decoded, frame);
        assert!(tail.is_empty());
    }
}

#[test]
fn terminal_resize_zero_dims_round_trips() {
    // SPEC §10.2 leaves zero dims implementation-defined (the server's PTY
    // layer SHOULD treat them as no-ops rather than kernel errors). The
    // wire codec is agnostic and round-trips zeros faithfully.
    let frame = FrameKind::TerminalResize {
        terminal_id: TerminalId::local(1),
        cols: 0,
        rows: 0,
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn terminal_closed_signal_exit_uses_none() {
    // `exit_status = None` is the wire encoding for "killed by signal /
    // unknown cause" — a deliberately compact subset of SPEC §10.1's
    // `ExitStatus` tagged union. The full tagged union grows in a follow-
    // up wire bump if the additional structure proves load-bearing.
    let frame = FrameKind::TerminalClosed {
        terminal_id: TerminalId::local(7),
        exit_status: None,
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn terminal_closed_exit_status_negative_round_trips() {
    // `i32` is encoded as `u32` two's-complement on the wire; negative
    // values round-trip faithfully.
    let frame = FrameKind::TerminalClosed {
        terminal_id: TerminalId::local(7),
        exit_status: Some(-1),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
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
        let frame = FrameKind::SubscribeEvents { terminal };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
    }

    /// `EVENT` round-trips across the full event taxonomy and both scope
    /// shapes.
    #[test]
    fn roundtrip_event(
        terminal in proptest::option::of(arb_terminal_id()),
        event in arb_agent_event(),
    ) {
        let frame = FrameKind::Event { terminal, event };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, tail) = FrameKind::decode(&buf).unwrap();
        prop_assert_eq!(decoded, frame);
        prop_assert!(tail.is_empty());
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
fn event_unknown_tag_reencodes_verbatim() {
    // A relay that decodes an unknown event and re-encodes it MUST produce
    // byte-identical output — `Unknown` is a lossless passthrough.
    let frame = FrameKind::Event {
        terminal: None,
        event: AgentEvent::Unknown {
            tag: 0x55,
            body: vec![1, 2, 3, 4, 5],
        },
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn event_asked_round_trips_full() {
    // The new `AgentEvent::Asked` carries an agent's pending question with
    // every field populated; it MUST round-trip on the EVENT stream.
    let frame = FrameKind::Event {
        terminal: None,
        event: AgentEvent::Asked {
            id: "q-7f3a".to_string(),
            question: "Which transport should the bridge use?".to_string(),
            suggestions: vec![
                "WebSocket".to_string(),
                "gRPC".to_string(),
                "raw TCP".to_string(),
            ],
            elapsed_seconds: Some(42),
        },
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn event_asked_round_trips_minimal() {
    // No suggestions, no elapsed counter: the absent fields default to an
    // empty list / `None` on decode.
    let frame = FrameKind::Event {
        terminal: None,
        event: AgentEvent::Asked {
            id: "q-0".to_string(),
            question: "Proceed?".to_string(),
            suggestions: Vec::new(),
            elapsed_seconds: None,
        },
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn event_cwd_changed_round_trips() {
    // phux-foz.4: the `cwd_changed` event (tag 0x0a) carries the pane's new
    // working directory and MUST round-trip on the EVENT stream.
    let frame = FrameKind::Event {
        terminal: Some(TerminalId::local(7)),
        event: AgentEvent::CwdChanged {
            cwd: "/Users/phall/workspace/phux".to_string(),
        },
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
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
