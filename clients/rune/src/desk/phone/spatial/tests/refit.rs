use super::*;
use phux_client_runtime::Client;
use phux_protocol::wire::frame::FrameKind;

fn resizes(client: &Client) -> Vec<(ResourceId, u16, u16)> {
    outbound(client)
        .into_iter()
        .filter_map(|frame| match frame {
            FrameKind::ResizeTerminal {
                terminal_id,
                cols,
                rows,
                ..
            } => Some((terminal_id, cols, rows)),
            _ => None,
        })
        .collect()
}

fn area(desk: &mut Desk, clock: f64, height: f32) {
    desk.clock = clock;
    desk.laid_out(id(1), [0.0, 0.0, 360.0, height]);
}

#[test]
fn a_phone_resize_burst_sends_only_the_settled_grid() {
    let (client, mut rt) = fixture(true);
    let desk = &mut rt.state;
    for (clock, height) in [(2.0, 500.0), (2.04, 420.0), (2.08, 320.0)] {
        area(desk, clock, height);
        assert!(
            resizes(&client).is_empty(),
            "an intermediate grid reached the PTY"
        );
    }
    // Repeated layouts at the same grid do not postpone its deadline.
    area(desk, 2.18, 320.0);
    assert!(resizes(&client).is_empty());
    desk.clock = 2.201;
    desk.fit();
    let (cols, rows) = desk.metrics().fit([360.0, 320.0]);
    assert_eq!(resizes(&client), [(id(1), cols, rows)]);
    desk.clock = 10.0;
    desk.fit();
    assert!(
        resizes(&client).is_empty(),
        "one settled proposal, not a timer loop"
    );
}

#[test]
fn a_phone_round_trip_back_to_its_grid_never_resizes_the_pty() {
    let (client, mut rt) = fixture(true);
    let desk = &mut rt.state;
    let [w, h] = desk.areas[&id(1)];
    let fitted = desk.fitted[&id(1)];
    area(desk, 2.0, 500.0);
    desk.clock = 2.05;
    desk.laid_out(id(1), [0.0, 0.0, w, h]);
    desk.clock = 3.0;
    desk.fit();
    assert_eq!(desk.fitted[&id(1)], fitted);
    assert!(resizes(&client).is_empty());
}

#[test]
fn desktop_resizes_and_a_phones_first_grid_do_not_wait() {
    let (client, mut rt) = fixture(false);
    area(&mut rt.state, 2.0, 500.0);
    assert!(!resizes(&client).is_empty());
    let (client, mut rt) = fixture(true);
    rt.state.fitted.clear();
    area(&mut rt.state, 2.0, 500.0);
    assert_eq!(
        resizes(&client).len(),
        1,
        "an opening terminal fits immediately"
    );
}

#[test]
fn navigating_during_a_pending_fit_never_resizes_the_old_pane() {
    let (client, mut rt) = fixture(true);
    area(&mut rt.state, 2.0, 500.0);
    assert!(resizes(&client).is_empty());
    rt.state.focus_pane(&id(2));
    rt.state.clock = 3.0;
    rt.state.laid_out(id(2), [0.0, 0.0, 360.0, 320.0]);
    let sizes = resizes(&client);
    assert_eq!(sizes.len(), 1);
    assert_eq!(sizes[0].0, id(2));
}

#[test]
fn phone_layout_arms_a_deadline_and_poll_fits_without_another_layout_or_input() {
    let (client, mut rt) = fixture(true);
    rt.poll = Some(Desk::poll);
    rt.frame(0.01);
    resizes(&client);
    rt.set_viewport((393.0, 700.0), 2.0);
    // The nested spatial stage measures, rebuilds its pane, then delivers
    // that pane's queued layout after poll. The following poll arms its wake.
    for _ in 0..4 {
        rt.frame(0.01);
        assert!(resizes(&client).is_empty());
    }
    let deadline = rt
        .wake_deadline()
        .expect("an idle terminal must wake for its fit");
    assert!(deadline > rt.state.clock && deadline <= rt.state.clock + 0.12);
    rt.frame(deadline - rt.state.clock - 0.005);
    assert!(resizes(&client).is_empty());
    rt.frame(0.01);
    let sizes = resizes(&client);
    assert_eq!(sizes.len(), 1);
    let expected = rt.state.metrics().fit(rt.state.areas[&id(1)]);
    assert_eq!(sizes[0], (id(1), expected.0, expected.1));
    rt.frame(1.0);
    assert!(resizes(&client).is_empty());
}

#[test]
fn moving_a_phone_pane_without_changing_its_grid_does_not_restart_the_wait() {
    let (client, mut rt) = fixture(true);
    let desk = &mut rt.state;
    area(desk, 2.0, 320.0);
    desk.clock = 2.1;
    desk.laid_out(id(1), [20.0, 40.0, 360.1, 320.1]);
    assert!(resizes(&client).is_empty());
    desk.clock = 2.121;
    desk.fit();
    assert_eq!(resizes(&client).len(), 1);
    assert_eq!(
        desk.origins[&id(1)],
        [20.0, 40.0],
        "hit testing moves immediately"
    );
}

#[test]
fn a_native_viewport_change_cannot_release_the_expired_previous_measurement() {
    let (client, mut rt) = fixture(true);
    area(&mut rt.state, 2.0, 500.0);
    assert!(resizes(&client).is_empty());
    // As in the bridge: UIKit changed bounds, but Rune's queued on_layout
    // has not arrived. Even an already-due proposal must not go out now.
    crate::mobile::viewport_changed(&mut rt);
    rt.set_viewport((393.0, 700.0), 2.0);
    rt.state.clock = 2.2;
    rt.state.fit();
    assert!(resizes(&client).is_empty());
    assert!(
        rt.state.areas.contains_key(&id(1)),
        "a sub-threshold change may deliver no layout"
    );
    area(&mut rt.state, 2.21, 320.0);
    assert!(resizes(&client).is_empty());
    rt.state.clock = 2.331;
    rt.state.fit();
    let (cols, rows) = rt.state.metrics().fit([360.0, 320.0]);
    assert_eq!(resizes(&client), [(id(1), cols, rows)]);
}

#[test]
fn native_sub_threshold_and_scale_only_changes_keep_a_usable_measurement() {
    let (client, mut rt) = fixture(true);
    rt.poll = Some(Desk::poll);
    rt.frame(0.01);
    resizes(&client);
    let initial = rt.state.areas[&id(1)];
    // Rune suppresses on_layout for a sub-half-point rect change. This is
    // also the native layer's pixel-rounded, effectively unchanged case.
    crate::mobile::viewport_changed(&mut rt);
    rt.set_viewport((393.01, 852.0), 2.0);
    for _ in 0..20 {
        rt.frame(0.01);
    }
    assert_eq!(rt.state.areas[&id(1)], initial);
    assert!(resizes(&client).is_empty());

    // A device-pixel scale change need not move a pane in points either,
    // but its cell pixels must still reach the PTY after settling.
    let before = rt.state.fitted[&id(1)];
    crate::mobile::viewport_changed(&mut rt);
    rt.set_viewport((393.01, 852.0), 3.0);
    for _ in 0..20 {
        rt.frame(0.01);
    }
    assert!(rt.state.areas.contains_key(&id(1)));
    assert_ne!(rt.state.fitted[&id(1)].1, before.1);
    assert!(!resizes(&client).is_empty());

    // A later pinch/font change still has an area to fit, even without
    // another viewport layout. Never leave the terminal permanently stuck.
    rt.update(|d, _| d.zoom(Some(d.font_size + 2.0)));
    assert!(!resizes(&client).is_empty());
}

#[test]
fn using_the_desktop_takes_back_a_pane_another_client_resized() {
    let (client, mut rt) = fixture(false);
    let desk = &mut rt.state;
    let (cols, rows) = desk.fitted[&id(1)].0;
    // The fixture's server grid stays 80x24: some other client's size.
    assert_ne!((cols, rows), (80, 24));
    resizes(&client);
    desk.fit();
    assert!(
        resizes(&client).is_empty(),
        "a plain fit trusts its last proposal"
    );
    desk.reclaim_fits();
    assert!(resizes(&client).contains(&(id(1), cols, rows)));
}
