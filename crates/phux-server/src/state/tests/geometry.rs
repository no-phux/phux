//! Window-size policy and geometry resolution across attached views.

use super::*;

#[test]
fn resolve_geometry_applies_window_size_policy_across_subscribers() {
    use phux_config::WindowSize;
    use phux_protocol::wire::frame::ViewportInfo;

    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let big = s.new_client_id();
    let small = s.new_client_id();
    s.attach_default_caps(big, "default", mk_tx()).unwrap();
    s.attach_default_caps(small, "default", mk_tx()).unwrap();
    s.set_client_viewport(big, ViewportInfo::new(120, 48));
    s.set_client_viewport(small, ViewportInfo::new(80, 24));

    // smallest: per-axis min — nothing cropped.
    s.set_window_size(WindowSize::Smallest);
    assert_eq!(s.resolve_terminal_geometry(pid, None), Some((80, 24)));

    // largest: per-axis max.
    s.set_window_size(WindowSize::Largest);
    assert_eq!(s.resolve_terminal_geometry(pid, None), Some((120, 48)));

    // latest: the resizing client's viewport.
    s.set_window_size(WindowSize::Latest);
    assert_eq!(
        s.resolve_terminal_geometry(pid, Some(ViewportInfo::new(100, 30))),
        Some((100, 30)),
    );

    // manual: geometry is never derived from views.
    s.set_window_size(WindowSize::Manual);
    assert_eq!(
        s.resolve_terminal_geometry(pid, Some(ViewportInfo::new(100, 30))),
        None
    );

    // A zero-dimension viewport is ignored, so it can't collapse the grid.
    s.set_window_size(WindowSize::Smallest);
    s.set_client_viewport(small, ViewportInfo::new(0, 0));
    assert_eq!(s.resolve_terminal_geometry(pid, None), Some((120, 48)));
}

#[test]
fn detach_uses_remaining_views_then_restores_usable_headless_geometry() {
    use phux_protocol::wire::frame::ViewportInfo;

    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let bundle = phux_terminal::actor::TerminalActor::new(80, 24).unwrap();
    s.register_resource_handle(pid, bundle.handle.clone(), bundle.token);
    let big = s.new_client_id();
    let tiny = s.new_client_id();
    s.attach_default_caps(big, "default", mk_tx()).unwrap();
    s.attach_default_caps(tiny, "default", mk_tx()).unwrap();
    s.set_client_viewport(big, ViewportInfo::new(120, 48));
    s.set_client_viewport(tiny, ViewportInfo::new(1, 1));
    s.registry_mut().terminal_mut(pid).unwrap().dims = (1, 1);

    s.detach(tiny);
    assert_eq!(s.registry().terminal(pid).unwrap().dims, (120, 48));
    s.detach(big);
    assert_eq!(
        s.registry().terminal(pid).unwrap().dims,
        HEADLESS_TERMINAL_DIMS
    );
}

#[test]
fn a_client_without_a_geometry_vote_leaves_explicit_geometry_on_detach() {
    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let gui = s.new_client_id();
    s.attach_default_caps(gui, "default", mk_tx()).unwrap();
    // A zero ATTACH viewport records nothing; the GUI sized the pane itself.
    s.registry_mut().terminal_mut(pid).unwrap().dims = (137, 53);

    s.detach(gui);
    assert_eq!(s.registry().terminal(pid).unwrap().dims, (137, 53));
}

#[test]
fn manual_policy_holds_explicit_geometry_after_last_detach() {
    use phux_config::WindowSize;
    use phux_protocol::wire::frame::ViewportInfo;

    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let client = s.new_client_id();
    s.attach_default_caps(client, "default", mk_tx()).unwrap();
    s.set_client_viewport(client, ViewportInfo::new(1, 1));
    s.registry_mut().terminal_mut(pid).unwrap().dims = (137, 53);
    s.set_window_size(WindowSize::Manual);

    s.detach(client);
    assert_eq!(s.registry().terminal(pid).unwrap().dims, (137, 53));
}

#[test]
fn latest_policy_uses_the_most_recent_remaining_view_after_detach() {
    use phux_config::WindowSize;
    use phux_protocol::wire::frame::ViewportInfo;

    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let bundle = phux_terminal::actor::TerminalActor::new(80, 24).unwrap();
    s.register_resource_handle(pid, bundle.handle.clone(), bundle.token);
    let recent = s.new_client_id();
    let departing = s.new_client_id();
    s.attach_default_caps(recent, "default", mk_tx()).unwrap();
    s.attach_default_caps(departing, "default", mk_tx())
        .unwrap();
    s.set_client_viewport(recent, ViewportInfo::new(111, 37));
    s.set_client_viewport(departing, ViewportInfo::new(70, 20));
    s.set_window_size(WindowSize::Latest);

    s.detach(departing);
    assert_eq!(s.registry().terminal(pid).unwrap().dims, (111, 37));
}

#[test]
fn resolve_cell_px_prefers_most_recent_usable_pixel_report() {
    use phux_protocol::wire::frame::ViewportInfo;

    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let retina = s.new_client_id();
    let lodpi = s.new_client_id();
    s.attach_default_caps(retina, "default", mk_tx()).unwrap();
    s.attach_default_caps(lodpi, "default", mk_tx()).unwrap();

    // No viewports yet: no pixel truth.
    assert_eq!(s.resolve_terminal_cell_px(pid), None);

    // A viewport without pixel metrics contributes nothing.
    s.set_client_viewport(retina, ViewportInfo::new(120, 48));
    assert_eq!(s.resolve_terminal_cell_px(pid), None);

    // 120x48 cells over 1920x1440 px -> 16x30 px cells.
    s.set_client_viewport(
        retina,
        ViewportInfo::new(120, 48).with_pixels(Some(1920), Some(1440)),
    );
    assert_eq!(s.resolve_terminal_cell_px(pid), Some((16, 30)));

    // A later report from another display wins on recency...
    s.set_client_viewport(
        lodpi,
        ViewportInfo::new(80, 24).with_pixels(Some(640), Some(384)),
    );
    assert_eq!(s.resolve_terminal_cell_px(pid), Some((8, 16)));

    // A later report without usable pixels keeps the best known size.
    s.set_client_viewport(
        lodpi,
        ViewportInfo::new(80, 24).with_pixels(Some(79), Some(23)),
    );
    assert_eq!(s.resolve_terminal_cell_px(pid), Some((16, 30)));
    s.set_client_viewport(lodpi, ViewportInfo::new(80, 24));
    assert_eq!(s.resolve_terminal_cell_px(pid), Some((16, 30)));

    // Detach drops the donor's report with it.
    s.detach(retina);
    assert_eq!(s.resolve_terminal_cell_px(pid), None);
}

#[test]
fn detach_removes_client_and_drops_empty_subscriber_lists() {
    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let cid = s.new_client_id();
    s.attach_default_caps(cid, "default", mk_tx()).unwrap();
    assert!(!s.subscribers_for_terminal(pid).is_empty());
    s.detach(cid);
    assert!(!s.attached().contains_key(&cid));
    assert!(s.subscribers_for_terminal(pid).is_empty());
    assert!(
        s.resources.subscriber_map_is_empty(),
        "empty lists should be GC'd"
    );
}

#[test]
fn detach_is_idempotent() {
    let mut s = ServerState::new();
    let cid = ClientId(99);
    // Not attached at all — must not panic.
    s.detach(cid);
    s.detach(cid);
}
