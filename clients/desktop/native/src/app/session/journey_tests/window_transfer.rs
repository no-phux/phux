//! Pane transfer between connected and still-connecting windows.

use super::*;

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn moving_a_pane_to_the_next_window_keeps_its_terminal_running(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let handle = server.launch(&desktop, cx);
    let a = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| attached(&a, cx) && placement_count(&a, cx) == 1);
    cx.update_window(*handle, |_, window, cx| {
        a.update(cx, |desk, cx| desk.execute("split-right", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&a, cx) == 2);
    desktop.update(cx, |desktop, cx| {
        desktop.open_session("journey-A".to_owned(), None, cx)
    });
    let b = desktop.read_with(cx, |desktop, _| desktop.windows[1].desk.clone());
    wait_for(cx, |cx| attached(&b, cx) && placement_count(&b, cx) == 1);
    let moved = a.read_with(cx, |desk, _| {
        desk.focused_placement()
            .and_then(|placement| placement.terminal_id.clone())
            .unwrap()
    });
    cx.update_window(*handle, |_, window, cx| {
        a.update(cx, |desk, cx| desk.execute("window-next", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| {
        b.read_with(cx, |desk, _| {
            let link = desk.link.as_ref().unwrap();
            link.workspace.focused().is_some_and(|placement| {
                placement.terminal_id.as_ref() == Some(&moved) && placement.view_id.is_some()
            })
        })
    });
    assert_eq!(placement_count(&a, cx), 1);
    assert_eq!(placement_count(&b, cx), 2);
    a.read_with(cx, |desk, _| {
        let link = desk.link.as_ref().unwrap();
        assert_eq!(link.workspace.views_of(&moved), 0);
        // Releasing the source view never ends the process.
        assert!(link.connection.pane(&moved).is_some());
    });
    a.update(cx, |desk, cx| desk.prepare_close(cx));
    b.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn moving_a_pane_to_a_window_still_connecting_is_refused_and_keeps_the_pane(
    cx: &mut TestAppContext,
) {
    let server = Server::new();
    let desktop = desktop(cx);
    let handle = server.launch(&desktop, cx);
    let a = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| attached(&a, cx) && placement_count(&a, cx) == 1);
    cx.update_window(*handle, |_, window, cx| {
        a.update(cx, |desk, cx| desk.execute("split-right", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&a, cx) == 2);
    let moved = a.read_with(cx, |desk, _| {
        desk.focused_placement()
            .and_then(|placement| placement.terminal_id.clone())
            .unwrap()
    });
    desktop.update(cx, |desktop, cx| {
        desktop.open_session("journey-A".to_owned(), None, cx)
    });
    let b = desktop.read_with(cx, |desktop, _| desktop.windows[1].desk.clone());
    // The second window has not been parked, so it has not attached yet.
    assert!(!attached(&b, cx));
    cx.update_window(*handle, |_, window, cx| {
        a.update(cx, |desk, cx| desk.execute("window-next", window, cx));
    })
    .unwrap();
    a.read_with(cx, |desk, _| {
        // A refusal shows as the transient toast, not in the feed list.
        assert!(
            desk.feed
                .peek()
                .is_some_and(|entry| entry.title == "No other connected window")
        );
        let link = desk.link.as_ref().unwrap();
        // Nothing was released: the pane keeps its placement and its view.
        assert_eq!(link.workspace.views_of(&moved), 1);
        assert!(link.workspace.focused().is_some_and(|placement| {
            placement.terminal_id.as_ref() == Some(&moved) && placement.view_id.is_some()
        }));
    });
    wait_for(cx, |cx| attached(&b, cx) && placement_count(&b, cx) == 1);
    assert_eq!(placement_count(&a, cx), 2);
    b.read_with(cx, |desk, _| {
        let link = desk.link.as_ref().unwrap();
        assert_eq!(link.workspace.views_of(&moved), 0);
    });
    a.update(cx, |desk, cx| desk.prepare_close(cx));
    b.update(cx, |desk, cx| desk.prepare_close(cx));
}
