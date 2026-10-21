//! Viewport geometry, resize ordering, bells, and mouse input.

use super::*;

/// The viewport's size in pixels, as the ATTACH or VIEWPORT_RESIZE in
/// `frame` reports it.
fn reported_pixels(frame: &[u8]) -> (Option<u16>, Option<u16>) {
    match decode_one(frame) {
        FrameKind::Attach { viewport, .. } | FrameKind::ViewportResize { viewport } => {
            (viewport.pixel_w, viewport.pixel_h)
        }
        other => panic!("not a viewport report: {other:?}"),
    }
}

/// The server sizes its mouse encoder's cells from the viewport's pixel
/// size (`pixel / cells`), so the client reports the cell grid it draws and
/// sends pointer positions in: without it, the server divides by whatever
/// cell size another client reported, or its 8x16 default.
#[wasm_bindgen_test]
async fn the_viewport_reports_the_cell_grid_in_pixels() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 80, 24);
    session.set_cell_size(9, 18);
    let attach = session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    assert_eq!(
        reported_pixels(&attach_after_state(&mut session, &attach)),
        (Some(80 * 9), Some(24 * 18))
    );
    let resize = session.resize_frame(100, 30).expect("VIEWPORT_RESIZE");
    assert_eq!(reported_pixels(&resize), (Some(100 * 9), Some(30 * 18)));

    // A grid too wide for the wire's u16 pixels reports no pixel size
    // rather than a wrapped one.
    let huge = session.resize_frame(8_000, 30).expect("VIEWPORT_RESIZE");
    assert_eq!(reported_pixels(&huge), (None, None));
}

#[wasm_bindgen_test]
async fn resize_rides_the_attach_before_hello_ok_and_viewport_resize_after() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 80, 24);
    assert!(
        session.resize_frame(100, 30).is_none(),
        "no stateful frame before HELLO_OK"
    );
    let attach = session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    let FrameKind::Attach { viewport, .. } = decode_one(&attach_after_state(&mut session, &attach))
    else {
        panic!("the session list is answered by ATTACH");
    };
    assert_eq!((viewport.cols, viewport.rows), (100, 30));

    let frame = session.resize_frame(120, 40).expect("VIEWPORT_RESIZE");
    let FrameKind::ViewportResize { viewport } = decode_one(&frame) else {
        panic!("resize encodes VIEWPORT_RESIZE");
    };
    assert_eq!((viewport.cols, viewport.rows), (120, 40));
    assert!(
        session.resize_frame(120, 40).is_none(),
        "an unchanged size sends nothing"
    );
    let clamped = session.resize_frame(0, 0).expect("clamped resize");
    let FrameKind::ViewportResize { viewport } = decode_one(&clamped) else {
        panic!("resize encodes VIEWPORT_RESIZE");
    };
    assert_eq!((viewport.cols, viewport.rows), (1, 1));

    session.fail_protocol("closed");
    assert!(
        session.resize_frame(90, 20).is_none(),
        "a failed session is inert"
    );
}

#[wasm_bindgen_test]
async fn live_bells_ring_bootstrap_bells_do_not_and_mouse_input_is_structured() {
    use phux_protocol::input::InputEvent;
    use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};

    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 20, 3);
    let terminal_id = ResourceId::local(1);
    let (stream_id, bootstrap_id) = (stream(1), bootstrap(1));
    session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    session.on_frame(attached(terminal_id.clone(), 20, 3));
    session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        bootstrap_id,
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
        20,
        3,
        0,
    ));
    let replayed = session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        chunk_seq: 0,
        payload: Bytes::from_static(b"old\x07"),
    });
    assert!(!replayed.bell, "replayed state never rings");
    session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    let ready = session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    assert!(!ready.bell, "a bootstrap bell does not ring at publication");

    let output = |seq, bytes: &'static [u8]| FrameKind::ResourceOutput {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        seq,
        bytes: Bytes::from_static(bytes),
    };
    assert!(
        !session.on_frame(output(1, b"\x1b]0;title\x07")).bell,
        "an OSC terminator is not a bell"
    );
    assert!(
        session.on_frame(output(2, b"ding\x07")).bell,
        "a live BEL rings"
    );
    assert!(!session.on_frame(output(3, b"more")).bell);

    assert!(!session.terminal().expect("published").mouse_tracking());
    session.on_frame(output(4, b"\x1b[?1000h"));
    assert!(session.terminal().expect("published").mouse_tracking());

    let press = MouseEvent {
        action: MouseAction::Press,
        button: MouseButton::Left,
        mods: ModSet::empty(),
        x: 32.0,
        y: 16.0,
    };
    let frame = session
        .input_frame(InputEvent::Mouse(press))
        .expect("mouse input is eligible");
    assert!(matches!(
        decode_one(&frame),
        FrameKind::InputMouse { event, .. } if event == press
    ));
}
