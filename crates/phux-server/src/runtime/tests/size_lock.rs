//! A view's size lock (`phux.view.size-lock/v1`) against other viewers.

use super::*;
use crate::runtime::apps::terminal::commands::terminal_input::handle_size_lock;

fn drain(receiver: &mut StubRx) -> Option<(u16, u16)> {
    let mut last = None;
    while let Ok(request) = receiver.resize.try_recv() {
        last = Some((request.cols, request.rows));
    }
    last
}

/// A desktop sizing its panes explicitly locks them: the phone's viewport,
/// use and explicit resizes leave them alone; unlocking restores `active`.
#[test]
fn a_locked_view_keeps_its_explicit_sizes_until_it_unlocks() {
    use phux_config::WindowSize;
    let state = SharedState::new();
    let (_, pane) = state.with_mut(|s| s.seed_session("home"));
    let mut receiver = register_stub(&state, pane);
    state.with_mut(|s| s.config_mut().window_size = WindowSize::Active);
    let desk = attach_client(&state, "home");
    let phone = attach_client(&state, "home");
    let wire = state.with_mut(|s| s.idspace.intern_resource(pane));
    let dims = || state.with(|s| s.registry().terminal(pane).unwrap().dims);

    handle_terminal_resize(&state, desk, &wire, (150, 40), None);
    handle_viewport_resize(&state, phone, &ViewportInfo::new(44, 30));
    assert_eq!(dims(), (44, 30), "unlocked, the phone takes the pane");
    drain(&mut receiver);

    handle_size_lock(&state, desk, b"true");
    assert_eq!(
        drain(&mut receiver),
        Some((150, 40)),
        "the claim puts it back"
    );
    assert_eq!(state.with(|s| s.terminal_size_owner(pane)), Some(desk));

    handle_viewport_resize(&state, phone, &ViewportInfo::new(40, 28));
    handle_client_activity(&state, phone);
    handle_terminal_resize(&state, phone, &wire, (40, 28), None);
    assert_eq!(drain(&mut receiver), None, "the phone resizes nothing");
    assert_eq!(dims(), (150, 40));
    handle_terminal_resize(&state, desk, &wire, (160, 44), None);
    assert_eq!(
        drain(&mut receiver),
        Some((160, 44)),
        "the holder still can"
    );

    handle_size_lock(&state, desk, b"bogus");
    handle_client_activity(&state, phone);
    assert_eq!(
        drain(&mut receiver),
        None,
        "a malformed write changes nothing"
    );

    handle_size_lock(&state, desk, b"false");
    handle_client_activity(&state, phone);
    assert_eq!(
        drain(&mut receiver),
        Some((40, 28)),
        "unlocked: active again"
    );
}

/// A viewport-voting view's lock holds its viewport over `smallest`, and
/// the lock ends with the attachment.
#[test]
fn a_locked_viewport_wins_until_its_view_leaves() {
    let state = SharedState::new();
    let (_, pane) = state.with_mut(|s| s.seed_session("home"));
    let mut receiver = register_stub(&state, pane);
    let desk = attach_client(&state, "home");
    let phone = attach_client(&state, "home");
    handle_viewport_resize(&state, desk, &ViewportInfo::new(160, 48));
    handle_viewport_resize(&state, phone, &ViewportInfo::new(44, 30));
    assert_eq!(drain(&mut receiver), Some((44, 30)), "smallest");

    handle_size_lock(&state, desk, b"true");
    assert_eq!(drain(&mut receiver), Some((160, 48)));
    handle_viewport_resize(&state, phone, &ViewportInfo::new(40, 28));
    assert_ne!(
        drain(&mut receiver),
        Some((40, 28)),
        "the phone's vote loses"
    );

    state.with_mut(|s| s.detach(desk));
    assert_eq!(drain(&mut receiver), Some((40, 28)), "the policy resumes");
}
