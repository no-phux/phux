//! Session protocol logic against the real engine, via the real wire codec —
//! no WebSocket/DOM needed (runs under node).

use bytes::BytesMut;
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::FrameKind;
use phux_vt_web::Vt;
use phux_web::Session;
use wasm_bindgen_test::wasm_bindgen_test;

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
        bytes: b"Hi phux".to_vec().into(),
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
async fn handshake_emits_hello_then_attach() {
    let vt = Vt::load().await.expect("load engine");
    let session = Session::new(&vt, 80, 24);

    let frames = session.handshake();
    assert_eq!(frames.len(), 2, "HELLO + ATTACH");
    let (hello, _) = FrameKind::decode(&frames[0]).expect("decode hello");
    assert!(matches!(hello, FrameKind::Hello { .. }), "first is HELLO");
    let (attach, _) = FrameKind::decode(&frames[1]).expect("decode attach");
    assert!(
        matches!(attach, FrameKind::Attach { .. }),
        "second is ATTACH"
    );
}
