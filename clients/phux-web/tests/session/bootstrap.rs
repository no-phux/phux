//! Synthesized bootstraps: dual readiness, state-sync acks, history, replacement.

use super::*;

#[wasm_bindgen_test]
async fn raw_transcript_waits_for_dual_and_global_ready_without_ack() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 20, 3);
    let terminal_id = ResourceId::local(1);
    let stream_id = stream(1);
    let bootstrap_id = bootstrap(1);

    let hello = session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    assert!(hello.fatal.is_none());
    assert_eq!(hello.send.len(), 2);
    let attach = decode_one(&attach_after_state(&mut session, &hello));
    assert!(matches!(attach, FrameKind::Attach { attach_id: 1, .. }));

    assert!(
        session
            .on_frame(attached(terminal_id.clone(), 20, 3))
            .fatal
            .is_none()
    );
    assert!(
        !session
            .on_frame(begin(
                terminal_id.clone(),
                stream_id,
                bootstrap_id,
                phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
                20,
                3,
                6,
            ))
            .render
    );
    assert!(
        !session
            .on_frame(FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: Bytes::from_static(b"Hi "),
            })
            .render
    );
    assert!(
        !session
            .on_frame(FrameKind::BootstrapReady {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                history_cursor: None,
            })
            .render
    );
    assert!(!session.render_visible());
    assert!(session.key_frame(key()).is_none());
    assert!(
        !session.is_failed(),
        "input gate rejection is not protocol-fatal"
    );

    let output = session.on_frame(FrameKind::ResourceOutput {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        seq: 7,
        bytes: Bytes::from_static(b"phux"),
    });
    assert!(
        !output.render,
        "global ATTACH_READY still gates first damage"
    );
    assert!(output.send.is_empty(), "raw profile never emits FRAME_ACK");

    let ready = session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    assert!(ready.render);
    assert!(session.is_attach_ready());
    assert!(session.render_visible());
    assert!(session.key_frame(key()).is_some());
    let grid = session.grid();
    let row0: String = grid.cells[..usize::from(grid.cols)]
        .iter()
        .map(|cell| cell.ch)
        .collect();
    assert!(row0.starts_with("Hi phux"), "row 0 = {row0:?}");
}

#[wasm_bindgen_test]
async fn state_sync_output_acks_the_exact_generation() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 10, 2);
    let terminal_id = ResourceId::local(2);
    let stream_id = stream(2);
    let bootstrap_id = bootstrap(2);
    session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtStateSync,
        BootstrapLimits::default(),
    ));
    session.on_frame(attached(terminal_id.clone(), 10, 2));
    session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        bootstrap_id,
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtStateSync,
        10,
        2,
        40,
    ));
    session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        chunk_seq: 0,
        payload: Bytes::from_static(b"base"),
    });
    session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    session.on_frame(FrameKind::AttachReady { attach_id: 1 });

    let output = session.on_frame(FrameKind::ResourceOutput {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        seq: 41,
        bytes: Bytes::from_static(b"+"),
    });
    assert!(output.render);
    assert_eq!(output.send.len(), 1);
    let (ack, rest) = FrameKind::decode(&output.send[0]).expect("decode ack");
    assert!(rest.is_empty());
    assert_eq!(
        ack,
        FrameKind::FrameAck {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq: 41,
        }
    );
}

#[wasm_bindgen_test]
async fn history_cursor_chain_is_echoed_and_bounded_after_ready() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 10, 2);
    let terminal_id = ResourceId::local(3);
    let stream_id = stream(3);
    let bootstrap_id = bootstrap(3);
    let limits = BootstrapLimits::new(1024, 77).expect("valid limits");
    session.on_frame(hello_ok(BootstrapProfile::SynthesizedVtRaw, limits));
    session.on_frame(attached(terminal_id.clone(), 10, 2));
    session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        bootstrap_id,
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
        10,
        2,
        0,
    ));
    session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        chunk_seq: 0,
        payload: Bytes::from_static(b"live"),
    });

    let first = session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: Some(Bytes::from_static(b"cursor-1")),
    });
    assert_eq!(first.send.len(), 1);
    let (request, _) = FrameKind::decode(&first.send[0]).expect("decode history request");
    assert_eq!(
        request,
        FrameKind::HistoryRequest {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: Bytes::from_static(b"cursor-1"),
            max_bytes: 77,
            max_rows: 1024,
        }
    );

    let second = session.on_frame(FrameKind::HistoryPage {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        page_seq: 1,
        rows: 1,
        cursor: Bytes::from_static(b"cursor-1"),
        next_cursor: Some(Bytes::from_static(b"cursor-2")),
        payload: Bytes::from_static(b"opaque-history-1"),
    });
    assert_eq!(second.send.len(), 1);
    let (request, _) = FrameKind::decode(&second.send[0]).expect("decode next request");
    assert_eq!(
        request,
        FrameKind::HistoryRequest {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: Bytes::from_static(b"cursor-2"),
            max_bytes: 77,
            max_rows: 1024,
        }
    );

    let done = session.on_frame(FrameKind::HistoryPage {
        terminal_id,
        stream_id,
        bootstrap_id,
        page_seq: 1,
        rows: 1,
        cursor: Bytes::from_static(b"cursor-2"),
        next_cursor: None,
        payload: Bytes::from_static(b"opaque-history-2"),
    });
    assert!(done.send.is_empty());
    assert!(done.fatal.is_none());
}

#[wasm_bindgen_test]
async fn replacement_stages_without_touching_published_grid() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 10, 2);
    let terminal_id = ResourceId::local(4);
    let stream_id = stream(4);
    let first_bootstrap = bootstrap(4);
    let second_bootstrap = bootstrap(5);
    session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    session.on_frame(attached(terminal_id.clone(), 10, 2));
    session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        first_bootstrap,
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
        10,
        2,
        0,
    ));
    session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id: first_bootstrap,
        chunk_seq: 0,
        payload: Bytes::from_static(b"old"),
    });
    session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id: first_bootstrap,
        history_cursor: None,
    });
    session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    session.on_frame(attached(terminal_id.clone(), 10, 2));
    assert!(!session.render_visible());

    session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        second_bootstrap,
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
        10,
        2,
        10,
    ));
    session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id: second_bootstrap,
        chunk_seq: 0,
        payload: Bytes::from_static(b"new"),
    });
    let before: String = session.grid().cells[..10]
        .iter()
        .map(|cell| cell.ch)
        .collect();
    assert!(before.starts_with("old"));

    let swapped = session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id: second_bootstrap,
        history_cursor: None,
    });
    assert!(!swapped.render);
    assert!(!session.render_visible());
    let released = session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    assert!(released.render);
    assert!(session.render_visible());
    let after: String = session.grid().cells[..10]
        .iter()
        .map(|cell| cell.ch)
        .collect();
    assert!(after.starts_with("new"));
}

#[wasm_bindgen_test]
async fn attach_barrier_close_repaints_the_prior_visible_terminal_to_blank() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 10, 2);
    let terminal_id = ResourceId::local(6);
    let stream_id = stream(6);
    let bootstrap_id = bootstrap(7);
    session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    session.on_frame(attached(terminal_id.clone(), 10, 2));
    session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        bootstrap_id,
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
        10,
        2,
        0,
    ));
    session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        chunk_seq: 0,
        payload: Bytes::from_static(b"visible"),
    });
    session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    assert!(
        session
            .on_frame(FrameKind::AttachReady { attach_id: 1 })
            .render
    );
    assert!(session.render_visible());

    session.on_frame(attached(terminal_id.clone(), 10, 2));
    assert!(!session.render_visible());
    let closed = session.on_frame(FrameKind::ResourceClosed {
        terminal_id,
        exit_status: None,
        reason: phux_protocol::wire::frame::CloseReason::Unknown,
        signal: None,
    });
    assert!(!closed.render);
    assert!(!session.render_visible());

    let released = session.on_frame(FrameKind::AttachReady { attach_id: 1 });
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
