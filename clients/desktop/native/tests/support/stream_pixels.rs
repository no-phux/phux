//! Main-thread GPU fixture support; shared runtime and pixel helpers live in pixels.rs.

use super::*;

/// Streaming output under a painting terminal refills the buffers of frames
/// it already painted. The scene, the paint report and the link layer each
/// keep the last painted frame, so the runtime must not need the frame it
/// just replaced to be free.
pub(super) fn streaming_output_recycles_frame_buffers() {
    let (client, view) = publish_at(b"ready", 80, 24);
    let mut cx = gpu();
    let in_view = Arc::clone(&client);
    let window = cx
        .open_window(size(px(960.), px(600.)), move |window, cx| {
            cx.new(|cx| Terminal::new(in_view, view, terminal(), settings(false), window, cx))
        })
        .expect("offscreen window");
    let mut seq = 0_u64;
    let mut tick = |cx: &mut HeadlessAppContext| {
        seq += 1;
        client
            .feed(FrameKind::ResourceOutput {
                terminal_id: terminal(),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq,
                bytes: format!("line {seq}\r\n").into_bytes().into(),
            })
            .expect("live output");
        // The desk notifies a terminal its runtime changed; the view is
        // otherwise drawn from the last frame.
        window.update(cx, |_, _, cx| cx.notify()).expect("notify");
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .expect("draw");
    };
    // The first publications start the retired list.
    for _ in 0..4 {
        tick(&mut cx);
    }
    let published = phux_client_runtime::perf::PUBLISHED.get();
    let fresh = phux_client_runtime::perf::BUFFER_FRESH.get();
    for _ in 0..32 {
        tick(&mut cx);
    }
    drop(tick);
    let published = phux_client_runtime::perf::PUBLISHED.get() - published;
    assert!(published >= 32, "only {published} publications");
    assert_eq!(
        phux_client_runtime::perf::BUFFER_FRESH.get() - fresh,
        0,
        "a steady stream must reuse retired buffers"
    );
    let painted = window
        .update(&mut cx, |terminal, _, _| terminal.painted())
        .expect("painted");
    assert!(
        painted.frame.expect("frame").text().contains("line 36"),
        "the last output must be painted"
    );
    drop(cx);
    drop(client);
}

pub(super) fn sparse_output_repaints_a_row_without_disturbing_its_neighbor() {
    let (client, view) = publish(b"hello\r\nworld");
    let mut cx = gpu();
    let in_view = Arc::clone(&client);
    let window = cx
        .open_window(size(px(480.), px(200.)), move |window, cx| {
            cx.new(|cx| Terminal::new(in_view, view, terminal(), settings(false), window, cx))
        })
        .expect("offscreen window");
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
        .expect("first draw");
    let before = Shot {
        painted: window
            .update(&mut cx, |terminal, _, _| terminal.painted())
            .unwrap(),
        image: cx.capture_screenshot(window.into()).expect("first pixels"),
    };
    let patched = phux_desktop::perf::PREPARE_PATCHED.get();
    client
        .feed(FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq: 1,
            bytes: b"\x1b[2;1HWORLD".to_vec().into(),
        })
        .expect("live output");
    window.update(&mut cx, |_, _, cx| cx.notify()).unwrap();
    cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
        .expect("updated draw");
    let after = Shot {
        painted: window
            .update(&mut cx, |terminal, _, _| terminal.painted())
            .unwrap(),
        image: cx
            .capture_screenshot(window.into())
            .expect("updated pixels"),
    };
    assert_eq!(phux_desktop::perf::PREPARE_PATCHED.get(), patched + 1);
    let frame = after.painted.frame.as_ref().unwrap();
    assert_eq!(frame.damage, GridDamage::Rows);
    assert_eq!(frame.dirty_rows().collect::<Vec<_>>(), vec![1]);
    assert_eq!(frame.row_text(1), "WORLD");
    assert_eq!(cell_pixels(&before, 0, 0), cell_pixels(&after, 0, 0));
    assert_ne!(cell_pixels(&before, 1, 0), cell_pixels(&after, 1, 0));
    drop(cx);
    drop(client);
}

/// A wheel event between rows shifts the grid by the fraction and fills the
/// opened space with the row below the viewport, clipped to the grid.
pub(super) fn smooth_scroll_paints_the_row_below_the_viewport() {
    // Ten lines on a four-row screen: amber blocks on the ninth, the cursor
    // alone on the tenth. Scrolling 1.5 rows up puts the blocks just below
    // the viewport, half of them showing.
    let mut payload = Vec::new();
    for _ in 0..8 {
        payload.extend_from_slice(b"x\r\n");
    }
    payload.extend_from_slice(
        "\x1b[38;2;255;204;0m\u{2588}\u{2588}\u{2588}\u{2588}\x1b[m\r\n".as_bytes(),
    );
    let (client, view) = publish(&payload);
    let mut cx = gpu();
    let in_view = Arc::clone(&client);
    let window = cx
        .open_window(size(px(480.), px(200.)), move |window, cx| {
            cx.new(|cx| Terminal::new(in_view, view, terminal(), settings(false), window, cx))
        })
        .expect("offscreen window");
    let draw = |cx: &mut HeadlessAppContext| {
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .expect("draw");
        let painted = window
            .update(cx, |terminal, _, _| terminal.painted())
            .expect("painted");
        let image = cx.capture_screenshot(window.into()).expect("pixels");
        Shot { image, painted }
    };
    let wheel = |cx: &mut HeadlessAppContext,
                 rows: f32,
                 geometry: phux_desktop::terminal::geometry::Geometry| {
        let event = gpui::ScrollEvent {
            position: gpui::point(
                geometry.bounds.origin.x + geometry.cell_width / 2.,
                geometry.bounds.origin.y + geometry.cell_height / 2.,
            ),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), geometry.cell_height * rows)),
            ..Default::default()
        };
        cx.update_window(window.into(), |_, window, cx| {
            window.dispatch_event(gpui::PlatformInput::Scroll(event), cx);
        })
        .expect("wheel");
    };
    let tail = draw(&mut cx);
    let geometry = tail.painted.geometry;
    let tail_offset = tail.painted.frame.as_ref().unwrap().scrollbar.offset;
    assert_eq!(tail.painted.shift, px(0.));

    wheel(&mut cx, 1.5, geometry);
    let shot = draw(&mut cx);
    let frame = shot.painted.frame.as_ref().unwrap();
    assert_eq!(frame.scrollbar.offset, tail_offset - 2);
    assert_eq!(frame.buffer.fringe.len(), usize::from(frame.cols));
    let half = geometry.cell_height / 2.;
    assert!(
        (shot.painted.shift + half).abs() <= px(1.),
        "shift {:?}",
        shot.painted.shift
    );

    // The bottom of the grid shows the top of the amber fringe row, and
    // nothing is painted into the padding under the grid.
    let scale = geometry.scale;
    let bottom = geometry.bounds.origin.y + geometry.cell_height * f32::from(frame.rows);
    let band = |top: Pixels, bottom: Pixels| {
        let (left, top, right, bottom) = inset(
            device(geometry.bounds.origin.x, scale),
            device(top, scale),
            device(geometry.bounds.origin.x + geometry.cell_width * 4., scale),
            device(bottom, scale),
        );
        let mut pixels = Vec::new();
        for y in top..bottom {
            for x in left..right {
                pixels.push(shot.image.get_pixel(x, y).0);
            }
        }
        pixels
    };
    let reused = phux_desktop::perf::PREPARE_REUSED.get();
    assert_mostly(&band(bottom - half + px(1.), bottom), AMBER, "fringe row");
    assert_blank(
        &band(bottom + px(1.), bottom + geometry.cell_height),
        "padding under the grid",
    );

    // A move within the row keeps the frame and its scene.
    wheel(&mut cx, -0.25, geometry);
    let within = draw(&mut cx);
    assert!(Arc::ptr_eq(within.painted.frame.as_ref().unwrap(), frame));
    assert!(phux_desktop::perf::PREPARE_REUSED.get() > reused);
    let quarter = geometry.cell_height * 0.75;
    assert!((within.painted.shift + quarter).abs() <= px(1.));
    drop(cx);
    drop(client);
}

/// Scrolling history row by row moves the fringe with the viewport instead
/// of shaping it again. Those moved rows must paint exactly what one long
/// scroll, which shapes its fringe fresh, paints at the same position.
pub(super) fn stepped_history_scroll_paints_like_one_long_scroll() {
    let mut payload = Vec::new();
    for line in 0..10 {
        payload
            .extend_from_slice(format!("\x1b[3{}mline {line}\x1b[m\r\n", line % 7 + 1).as_bytes());
    }
    let (client, stepped_view) = publish(&payload);
    let long_view = client.create_view(&terminal()).expect("second view");
    let mut cx = gpu();
    let open = |cx: &mut HeadlessAppContext, view| {
        let in_view = Arc::clone(&client);
        let window = cx
            .open_window(size(px(480.), px(200.)), move |window, cx| {
                cx.new(|cx| Terminal::new(in_view, view, terminal(), settings(false), window, cx))
            })
            .expect("offscreen window");
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .expect("first draw");
        window
    };
    let stepped = open(&mut cx, stepped_view);
    let long = open(&mut cx, long_view);
    let geometry = |cx: &mut HeadlessAppContext, window: gpui::WindowHandle<Terminal>| {
        window
            .update(cx, |terminal, _, _| terminal.painted())
            .expect("painted")
            .geometry
    };
    let at = geometry(&mut cx, stepped);
    let reused = phux_desktop::perf::PREPARE_PATCHED.get();
    for rows in [1., 1., 1.5] {
        wheel_step(&mut cx, stepped, at, rows);
    }
    assert!(
        phux_desktop::perf::PREPARE_PATCHED.get() >= reused + 3,
        "each step patches the previous scene"
    );
    let at = geometry(&mut cx, long);
    wheel_step(&mut cx, long, at, 3.5);
    let painted = |cx: &mut HeadlessAppContext, window: gpui::WindowHandle<Terminal>| {
        let painted = window
            .update(cx, |terminal, _, _| terminal.painted())
            .expect("painted");
        let image = cx.capture_screenshot(window.into()).expect("pixels");
        (painted, image)
    };
    let (stepped_paint, stepped_image) = painted(&mut cx, stepped);
    let (long_paint, long_image) = painted(&mut cx, long);
    let offset = |paint: &phux_desktop::terminal::PaintedFrame| {
        paint.frame.as_ref().expect("frame").scrollbar.offset
    };
    assert_eq!(offset(&stepped_paint), offset(&long_paint));
    assert_eq!(stepped_paint.shift, long_paint.shift);
    assert_ne!(stepped_paint.shift, px(0.), "the fringe is in view");
    assert!(
        stepped_image.as_raw() == long_image.as_raw(),
        "a moved fringe must paint like a freshly shaped one"
    );
    drop(cx);
    drop(client);
}

/// One trackpad-sized wheel step over the pane's first cell, `rows` rows
/// toward history, through the window's real event dispatch. The step's
/// notify leaves the window dirty, and GPUI's test mode draws a dirty window
/// when the update's effects flush, so the step includes its draw.
pub(super) fn wheel_step(
    cx: &mut HeadlessAppContext,
    window: gpui::WindowHandle<Terminal>,
    geometry: phux_desktop::terminal::geometry::Geometry,
    rows: f32,
) {
    let event = gpui::ScrollEvent {
        position: gpui::point(
            geometry.bounds.origin.x + geometry.cell_width / 2.,
            geometry.bounds.origin.y + geometry.cell_height / 2.,
        ),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), geometry.cell_height * rows)),
        ..Default::default()
    };
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_event(gpui::PlatformInput::Scroll(event), cx);
    })
    .expect("wheel");
}
