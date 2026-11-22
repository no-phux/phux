//! Synthesized bootstraps: dual readiness, state-sync acks, history, replacement.

use super::*;

#[wasm_bindgen_test]
async fn raw_transcript_waits_for_dual_and_global_ready_without_ack() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 20, 3);
    let pane = generation(1, 1, 1);

    let hello = session.on_frame(synth_hello());
    assert!(hello.fatal.is_none());
    assert_eq!(hello.send.len(), 2);
    let attach = decode_one(&attach_after_state(&mut session, &hello));
    assert!(matches!(attach, FrameKind::Attach { attach_id: 1, .. }));

    assert!(
        session
            .on_frame(attached(pane.id.clone(), 20, 3))
            .fatal
            .is_none()
    );
    for frame in [
        pane.begin(RAW, 20, 3, 6),
        pane.chunk(0, &b"Hi "[..]),
        pane.ready(),
    ] {
        assert!(!session.on_frame(frame).render);
    }
    assert!(!session.render_visible());
    assert!(session.key_frame(key()).is_none());
    assert!(
        !session.is_failed(),
        "input gate rejection is not protocol-fatal"
    );

    let output = session.on_frame(pane.output(7, &b"phux"[..]));
    assert!(
        !output.render,
        "global ATTACH_READY still gates first damage"
    );
    assert!(output.send.is_empty(), "raw profile never emits FRAME_ACK");

    let ready = session.on_frame(ATTACH_READY);
    assert!(ready.render);
    assert!(session.is_attach_ready());
    assert!(session.render_visible());
    assert!(session.key_frame(key()).is_some());
    let row0 = row0(&session);
    assert!(row0.starts_with("Hi phux"), "row 0 = {row0:?}");
}

#[wasm_bindgen_test]
async fn state_sync_output_acks_the_exact_generation() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 10, 2);
    let pane = generation(2, 2, 2);
    session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtStateSync,
        BootstrapLimits::default(),
    ));
    session.on_frame(attached(pane.id.clone(), 10, 2));
    let profile = BootstrapStreamProfile::SynthesizedVtStateSync;
    session.on_frame(pane.begin(profile, 10, 2, 40));
    session.on_frame(pane.chunk(0, &b"base"[..]));
    session.on_frame(pane.ready());
    session.on_frame(ATTACH_READY);

    let output = session.on_frame(pane.output(41, &b"+"[..]));
    assert!(output.render);
    assert_eq!(output.send.len(), 1);
    assert_eq!(
        decode_one(&output.send[0]),
        FrameKind::FrameAck {
            terminal_id: pane.id,
            stream_id: pane.stream_id,
            bootstrap_id: pane.bootstrap_id,
            seq: 41,
        }
    );
}

#[wasm_bindgen_test]
async fn history_cursor_chain_is_echoed_and_bounded_after_ready() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 10, 2);
    let pane = generation(3, 3, 3);
    let limits = BootstrapLimits::new(1024, 77).expect("valid limits");
    session.on_frame(hello_ok(BootstrapProfile::SynthesizedVtRaw, limits));
    session.on_frame(attached(pane.id.clone(), 10, 2));
    session.on_frame(pane.begin(RAW, 10, 2, 0));
    session.on_frame(pane.chunk(0, &b"live"[..]));
    let request = |cursor: &'static [u8]| FrameKind::HistoryRequest {
        terminal_id: pane.id.clone(),
        stream_id: pane.stream_id,
        bootstrap_id: pane.bootstrap_id,
        cursor: Bytes::from_static(cursor),
        max_bytes: 77,
        max_rows: 1024,
    };
    let page = |cursor: &'static [u8], next: Option<&'static [u8]>| FrameKind::HistoryPage {
        terminal_id: pane.id.clone(),
        stream_id: pane.stream_id,
        bootstrap_id: pane.bootstrap_id,
        page_seq: 1,
        rows: 1,
        cursor: Bytes::from_static(cursor),
        next_cursor: next.map(Bytes::from_static),
        payload: Bytes::from_static(b"opaque-history"),
    };

    let first = session.on_frame(pane.ready_with(Some(Bytes::from_static(b"cursor-1"))));
    assert_eq!(first.send.len(), 1);
    assert_eq!(decode_one(&first.send[0]), request(b"cursor-1"));
    let second = session.on_frame(page(b"cursor-1", Some(b"cursor-2")));
    assert_eq!(second.send.len(), 1);
    assert_eq!(decode_one(&second.send[0]), request(b"cursor-2"));
    let done = session.on_frame(page(b"cursor-2", None));
    assert!(done.send.is_empty());
    assert!(done.fatal.is_none());
}

#[wasm_bindgen_test]
async fn replacement_stages_without_touching_published_grid() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 10, 2);
    let first = generation(4, 4, 4);
    let second = generation(4, 4, 5);
    session.on_frame(synth_hello());
    session.on_frame(attached(first.id.clone(), 10, 2));
    first.publish(&mut session, RAW, (10, 2), &b"old"[..]);
    session.on_frame(ATTACH_READY);
    session.on_frame(attached(first.id.clone(), 10, 2));
    assert!(!session.render_visible());

    session.on_frame(second.begin(RAW, 10, 2, 10));
    session.on_frame(second.chunk(0, &b"new"[..]));
    assert!(row0(&session).starts_with("old"));

    let swapped = session.on_frame(second.ready());
    assert!(!swapped.render);
    assert!(!session.render_visible());
    let released = session.on_frame(ATTACH_READY);
    assert!(released.render);
    assert!(session.render_visible());
    assert!(row0(&session).starts_with("new"));
}

#[wasm_bindgen_test]
async fn attach_barrier_close_repaints_the_prior_visible_terminal_to_blank() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 10, 2);
    let pane = generation(6, 6, 7);
    session.on_frame(synth_hello());
    session.on_frame(attached(pane.id.clone(), 10, 2));
    pane.publish(&mut session, RAW, (10, 2), &b"visible"[..]);
    assert!(session.on_frame(ATTACH_READY).render);
    assert!(session.render_visible());

    session.on_frame(attached(pane.id.clone(), 10, 2));
    assert!(!session.render_visible());
    let closed = session.on_frame(closed(pane.id, CloseReason::Unknown));
    assert!(!closed.render);
    assert!(!session.render_visible());

    let released = session.on_frame(ATTACH_READY);
    assert!(released.render);
    assert!(session.render_visible());
    assert!(
        session
            .grid()
            .cells
            .iter()
            .all(|cell| cell.ch == ' ' || cell.ch == '\0'),
        "released Removed damage must clear the prior visible canvas",
    );
}
