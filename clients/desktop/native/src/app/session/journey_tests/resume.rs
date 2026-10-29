//! Installed-update handoff against live terminals on a private server.

use super::*;

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN and disposable XDG_STATE_HOME"]
fn update_reopens_the_current_session_and_fresh_window_without_spawning(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let handle = server.launch_named(&desktop, "journey-A", true, cx);
    let main = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&main, cx)
            && main.read_with(cx, |desk, _| {
                desk.work_catalog().iter().any(|work| {
                    work.destination.name == "journey-B" && work.destination.key.is_some()
                })
            })
    });
    // The app was launched on A, but the user is now chatting in B.
    main.update(cx, |desk, cx| {
        let destination = desk
            .work_catalog()
            .into_iter()
            .find(|work| work.destination.name == "journey-B")
            .unwrap()
            .destination;
        desk.navigate_work(destination, cx);
    });
    wait_for(cx, |cx| {
        attached(&main, cx)
            && home_key(&main, cx).is_some()
            && main.read_with(cx, |desk, _| {
                desk.link.as_ref().unwrap().workspace.restored()
            })
    });
    cx.update_window(*handle, |_, window, cx| {
        main.update(cx, |desk, cx| desk.execute("new", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&main, cx) == 1);
    cx.update_window(*handle, |_, window, cx| {
        main.update(cx, |desk, cx| desk.execute("new", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&main, cx) == 2);
    desktop.update(cx, |desktop, cx| {
        desktop.open_session("journey-B".into(), None, cx);
    });
    let fresh = desktop.read_with(cx, |desktop, _| desktop.windows[1].desk.clone());
    wait_for(cx, |cx| {
        attached(&fresh, cx) && placement_count(&fresh, cx) == 1
    });
    let handles = desktop.read_with(cx, |desktop, _| {
        desktop
            .windows
            .iter()
            .map(|opened| opened.handle)
            .collect::<Vec<_>>()
    });
    cx.update_window(handles[1], |_, window, cx| {
        window.activate_window();
        fresh.update(cx, |desk, cx| desk.execute("split-right", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&fresh, cx) == 2);
    cx.update_window(handles[1], |_, window, cx| {
        fresh.update(cx, |desk, cx| desk.execute("new", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&fresh, cx) == 3);
    let expected_fresh = snapshot(&fresh, cx);
    let expected_main = desktop.update(cx, |desktop, cx| {
        // The handoff must win over the ordinary store's older tab focus.
        let expected = main.update(cx, |desk, _| {
            let link = desk.link.as_mut().unwrap();
            link.workspace.active_id = link.workspace.tabs[0].id.clone();
            let expected =
                crate::workspace::snapshot_layout(&link.workspace, &link.connection, &desk.prefs)
                    .unwrap();
            link.connection.status = Status::Closed;
            expected
        });
        // Even a secondary window temporarily offline at the update boundary
        // must keep the presentation it was showing, rather than its store's old layout.
        fresh.update(cx, |desk, _| {
            desk.link.as_mut().unwrap().connection.status = Status::Closed;
        });
        desktop.save_restart(cx).unwrap();
        expected
    });
    // The ordinary window can also show terminals opened by the fresh one.
    // Count the captured presentation, not an assumed number of distinct PTYs.
    let expected_placements = placement_count(&main, cx) + placement_count(&fresh, cx);
    let before: serde_json::Value =
        serde_json::from_slice(&server.output(&["ls", "--json"], b"")).unwrap();
    for handle in handles {
        cx.update_window(handle, |_, window, _| window.remove_window())
            .unwrap();
    }
    cx.run_until_parked();

    let reopened = cx.update(Desktop::install);
    reopened.update(cx, |desktop, cx| {
        desktop
            .launch_with_restore(
                AppOptions {
                    socket: server.socket.clone(),
                    session: "journey-A".into(),
                    phux: server.binary.clone(),
                    layout_path: PathBuf::new(),
                    fresh: false,
                    initial_terminal: None,
                    quick: false,
                    dev_telemetry: false,
                    startup_error: None,
                    cli_install: None,
                },
                true,
                cx,
            )
            .unwrap();
    });
    let restored = reopened.read_with(cx, |desktop, _| {
        assert_eq!(desktop.windows.len(), 2);
        desktop
            .windows
            .iter()
            .map(|opened| opened.desk.clone())
            .collect::<Vec<_>>()
    });
    wait_for(cx, |cx| {
        restored.iter().all(|desk| attached(desk, cx))
            && restored
                .iter()
                .map(|desk| placement_count(desk, cx))
                .sum::<usize>()
                == expected_placements
    });
    let main = restored
        .iter()
        .find(|desk| desk.read_with(cx, |desk, _| desk.role == super::super::WindowRole::Main))
        .unwrap();
    let fresh = restored
        .iter()
        .find(|desk| desk.read_with(cx, |desk, _| desk.role == super::super::WindowRole::Fresh))
        .unwrap();
    assert_eq!(
        main.read_with(cx, |desk, _| desk.session.clone()),
        "journey-B"
    );
    assert_eq!(snapshot(main, cx).tabs, expected_main.tabs);
    assert_eq!(snapshot(main, cx).active_tab, expected_main.active_tab);
    let actual = snapshot(fresh, cx);
    assert_eq!(actual.tabs, expected_fresh.tabs);
    assert_eq!(actual.active_tab, expected_fresh.active_tab);
    let after: serde_json::Value =
        serde_json::from_slice(&server.output(&["ls", "--json"], b"")).unwrap();
    assert_eq!(
        after["terminals"].as_array().unwrap(),
        before["terminals"].as_array().unwrap(),
        "restart must not spawn terminals"
    );
    for desk in restored {
        desk.update(cx, |desk, cx| desk.prepare_close(cx));
    }
}
