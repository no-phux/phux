//! Settled PTY resizes and the visible-owner claim.

use super::*;
use phux_client_runtime::control::TerminalResizeOutcome;
use phux_client_runtime::publication::{
    Cursor, FrameColors, GridBuffer, GridDamage, GridFrame, Scrollbar,
};
use std::sync::Arc;

#[gpui::test]
fn a_hidden_owners_delayed_fit_does_not_override_the_focused_view(cx: &mut TestAppContext) {
    let client = session(None);
    let older = client.create_view(&terminal()).expect("hidden view");
    let focused = client.create_view(&terminal()).expect("focused view");
    cx.add_empty_window().update(|window, cx| {
        // Press, tab switch, and zoom all end here: the newly focused
        // writable view fits, and the previous owner is no longer painted.
        let hidden = bind(&client, older, window, cx);
        let shown = bind(&client, focused, window, cx);
        arm(&hidden, true, 800., 408., window, cx);
        arm(&shown, true, 400., 204., window, cx);
        settle(&hidden, cx);
        assert!(
            resizes(&client).is_empty(),
            "a delayed fit from the hidden owner must not resize"
        );
        settle(&shown, cx);
        assert_eq!(resizes(&client), vec![(50, 12)]);

        // Returning to the first view settles that view's geometry.
        arm(&hidden, true, 800., 408., window, cx);
        settle(&hidden, cx);
        assert_eq!(resizes(&client), vec![(100, 24)]);

        // A later paint that is no longer the owner drops its timer.
        arm(&hidden, true, 560., 272., window, cx);
        arm(&hidden, false, 560., 272., window, cx);
        settle(&hidden, cx);
        assert!(resizes(&client).is_empty());
    });
}

#[gpui::test]
fn blur_of_an_inactive_window_still_settles_the_visible_owner(cx: &mut TestAppContext) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    cx.add_empty_window().update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, true, 800., 408., window, cx);
        window.blur(cx);
        assert!(
            !window.is_window_active(),
            "occlusion and blur keep the visible owner"
        );
        input.update(cx, |state, _| {
            state.flush_fit();
            assert!(state.settling.is_some(), "the clock has not settled");
        });
        assert!(resizes(&client).is_empty());
        settle(&input, cx);
        assert_eq!(resizes(&client), vec![(100, 24)]);
    });
}

#[gpui::test]
fn a_resize_burst_sends_only_the_settled_proposal(cx: &mut TestAppContext) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    cx.add_empty_window().update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, true, 800., 408., window, cx);
        arm(&input, true, 640., 340., window, cx);
        input.update(cx, |state, _| state.flush_fit());
        assert!(resizes(&client).is_empty());
        settle(&input, cx);
        assert_eq!(resizes(&client), vec![(80, 20)]);
    });
}

#[gpui::test]
fn reconnect_drops_the_hidden_fit_until_the_new_generation_is_shown(cx: &mut TestAppContext) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    cx.add_empty_window().update(|window, cx| {
        let stale = bind(&client, view, window, cx);
        arm(&stale, true, 800., 408., window, cx);
        open_session(&client, 2);
        settle(&stale, cx);
        assert!(
            resizes(&client).is_empty(),
            "the pre-reconnect proposal is a replaced generation"
        );
        let view = client
            .create_view(&terminal())
            .expect("view after reconnect");
        let fresh = bind(&client, view, window, cx);
        arm(&fresh, true, 800., 408., window, cx);
        settle(&fresh, cx);
        assert_eq!(resizes(&client), vec![(100, 24)]);
    });
}

#[gpui::test]
fn a_viewer_is_not_resized(cx: &mut TestAppContext) {
    let client = session(Some(RolePolicy::VIEWER));
    let view = client.create_view(&terminal()).expect("view");
    cx.add_empty_window().update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, true, 800., 408., window, cx);
        settle(&input, cx);
        assert!(resizes(&client).is_empty());
    });
}

#[gpui::test]
fn a_settled_fit_does_not_acknowledge_an_unknown_delivery_fence(cx: &mut TestAppContext) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    let armed = fence(&client).expect("fence");
    cx.add_empty_window().update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, true, 800., 408., window, cx);
        settle(&input, cx);
        assert_eq!(resizes(&client), vec![(100, 24)]);
        let current = client.with_control(|control| control.projection_fence(&terminal()));
        assert_eq!(current, Some(armed));
    });
}

/// The painted pitch is stored on the first `fit`, before the terminal is
/// ready and for a view that does not own the size. The settled resize is
/// not what sizes the image.
#[gpui::test]
fn the_first_fit_applies_local_cell_pixels(cx: &mut TestAppContext) {
    let image = natural_column();
    cx.add_empty_window().update(|window, cx| {
        window.activate_window();
        for owner in [true, false] {
            unready_fit_sizes_the_snapshot(owner, &image, window, cx);
        }
        ready_fit_corrects_before_any_resize(false, None, &image, window, cx);
        ready_fit_corrects_before_any_resize(true, Some(RolePolicy::VIEWER), &image, window, cx);
    });
}

/// The reattach snapshot is published before any `fit`. The first paint must
/// already use the font pitch: `fit` runs only after that paint.
#[gpui::test]
fn a_snapshot_before_fit_paints_the_font_pitch(cx: &mut TestAppContext) {
    let client = session_with(None, &natural_column());
    let view = client.create_view(&terminal()).expect("view");
    let before = client.acquire_view(view).expect("snapshot frame");
    assert_eq!(
        before.images[0].rows,
        17.0 / 16.0,
        "the snapshot used the 8x16 replica, before fit"
    );
    let settings = crate::terminal::Settings {
        line_height: 2.,
        ..crate::terminal::Settings::default()
    };
    let (surface, cx) = cx.add_window_view(|window, cx| {
        crate::terminal::Terminal::new(client.clone(), view, terminal(), settings, window, cx)
    });
    // The render that acquires this frame stores the font pitch first. `fit`
    // runs in the paint callback, after the frame has been drawn.
    let painted = cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        surface.read(cx).painted()
    });
    assert!(
        resizes(&client).is_empty(),
        "storing the font pitch sends no resize"
    );
    let frame = painted.frame.expect("the first paint has a frame");
    let pitch = crate::terminal::geometry::cell_px(
        painted.geometry.cell_width,
        painted.geometry.cell_height,
    );
    assert_ne!(pitch.1, 16, "the font pitch is not the replica fallback");
    assert_eq!(frame.images.len(), 1);
    assert_eq!(
        (frame.images[0].image.width, frame.images[0].image.height),
        (1, 17)
    );
    assert_eq!(frame.images[0].rows, 17.0 / f64::from(pitch.1));
    assert_eq!(
        frame.generation,
        before.generation + 1,
        "one republish, acquired before fit"
    );
    assert_eq!(
        client.acquire_view(view).expect("view").generation,
        frame.generation
    );
    assert_eq!(image_rows(&client).0, frame.images[0].rows);
}

fn unready_fit_sizes_the_snapshot(
    owner: bool,
    image: &[u8],
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let client = Arc::new(Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("fit".into())),
        ..ControlOptions::default()
    }));
    let attach_id = negotiate(&client);
    assert_eq!(
        client.with_control(|control| {
            control.resize_terminal_with_cell(&terminal(), 20, 4, Some((8, 17)))
        }),
        TerminalResizeOutcome::NotReady
    );
    let focus = cx.focus_handle();
    let weak = Arc::downgrade(&client);
    let terminal_id = terminal();
    // Not `move`: `window` is only borrowed for `TerminalInput::new`, then
    // `fit` needs it again. The owned handles are still moved in.
    let input = cx.new(|_cx| {
        TerminalInput::new(
            weak,
            ViewId::from_raw(1).expect("view"),
            terminal_id,
            focus,
            window,
        )
    });
    input.update(cx, |state, cx| {
        let epoch = state.client().expect("client").connection_epoch();
        state
            .presented_at(epoch, &blank_frame(), metrics(160., 68.))
            .expect("metrics before the snapshot");
        state.fit(owner, window, cx);
    });
    assert!(resizes(&client).is_empty(), "the first fit sends nothing");
    assert!(
        bootstrap_output(&client, attach_id, 1, 20, 4, image).is_empty(),
        "publishing the snapshot sends no resize"
    );
    let (rows, generation) = image_rows(&client);
    assert_eq!(generation, 1, "the snapshot's first frame");
    assert_eq!(rows, 1.0, "17px at a 17px cell is one row");
    input.update(cx, |state, cx| state.fit(owner, window, cx));
    assert_eq!(
        image_rows(&client).1,
        generation,
        "a repeat fit does not republish"
    );
    assert!(resizes(&client).is_empty());
}

fn ready_fit_corrects_before_any_resize(
    owner: bool,
    role: Option<RolePolicy>,
    image: &[u8],
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let client = session_with(role, image);
    assert_eq!(
        image_rows(&client).0,
        17.0 / 16.0,
        "the replica still has the default cell height"
    );
    if role == Some(RolePolicy::VIEWER) {
        assert_eq!(
            client.with_control(|control| {
                control.resize_terminal_with_cell(&terminal(), 20, 4, Some((8, 17)))
            }),
            TerminalResizeOutcome::Observer
        );
        assert_eq!(image_rows(&client).0, 17.0 / 16.0);
    }
    let view = client.create_view(&terminal()).expect("view");
    let input = bind(&client, view, window, cx);
    let generation = image_rows(&client).1;
    arm(&input, owner, 160., 68., window, cx);
    let (rows, next) = image_rows(&client);
    assert_eq!(rows, 1.0);
    assert_eq!(next, generation + 1, "the first fit republishes once");
    assert!(
        resizes(&client).is_empty(),
        "local pitch is not a server resize"
    );
    arm(&input, owner, 160., 68., window, cx);
    assert_eq!(
        image_rows(&client).1,
        next,
        "the same pitch does not republish"
    );
    assert!(resizes(&client).is_empty());
}

fn image_rows(client: &Client) -> (f64, u64) {
    let frame = client.acquire(&terminal()).expect("frame");
    assert_eq!(frame.images.len(), 1);
    assert_eq!(
        (frame.images[0].image.width, frame.images[0].image.height),
        (1, 17)
    );
    (frame.images[0].rows, frame.generation)
}

fn blank_frame() -> Arc<GridFrame> {
    Arc::new(GridFrame {
        terminal_id: terminal(),
        generation: 1,
        stream_id: 1,
        bootstrap_id: 1,
        last_seq: 0,
        cols: 20,
        rows: 4,
        cursor: Cursor::default(),
        scrollbar: Scrollbar::default(),
        colors: FrameColors::default(),
        damage: GridDamage::Full,
        images: Vec::new(),
        buffer: GridBuffer::default(),
    })
}

/// One column, 17 pixels, no kitty rows or columns.
fn natural_column() -> Vec<u8> {
    let header = "\x1b_Ga=T,f=24,s=1,v=17,i=1,q=2,C=1;";
    assert!(!header.contains(",c=") && !header.contains(",r="));
    let mut apc = header.as_bytes().to_vec();
    apc.extend_from_slice(base64(&[255, 0, 0].repeat(17)).as_bytes());
    apc.extend_from_slice(b"\x1b\\");
    apc
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    let (chunks, rest) = bytes.as_chunks::<3>();
    for chunk in chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        out.extend([
            TABLE[((n >> 18) & 0x3f) as usize],
            TABLE[((n >> 12) & 0x3f) as usize],
            TABLE[((n >> 6) & 0x3f) as usize],
            TABLE[(n & 0x3f) as usize],
        ]);
    }
    if !rest.is_empty() {
        let n = (u32::from(rest[0]) << 16) | (u32::from(rest.get(1).copied().unwrap_or(0)) << 8);
        let pad = if rest.len() == 1 {
            b'='
        } else {
            TABLE[((n >> 6) & 0x3f) as usize]
        };
        out.extend([
            TABLE[((n >> 18) & 0x3f) as usize],
            TABLE[((n >> 12) & 0x3f) as usize],
            pad,
            b'=',
        ]);
    }
    String::from_utf8(out).expect("base64 is ascii")
}
