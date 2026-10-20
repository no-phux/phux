/// Closing the floating overlay (ADR-0147) with `kill-pane` leaves every tile
/// where it was, yet its box covered them: the view must clear and recompose,
/// or the box frame and the covered cells stay on screen.
#[tokio::test(flavor = "current_thread")]
async fn closing_the_floating_overlay_clears_its_box() {
    let float = ResourceId::local(3);
    let (mut state, mut client, server, mut out) =
        side_by_side_loop(&[(&float, 60, 18, b"overlay pane\r\n")]).await;
    state
        .mirror
        .panes
        .get_mut(&float)
        .expect("floating slot")
        .floating = Some("Float Box".into());

    let sidebar = state.sidebar();
    out.clear();
    state
        .dispatch_batch(
            &mut client,
            &mut out,
            sidebar,
            vec![chord_press("C-a"), chord_press("x")],
        )
        .await
        .expect("dispatch");

    assert!(
        !state.mirror.panes.contains_key(&float),
        "kill-pane closed the overlay"
    );
    assert!(
        out.windows(4).any(|s| s == b"\x1b[2J"),
        "closing the overlay must clear its box: {:?}",
        String::from_utf8_lossy(&out)
    );
    drop(client);
    drop(server);
}

/// Dismissing the command palette leaves the tiles unchanged, but the modal
/// covered pane cells: the dismissal must recompose the view, not only move
/// the chrome, or the palette's frame and rows stay on screen.
#[tokio::test(flavor = "current_thread")]
async fn dismissing_the_palette_clears_its_box() {
    let (mut state, mut client, server, mut out) = side_by_side_loop(&[]).await;
    let sidebar = state.sidebar();
    state
        .dispatch_batch(
            &mut client,
            &mut out,
            sidebar,
            vec![chord_press("C-a"), chord_press("?")],
        )
        .await
        .expect("open palette");
    assert!(state.overlays.is_active(), "the palette is open");

    out.clear();
    state
        .dispatch_batch(&mut client, &mut out, sidebar, vec![chord_press("Escape")])
        .await
        .expect("dismiss palette");

    assert!(!state.overlays.is_active(), "Escape dismissed the palette");
    assert!(
        out.windows(4).any(|s| s == b"\x1b[2J"),
        "dismissing the palette must clear its box: {:?}",
        String::from_utf8_lossy(&out)
    );
    drop(client);
    drop(server);
}

/// A loop over one published 80x22 pane, with the shipped chrome, whose
/// layout read already settled: the view a held switch is waiting for.
async fn published_single_pane_loop() -> (SessionLoop, Connection, Connection, ResourceId) {
    let (mut state, client, server, _out) = bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let config = phux_config::parse_with_defaults("", std::path::Path::new("t.toml"))
        .expect("shipped defaults parse");
    state.settings = crate::settings::TuiSettings::tolerant_from(&config);
    state.viewport_dims = (80, 24);
    let a = ResourceId::local(1);
    let (kernel, _effects, panes) =
        crate::attach::pane_state::published_test_state(&[(&a, 80, 22, b"prompt$ ")]);
    state.mirror.engine_kernel = kernel;
    state.mirror.panes = panes;
    state.mirror.workspace = ArrangementDoc::single(a.clone());
    state.mirror.focused_resource = Some(a.clone());
    state.layout_read_complete = true;
    (state, client, server, a)
}

/// A session switch keeps the previous frame on the glass: nothing paints
/// while the new session's layout read is outstanding, and the release is
/// one synchronized clear-and-recompose, never a bare `ED2`.
#[tokio::test(flavor = "current_thread")]
async fn a_held_switch_paints_once_when_its_view_is_ready() {
    let (mut state, _client, _server, _a) = published_single_pane_loop().await;
    state.layout_read_complete = false;
    state.hold_paint_for_switch();
    let sidebar = state.sidebar();
    let mut out = Vec::new();
    state.repaint_view(&mut out, sidebar, RepaintLevel::Full);
    state.release_paint_hold_if_ready(&mut out, sidebar);
    assert!(out.is_empty(), "a held switch painted early: {out:?}");
    assert!(state.paint_held());

    state.layout_read_complete = true;
    state.release_paint_hold_if_ready(&mut out, sidebar);
    assert!(!state.paint_held());
    let painted = String::from_utf8_lossy(&out).into_owned();
    assert!(
        painted.starts_with("\x1b[?2026h") && painted.ends_with("\x1b[?2026l"),
        "the release is one synchronized frame: {painted:?}"
    );
    assert_eq!(painted.matches("\x1b[2J").count(), 1, "{painted:?}");
    assert!(painted.contains("prompt$"), "{painted:?}");
}

/// A view that never settles (no layout reply on a slow link) still paints
/// once the hold's deadline passes.
#[tokio::test(flavor = "current_thread")]
async fn a_held_switch_paints_at_its_deadline_regardless() {
    let (mut state, _client, _server, _a) = published_single_pane_loop().await;
    state.layout_read_complete = false;
    state.paint_hold = Some(tokio::time::Instant::now());
    let sidebar = state.sidebar();
    let mut out = Vec::new();
    state.release_paint_hold_if_ready(&mut out, sidebar);
    assert!(!state.paint_held());
    assert!(out.windows(4).any(|s| s == b"\x1b[2J"), "{out:?}");
}

/// A replaced layout over the tiles already on the glass (a pane's
/// re-bootstrap after a resize) redraws in place; one that moves a tile
/// still clears.
#[tokio::test(flavor = "current_thread")]
async fn a_replaced_layout_over_unchanged_tiles_repaints_in_place() {
    let (mut state, _client, _server, _a) = published_single_pane_loop().await;
    let sidebar = state.sidebar();
    let mut repaint = RepaintAccumulator::default();
    state.on_layout_replaced(sidebar, &mut repaint);
    assert_eq!(
        repaint.drain().level,
        RepaintLevel::Full,
        "nothing painted yet"
    );

    let mut out = Vec::new();
    state.repaint_view(&mut out, sidebar, RepaintLevel::Full);
    state.on_layout_replaced(sidebar, &mut repaint);
    assert_eq!(repaint.drain().level, RepaintLevel::Panes);

    state.viewport_dims = (100, 30);
    state.on_layout_replaced(state.sidebar(), &mut repaint);
    assert_eq!(repaint.drain().level, RepaintLevel::Full, "the tiles moved");
}

/// A switch hands the next entry what the server-wide sweeps knew: the
/// session being left joins the peer layouts, every agent record is kept,
/// and once ATTACHED names the new session its own panes' records move back
/// to the local index.
#[tokio::test(flavor = "current_thread")]
async fn a_switch_carries_the_peer_caches_into_the_next_entry() {
    let (mut old, _client, _server, _a) = published_single_pane_loop().await;
    // Not a pane id the fresh loop's bootstrap fixture uses.
    let a = ResourceId::local(5);
    let left = SessionId::new(1);
    let next = SessionId::new(2);
    let peer_pane = ResourceId::local(9);
    let record = |name: &str| AgentRecord {
        name: name.to_owned(),
        ..AgentRecord::default()
    };
    old.peers.focused_session = Some(left);
    old.peers.serving_host = Some("mini".to_owned());
    old.mirror
        .agent_meta
        .records
        .insert(a.clone(), record("local"));
    old.peers
        .foreign_agents
        .insert(peer_pane.clone(), record("peer"));
    old.peers
        .foreign_layouts
        .insert(next, ArrangementDoc::single(peer_pane.clone()));
    let carried = old.peers_for_switch();

    let (mut fresh, _client2, _server2, _out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    fresh.seed_carried_peers(carried);
    assert_eq!(fresh.peers.serving_host.as_deref(), Some("mini"));
    assert!(fresh.peers.foreign_layouts.contains_key(&left));
    fresh.peers.focused_session = Some(next);
    fresh.mirror.panes.insert(
        peer_pane.clone(),
        PaneSlot::new_with_size(80, 24).expect("slot"),
    );
    fresh.adopt_carried_local_agents();
    assert!(!fresh.peers.foreign_layouts.contains_key(&next));
    assert_eq!(
        fresh
            .mirror
            .agent_meta
            .records
            .get(&peer_pane)
            .map(|r| r.name.as_str()),
        Some("peer")
    );
    assert!(!fresh.peers.foreign_agents.contains_key(&peer_pane));
    assert_eq!(
        fresh.peers.foreign_agents.get(&a).map(|r| r.name.as_str()),
        Some("local"),
        "the left session's agent is a peer now"
    );
}
