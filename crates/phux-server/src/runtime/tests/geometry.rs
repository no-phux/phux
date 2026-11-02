use super::*;

/// `VIEWPORT_RESIZE` updates the registry dims and asks the actor for a
/// resize with a client resync; pixel reports resolve the cell size.
/// Unattached clients are ignored.
#[test]
fn viewport_resize_updates_dims_and_queues_actor_resize() {
    let state = SharedState::new();
    let (sid, pid) = state.with_mut(|s| s.seed_session("test-session"));
    let mut rx = register_stub(&state, pid);
    let dims = || {
        state
            .with(|s| s.registry().terminal(pid).map(|p| p.dims))
            .expect("pane exists")
    };

    handle_viewport_resize(&state, ClientId(9999), &ViewportInfo::new(200, 60));
    assert_eq!(dims(), (80, 24), "unattached client must not mutate");
    assert!(rx.resize.try_recv().is_err());

    let client_id = attach_client(&state, "test-session");
    handle_viewport_resize(&state, client_id, &ViewportInfo::new(132, 50));
    assert_eq!(dims(), (132, 50));
    let observed = rx.resize.try_recv().expect("resize queued");
    assert_eq!((observed.cols, observed.rows), (132, 50));
    assert!(observed.resync_clients, "a live resize resyncs clients");
    assert_eq!(observed.cell_px, None, "no pixels, no invented cell size");
    assert!(rx.resize.try_recv().is_err(), "exactly one resize");

    // 1320x750 px over 132x50 cells -> 10x15 px cells.
    let viewport = ViewportInfo::new(132, 50).with_pixels(Some(1320), Some(750));
    handle_viewport_resize(&state, client_id, &viewport);
    assert_eq!(
        rx.resize.try_recv().expect("resize").cell_px,
        Some((10, 15))
    );
    assert_eq!(
        state.with(|s| s.attached().get(&client_id).map(|c| c.session)),
        Some(sid)
    );
}

/// ADR-0145: `RESIZE_TERMINAL` carries a cell size to the actor when it
/// has one, and `None` (actor keeps its last cell size) when it has none
/// or a degenerate one.
#[test]
fn terminal_resize_forwards_cell_px_or_keeps_the_last() {
    let state = SharedState::new();
    let (_, pane) = state.with_mut(|s| s.seed_session("home"));
    let mut rx = register_stub(&state, pane);
    let client = attach_client(&state, "home");
    let wire = state.with_mut(|s| s.intern_resource_wire(pane));

    handle_terminal_resize(&state, client, &wire, (100, 40), Some((9, 18)));
    let sized = rx.resize.try_recv().expect("resize queued");
    assert_eq!(
        (sized.cols, sized.rows, sized.cell_px),
        (100, 40, Some((9, 18)))
    );

    handle_terminal_resize(&state, client, &wire, (90, 30), None);
    assert_eq!(rx.resize.try_recv().expect("resize").cell_px, None);

    handle_terminal_resize(&state, client, &wire, (90, 30), Some((0, 18)));
    assert_eq!(rx.resize.try_recv().expect("resize").cell_px, None);
    assert_eq!(
        state.with(|s| s.registry().terminal(pane).unwrap().dims),
        (90, 30)
    );
}

/// ADR-0145 regression: a pane-sizing client (the TUI) attaches with no
/// vote and sizes the pane once with its tile; detaching and re-attaching
/// (a sidebar session switch) never resizes it. A legacy outer-window
/// vote resized the pane to the full window first, then to the tile, and
/// the departing vote shrank it to the headless size: three PTY sizes per
/// switch, each tearing output written for the previous width.
#[test]
fn pane_sizing_attach_resizes_each_pane_at_most_once() {
    let state = SharedState::new();
    let (_, pane) = state.with_mut(|s| s.seed_session("home"));
    let mut rx = register_stub(&state, pane);
    let target = || {
        state.with_mut(|s| crate::state::AttachSnapshotPane {
            terminal_id: pane,
            handle: s.resource_handle(pane).unwrap().clone(),
            wire_terminal_id: s.intern_resource_wire(pane),
        })
    };
    let no_vote = ViewportInfo::new(0, 0);
    let mut sizes = Vec::new();
    let mut drain = |rx: &mut StubRx| {
        while let Ok(request) = rx.resize.try_recv() {
            sizes.push((request.cols, request.rows));
        }
    };

    let client = attach_client(&state, "home");
    attach::apply_attach_viewport(&state, client, &[target()], no_vote);
    handle_terminal_resize(
        &state,
        client,
        &target().wire_terminal_id,
        (116, 37),
        Some((9, 18)),
    );
    drain(&mut rx);
    // A session switch: detach, then attach again the same way.
    state.with_mut(|s| s.detach(client));
    drain(&mut rx);
    let again = attach_client(&state, "home");
    attach::apply_attach_viewport(&state, again, &[target()], no_vote);
    drain(&mut rx);

    assert_eq!(sizes, [(116, 37)], "one tile resize, never the full window");
    assert_eq!(
        state.with(|s| s.registry().terminal(pane).unwrap().dims),
        (116, 37)
    );
}

/// ADR-0145: a vote-free detach that leaves a pane unwatched and below
/// the usable minimum returns it to the headless size; a normally sized
/// pane keeps its tile, so a GUI or TUI relaunch reflows nothing.
#[test]
fn vote_free_detach_resets_only_an_unusably_small_pane() {
    let state = SharedState::new();
    let (window, tiny) = state.with_mut(|s| s.seed_session("home"));
    let normal = state.with_mut(|s| s.add_pane_to_session(window).unwrap());
    let mut tiny_rx = register_stub(&state, tiny);
    let mut normal_rx = register_stub(&state, normal);
    let client = attach_client(&state, "home");
    let (tiny_wire, normal_wire) =
        state.with_mut(|s| (s.intern_resource_wire(tiny), s.intern_resource_wire(normal)));
    handle_terminal_resize(&state, client, &tiny_wire, (1, 1), None);
    handle_terminal_resize(&state, client, &normal_wire, (116, 37), None);
    tiny_rx.resize.try_recv().expect("tile resize");
    normal_rx.resize.try_recv().expect("tile resize");

    state.with_mut(|s| s.detach(client));

    let reset = tiny_rx.resize.try_recv().expect("tiny pane reset");
    assert_eq!(
        (reset.cols, reset.rows),
        crate::state::HEADLESS_TERMINAL_DIMS
    );
    assert!(
        normal_rx.resize.try_recv().is_err(),
        "normal pane untouched"
    );
    assert_eq!(
        state.with(|s| s.registry().terminal(normal).unwrap().dims),
        (116, 37)
    );
}

#[test]
fn viewport_resize_fans_out_only_to_live_subscribed_session_panes() {
    let state = SharedState::new();
    let (window, active) = state.with_mut(|s| s.seed_session("home"));
    let inactive = state.with_mut(|s| s.add_pane_to_session(window).unwrap());
    let retained = state.with_mut(|s| s.add_pane_to_session(window).unwrap());
    let (_, foreign) = state.with_mut(|s| s.seed_session("foreign"));
    let unsubscribed = state.with_mut(|s| s.add_pane_to_session(window).unwrap());
    let mut receivers: Vec<_> = [active, inactive, retained, foreign, unsubscribed]
        .into_iter()
        .map(|pane| register_stub(&state, pane))
        .collect();
    let client = attach_client(&state, "home");
    state.with_mut(|s| {
        s.subscribe_resource(client, foreign, None);
        s.unsubscribe_resource(client, unsubscribed);
        s.restore_retained_exit(retained, phux_protocol::wire::info::ExitFacet::new(1, 1000));
    });
    let foreign_wire = state.with_mut(|s| s.intern_resource_wire(foreign));
    handle_terminal_resize(&state, client, &foreign_wire, (137, 57), None);
    receivers[3]
        .resize
        .try_recv()
        .expect("foreign exact geometry");
    for cols in 1..=512 {
        handle_viewport_resize(&state, client, &ViewportInfo::new(cols, 53));
    }
    for index in [0, 1] {
        let request = receivers[index].resize.try_recv().expect("final geometry");
        assert_eq!((request.cols, request.rows), (512, 53));
    }
    for index in [2, 3, 4] {
        assert!(receivers[index].resize.try_recv().is_err());
    }
    assert_eq!(
        state.with(|s| s.registry().terminal(retained).unwrap().dims),
        (80, 24)
    );
    assert_eq!(
        state.with(|s| s.registry().terminal(unsubscribed).unwrap().dims),
        (80, 24)
    );
    assert_eq!(
        state.with(|s| s.registry().terminal(foreign).unwrap().dims),
        (137, 57)
    );
    state.with_mut(|s| s.detach(client));
    assert!(
        receivers[3].resize.try_recv().is_err(),
        "detach cannot reflow a foreign resource subscription"
    );
    assert!(
        receivers[4].resize.try_recv().is_err(),
        "detach cannot reflow an unsubscribed session pane"
    );
    assert_eq!(
        state.with(|s| s.registry().terminal(foreign).unwrap().dims),
        (137, 57)
    );
    assert_eq!(
        state.with(|s| s.registry().terminal(retained).unwrap().dims),
        (80, 24)
    );
}

#[test]
fn viewport_resize_preserves_policy_and_zero_vote_protection() {
    use phux_config::WindowSize;
    let state = SharedState::new();
    let (_, pane) = state.with_mut(|s| s.seed_session("home"));
    let mut receiver = register_stub(&state, pane);
    let small = attach_client(&state, "home");
    let large = attach_client(&state, "home");
    state.with_mut(|s| s.set_client_viewport(small, ViewportInfo::new(70, 20)));
    for (policy, expected) in [
        (WindowSize::Smallest, (70, 20)),
        (WindowSize::Largest, (140, 50)),
        (WindowSize::Latest, (140, 50)),
    ] {
        state.with_mut(|s| s.set_window_size(policy));
        handle_viewport_resize(&state, large, &ViewportInfo::new(140, 50));
        let request = receiver.resize.try_recv().expect("policy geometry");
        assert_eq!((request.cols, request.rows), expected);
    }
    let wire = state.with_mut(|s| s.intern_resource_wire(pane));
    handle_terminal_resize(&state, large, &wire, (99, 33), None);
    receiver.resize.try_recv().expect("exact resize");
    state.with_mut(|s| s.set_window_size(WindowSize::Smallest));
    handle_viewport_resize(&state, large, &ViewportInfo::new(0, 50));
    assert!(
        receiver.resize.try_recv().is_err(),
        "zero report cannot resize from another vote"
    );
    assert_eq!(
        state.with(|s| s.registry().terminal(pane).unwrap().dims),
        (99, 33)
    );
    state.with_mut(|s| s.set_window_size(WindowSize::Manual));
    handle_viewport_resize(&state, large, &ViewportInfo::new(200, 60));
    assert!(receiver.resize.try_recv().is_err());
    assert_eq!(
        state.with(|s| s.registry().terminal(pane).unwrap().dims),
        (99, 33)
    );
}

#[test]
fn closed_resize_delivery_never_advances_registry_geometry() {
    let state = SharedState::new();
    let (_, pane) = state.with_mut(|s| s.seed_session("home"));
    drop(register_stub(&state, pane));
    let client = attach_client(&state, "home");
    let target = state.with_mut(|s| crate::state::AttachSnapshotPane {
        terminal_id: pane,
        handle: s.resource_handle(pane).unwrap().clone(),
        wire_terminal_id: s.intern_resource_wire(pane),
    });
    handle_viewport_resize(&state, client, &ViewportInfo::new(140, 50));
    assert_eq!(
        state.with(|s| s.registry().terminal(pane).unwrap().dims),
        (80, 24)
    );
    handle_terminal_resize(&state, client, &target.wire_terminal_id, (99, 33), None);
    assert_eq!(
        state.with(|s| s.registry().terminal(pane).unwrap().dims),
        (80, 24)
    );
    attach::apply_attach_viewport(&state, client, &[target], ViewportInfo::new(100, 40));
    assert_eq!(
        state.with(|s| s.registry().terminal(pane).unwrap().dims),
        (80, 24)
    );
}
