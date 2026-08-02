//! Session protocol logic against the real engine, via the real wire codec —
//! no WebSocket/DOM needed (runs under node).

use bytes::BytesMut;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapProfileKind, ImageProtocolSet, ServerCapabilities,
};
use phux_protocol::ids::{
    BootstrapId, ClientId, SessionId, StreamId, TerminalId, WindowId,
};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_vt_web::Vt;
use phux_web::Session;
use wasm_bindgen_test::wasm_bindgen_test;

fn hello_ok() -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new(),
        server_id: Vec::new(),
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    }
}

fn attached(attach_id: u32, terminal_id: TerminalId) -> FrameKind {
    FrameKind::Attached {
        attach_id,
        snapshot: phux_protocol::wire::info::SessionSnapshot::new(
            SessionId::new(1),
            WindowId::new(1),
            terminal_id,
        ),
        initial_client_id: ClientId::new(1),
    }
}

fn output(terminal_id: TerminalId, bytes: &'static [u8]) -> FrameKind {
    FrameKind::TerminalOutput {
        terminal_id,
        stream_id: StreamId::new(1).unwrap(),
        bootstrap_id: BootstrapId::new(1).unwrap(),
        seq: 7,
        bytes: bytes::Bytes::from_static(bytes),
    }
}

fn key_event() -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::A,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: Some("a".to_owned()),
        unshifted_codepoint: Some(u32::from(b'a')),
    }
}

fn first_row(session: &Session) -> String {
    let grid = session.grid();
    grid.cells[..usize::from(grid.cols)]
        .iter()
        .map(|cell| cell.ch)
        .collect()
}

#[wasm_bindgen_test]
async fn terminal_output_frame_feeds_engine_without_raw_ack() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 20, 3);
    let handshake = session.on_frame(hello_ok());
    assert!(handshake.fatal.is_none());
    let tid = TerminalId::local(1);
    assert!(session.key_frame(key_event()).is_none());
    assert!(session.on_frame(attached(1, tid.clone())).fatal.is_none());
    assert!(session.key_frame(key_event()).is_none());
    assert!(
        session
            .on_frame(FrameKind::AttachReady { attach_id: 1 })
            .fatal
            .is_none()
    );

    // A real TERMINAL_OUTPUT frame, round-tripped through the wire codec (the
    // exact bytes the server would send) before the session sees it.
    let frame = output(tid.clone(), b"Hi phux");
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, rest) = FrameKind::decode(&buf).expect("decode");
    assert!(rest.is_empty(), "one frame per message");

    let outcome = session.on_frame(decoded);
    assert!(outcome.render, "output should trigger a repaint");
    assert!(
        outcome.send.is_empty(),
        "SynthesizedVtRaw never emits FRAME_ACK"
    );
    assert!(outcome.fatal.is_none());

    // The engine rendered the bytes.
    let grid = session.grid();
    let row0: String = grid.cells[..usize::from(grid.cols)]
        .iter()
        .map(|c| c.ch)
        .collect();
    assert!(row0.starts_with("Hi phux"), "row 0 = {row0:?}");
}

#[wasm_bindgen_test]
async fn handshake_waits_for_hello_ok_before_attach() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 80, 24);

    let frames = session.handshake();
    assert_eq!(frames.len(), 1, "transport open sends only HELLO");
    let (hello, _) = FrameKind::decode(&frames[0]).expect("decode hello");
    assert!(matches!(hello, FrameKind::Hello { .. }), "first is HELLO");

    let outcome = session.on_frame(hello_ok());
    assert!(!outcome.render);
    assert!(outcome.fatal.is_none());
    assert_eq!(outcome.send.len(), 1, "HELLO_OK releases exactly one ATTACH");
    let (attach, _) = FrameKind::decode(&outcome.send[0]).expect("decode attach");
    assert!(
        matches!(attach, FrameKind::Attach { attach_id: 1, .. }),
        "ATTACH follows HELLO_OK with a non-zero correlation id"
    );
}

#[wasm_bindgen_test]
async fn attached_id_mismatch_is_fatal() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 80, 24);
    let handshake = session.on_frame(hello_ok());
    assert_eq!(handshake.send.len(), 1);
    let outcome = session.on_frame(attached(2, TerminalId::local(1)));
    assert!(outcome.send.is_empty());
    assert!(
        outcome
            .fatal
            .as_deref()
            .is_some_and(|message| message.contains("attach_id mismatch"))
    );
    let before = first_row(&session);
    let after_fatal = session.on_frame(output(TerminalId::local(1), b"must not render"));
    assert!(after_fatal.send.is_empty());
    assert!(!after_fatal.render);
    assert!(after_fatal.fatal.is_some());
    assert!(session.key_frame(key_event()).is_none());
    assert_eq!(first_row(&session), before);
}

#[wasm_bindgen_test]
async fn output_before_correlated_attached_and_ready_is_fatal() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 80, 24);
    assert!(session.on_frame(hello_ok()).fatal.is_none());
    assert!(session.key_frame(key_event()).is_none());

    let outcome = session.on_frame(output(TerminalId::local(1), b"early"));
    assert!(outcome.send.is_empty());
    assert!(!outcome.render);
    assert!(outcome.fatal.is_some());
    assert!(session.key_frame(key_event()).is_none());
}

#[wasm_bindgen_test]
async fn duplicate_hello_ok_latches_failure_before_queued_output() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 80, 24);
    assert!(session.on_frame(hello_ok()).fatal.is_none());
    assert!(
        session
            .on_frame(attached(1, TerminalId::local(1)))
            .fatal
            .is_none()
    );
    assert!(
        session
            .on_frame(FrameKind::AttachReady { attach_id: 1 })
            .fatal
            .is_none()
    );

    let duplicate = session.on_frame(hello_ok());
    assert!(duplicate.send.is_empty());
    assert!(!duplicate.render);
    assert!(duplicate.fatal.is_some());
    assert!(session.is_failed());
    let before = first_row(&session);

    let queued = session.on_frame(output(TerminalId::local(1), b"queued"));
    assert!(queued.send.is_empty());
    assert!(!queued.render);
    assert!(queued.fatal.is_some());
    assert!(session.key_frame(key_event()).is_none());
    assert_eq!(first_row(&session), before);
}

#[wasm_bindgen_test]
async fn attach_ready_before_attached_is_fatal() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 80, 24);
    assert!(session.on_frame(hello_ok()).fatal.is_none());
    let outcome = session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    assert!(outcome.fatal.is_some());
    assert!(session.is_failed());
}

/// Regression for phux-ycw0: the canvas renderer paints text/color/cursor
/// only, so the HELLO must not advertise any image protocol — otherwise the
/// server forwards image escapes (kitty graphics, sixel, iTerm2) this client
/// silently drops on the floor.
#[wasm_bindgen_test]
async fn hello_advertises_no_image_protocols() {
    let vt = Vt::load().await.expect("load engine");
    let session = Session::new(&vt, 80, 24);

    let frames = session.handshake();
    let (hello, _) = FrameKind::decode(&frames[0]).expect("decode hello");
    let FrameKind::Hello { client_caps, .. } = hello else {
        panic!("first handshake frame must be HELLO");
    };
    assert_eq!(
        client_caps.image_protocols,
        ImageProtocolSet::new(),
        "phux-web cannot render images yet; it must not advertise image \
         protocols (docs/consumers/web.md, ADR-0034)"
    );
    assert!(
        client_caps
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtRaw)
    );
    assert!(
        !client_caps
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::NativeState),
        "web cannot restore native checkpoints and must advertise only its explicit synth fallback",
    );
}

#[wasm_bindgen_test]
async fn malicious_hello_ok_outside_web_offer_is_fatal_before_attach() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 80, 24);
    let outcome = session.on_frame(FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new(),
        server_id: Vec::new(),
        selected_profile: BootstrapProfile::NativeState {
            codec: phux_protocol::EngineCodec::LibghosttyCheckpointV2,
            features: phux_protocol::EngineFeatureSet::required_native(),
        },
        bootstrap_limits: BootstrapLimits::default(),
    });
    assert!(outcome.send.is_empty());
    assert!(
        outcome
            .fatal
            .as_deref()
            .is_some_and(|message| message.contains("outside the web client's offer"))
    );
}

#[wasm_bindgen_test]
async fn hello_ok_bounds_above_web_offer_are_fatal_before_attach() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 80, 24);
    let too_large = BootstrapLimits::new(512 * 1024, 2 * 1024 * 1024).unwrap();
    let outcome = session.on_frame(FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new(),
        server_id: Vec::new(),
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: too_large,
    });
    assert!(outcome.send.is_empty());
    assert!(
        outcome
            .fatal
            .as_deref()
            .is_some_and(|message| message.contains("limits outside"))
    );

}