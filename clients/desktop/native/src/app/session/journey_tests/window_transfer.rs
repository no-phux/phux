//! Pane transfer between connected and still-connecting windows.

use super::*;

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn native_documents_are_discovered_and_transferred_without_replacing_the_resource(
    cx: &mut TestAppContext,
) {
    use crate::connection::documents::Content;
    let server = Server::new();
    let desktop = desktop(cx);
    let handle = server.launch(&desktop, cx);
    let source = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&source, cx) && placement_count(&source, cx) == 1
    });
    let producer = source.read_with(cx, |desk, _| desk.focused_terminal().unwrap());
    let opened: serde_json::Value = serde_json::from_slice(&server.output(
        &[
            "agent",
            "surface",
            "open",
            &producer.to_string(),
            "--window",
            "Native journey",
            "--json",
        ],
        b"",
    ))
    .unwrap();
    let selector = opened["resource"].as_str().unwrap();
    let document =
        phux_protocol::ResourceId::local(selector.strip_prefix('@').unwrap().parse().unwrap());
    wait_for(cx, |cx| {
        source.read_with(cx, |desk, _| desk.placement_of(&document).is_some())
    });
    source.read_with(cx, |desk, _| {
        let placement = desk.placement_of(&document).unwrap();
        assert!(
            placement.view_id.is_none(),
            "empty document never creates a terminal view"
        );
        assert_eq!(
            desk.pane_identity(placement).kind,
            crate::app::pane_identity::PaneKind::Surface
        );
    });
    let png = crate::workspace::view_tests::documents::png();
    server.output(&["agent", "surface", "publish", selector], &png);
    wait_for(cx, |cx| {
        source.read_with(cx, |desk, _| {
            matches!(
                desk.link
                    .as_ref()
                    .unwrap()
                    .connection
                    .document_content(&document),
                Some(Content::Image(_))
            )
        })
    });
    desktop.update(cx, |desktop, cx| {
        desktop.open_session("journey-A".into(), None, cx)
    });
    let target = desktop.read_with(cx, |desktop, _| desktop.windows[1].desk.clone());
    wait_for(cx, |cx| {
        attached(&target, cx) && placement_count(&target, cx) == 1
    });
    cx.update_window(*handle, |_, window, cx| {
        source.update(cx, |desk, cx| {
            desk.reveal_terminal(&document);
            desk.execute("window-next", window, cx);
        })
    })
    .unwrap();
    wait_for(cx, |cx| {
        source.read_with(cx, |desk, _| desk.placement_of(&document).is_none())
    });
    wait_for(cx, |cx| {
        target.read_with(cx, |desk, _| {
            matches!(
                desk.link
                    .as_ref()
                    .unwrap()
                    .connection
                    .document_content(&document),
                Some(Content::Image(_))
            )
        })
    });
    target.read_with(cx, |desk, _| {
        assert!(desk.placement_of(&document).unwrap().view_id.is_none());
        assert!(
            desk.link
                .as_ref()
                .unwrap()
                .connection
                .pane(&producer)
                .is_some()
        );
    });
    let target_handle = desktop.read_with(cx, |desktop, _| desktop.windows[1].handle);
    cx.update_window(target_handle, |_, window, cx| {
        target.update(cx, |desk, cx| desk.execute("window", window, cx))
    })
    .unwrap();
    let moved = desktop.read_with(cx, |desktop, _| desktop.windows[2].desk.clone());
    wait_for(cx, |cx| {
        target.read_with(cx, |desk, _| desk.placement_of(&document).is_none())
    });
    wait_for(cx, |cx| {
        moved.read_with(cx, |desk, _| {
            matches!(
                desk.link
                    .as_ref()
                    .unwrap()
                    .connection
                    .document_content(&document),
                Some(Content::Image(_))
            )
        })
    });
    let path = server.root.join("frame.png");
    server.run(&[
        "agent",
        "surface",
        "snapshot",
        selector,
        "--out",
        path.to_str().unwrap(),
    ]);
    assert_eq!(
        std::fs::read(path).unwrap(),
        png,
        "same live resource, same publication"
    );
    source.update(cx, |desk, cx| desk.prepare_close(cx));
    target.update(cx, |desk, cx| desk.prepare_close(cx));
    moved.update(cx, |desk, cx| desk.prepare_close(cx));
}

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
    let kept = a.read_with(cx, |desk, _| {
        desk.link
            .as_ref()
            .unwrap()
            .workspace
            .tabs
            .iter()
            .flat_map(|tab| crate::workspace::layout::placements(&tab.root))
            .filter_map(|placement| placement.terminal_id.clone())
            .filter(|terminal| terminal != &moved)
            .collect::<Vec<_>>()
    });
    let before: serde_json::Value =
        serde_json::from_slice(&server.output(&["ls", "--json"], b"")).unwrap();
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
    assert_eq!(placement_count(&b, cx), 2);
    a.read_with(cx, |desk, _| {
        let link = desk.link.as_ref().unwrap();
        assert_eq!(link.workspace.views_of(&moved), 0);
        // The main window may also adopt the fresh window's terminal. Check
        // the actual existing views, not a fixed presentation count.
        for terminal in kept {
            assert!(link.workspace.views_of(&terminal) > 0);
        }
        // Releasing the source view never ends the process.
        assert!(link.connection.pane(&moved).is_some());
    });
    let after: serde_json::Value =
        serde_json::from_slice(&server.output(&["ls", "--json"], b"")).unwrap();
    assert_eq!(
        after["terminals"].as_array().unwrap(),
        before["terminals"].as_array().unwrap(),
        "transfer must not replace or spawn terminals"
    );
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
    assert_eq!(placement_count(&a, cx), 2);
    wait_for(cx, |cx| attached(&b, cx) && placement_count(&b, cx) == 1);
    a.read_with(cx, |desk, _| {
        assert_eq!(desk.link.as_ref().unwrap().workspace.views_of(&moved), 1)
    });
    b.read_with(cx, |desk, _| {
        let link = desk.link.as_ref().unwrap();
        assert_eq!(link.workspace.views_of(&moved), 0);
    });
    a.update(cx, |desk, cx| desk.prepare_close(cx));
    b.update(cx, |desk, cx| desk.prepare_close(cx));
}
