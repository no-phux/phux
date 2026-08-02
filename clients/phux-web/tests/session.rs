//! Session protocol logic against the real engine, via the real wire codec —
//! no WebSocket/DOM needed (runs under node).

use bytes::BytesMut;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ImageProtocolSet, ServerCapabilities};
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::FrameKind;
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
    }
}

#[wasm_bindgen_test]
async fn terminal_output_frame_feeds_engine_and_acks() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 20, 3);

    // A real TERMINAL_OUTPUT frame, round-tripped through the wire codec (the
    // exact bytes the server would send) before the session sees it.
    let tid = TerminalId::local(1);
    let frame = FrameKind::TerminalOutput {
        terminal_id: tid.clone(),
        seq: 7,
        bytes: bytes::Bytes::from_static(b"Hi phux"),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, rest) = FrameKind::decode(&buf).expect("decode");
    assert!(rest.is_empty(), "one frame per message");

    let outcome = session.on_frame(decoded);
    assert!(outcome.render, "output should trigger a repaint");
    assert_eq!(outcome.send.len(), 1, "output should be acked");

    // The ack is a real FRAME_ACK for the same terminal + seq.
    let (ack, _) = FrameKind::decode(&outcome.send[0]).expect("decode ack");
    match ack {
        FrameKind::FrameAck { terminal_id, seq } => {
            assert_eq!(terminal_id, tid);
            assert_eq!(seq, 7);
        }
        other => panic!("expected FRAME_ACK, got {other:?}"),
    }

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
    assert_eq!(outcome.send.len(), 1, "HELLO_OK releases exactly one ATTACH");
    let (attach, _) = FrameKind::decode(&outcome.send[0]).expect("decode attach");
    assert!(
        matches!(attach, FrameKind::Attach { .. }),
        "ATTACH follows HELLO_OK"
    );
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
}
