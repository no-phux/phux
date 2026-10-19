//! Federated work navigation and reconnect journeys.

use super::*;

#[gpui::test]
#[ignore = "requires checkout CLI and disposable XDG_CONFIG_HOME/XDG_STATE_HOME; run this journey alone"]
fn federated_work_round_trip_preserves_layouts_and_never_spawns_when_browsing(
    cx: &mut TestAppContext,
) {
    let config = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").expect("disposable config"));
    assert!(config.starts_with(std::env::temp_dir()) || config.starts_with("/tmp"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let local = Server::new();
    let remote = Server::with_listener(Some(port));
    std::fs::create_dir_all(config.join("phux")).unwrap();
    std::fs::write(
        config.join("phux/config.toml"),
        remote.paired_remote_entry("journey-remote"),
    )
    .unwrap();
    let desktop = desktop(cx);
    let handle = local.launch_named(&desktop, "journey-A", true, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && placement_count(&desk, cx) == 1
    });
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.execute("split-right", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&desk, cx) == 2);
    let expected = snapshot(&desk, cx);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.work_catalog().iter().any(|work| {
                work.destination.host.as_deref() == Some("journey-remote")
                    && work.destination.name == "journey-A"
                    && work.online
            })
        })
    });
    let target = desk.read_with(cx, |desk, _| {
        desk.work_catalog()
            .into_iter()
            .find(|work| {
                work.destination.host.as_deref() == Some("journey-remote")
                    && work.destination.name == "journey-A"
            })
            .unwrap()
            .destination
    });
    let remote_key = target.key;
    desk.update(cx, |desk, cx| desk.navigate_work(target, cx));
    wait_for(cx, |cx| {
        attached(&desk, cx) && home_key(&desk, cx) == remote_key
    });
    assert_eq!(
        placement_count(&desk, cx),
        0,
        "opening an empty remote session creates no terminal"
    );
    desk.read_with(cx, |desk, _| assert!(desk.work_catalog().iter().any(|work| work.destination.host.is_none() && work.destination.name == "journey-A")));
    // An explicit new terminal, unlike passive inventory or session selection,
    // starts work. Keep it for the inactive-pane reveal round trip below.
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.execute("new", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&desk, cx) == 1);
    let remote_terminal = desk.read_with(cx, |desk, _| desk.focused_terminal().unwrap());
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.execute("new", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&desk, cx) == 2);
    desk.read_with(cx, |desk, _| {
        assert_ne!(
            desk.focused_terminal().as_ref(),
            Some(&remote_terminal),
            "saved focus differs from the pane we will click"
        )
    });
    desk.update(cx, |desk, cx| {
        desk.update_prefs(
            |prefs| {
                prefs.set_sidebar_host_visible(None, false);
                prefs.sidebar_grouping = crate::settings::SidebarGrouping::Machine;
                prefs.sidebar_width = 320.;
                prefs.tab_position = crate::settings::TabPosition::Side;
            },
            cx,
        )
    });
    desktop.read_with(cx, |desktop, _| {
        assert_eq!(desktop.windows.len(), 1);
        assert_eq!(desktop.windows[0].remote.as_deref(), Some("journey-remote"));
    });
    let back = desk.read_with(cx, |desk, _| {
        desk.work_catalog()
            .into_iter()
            .find(|work| work.destination.host.is_none() && work.destination.name == "journey-A")
            .unwrap()
            .destination
    });
    let local_key = back.key;
    desk.update(cx, |desk, cx| desk.navigate_work(back, cx));
    wait_for(cx, |cx| {
        attached(&desk, cx) && home_key(&desk, cx) == local_key && placement_count(&desk, cx) == 2
    });
    assert_eq!(
        snapshot(&desk, cx).tabs,
        expected.tabs,
        "A -> B -> A restores the exact split tree and placement identities"
    );
    desktop.read_with(cx, |desktop, _| {
        assert_eq!(desktop.windows.len(), 1);
        assert!(desktop.windows[0].remote.is_none());
    });
    // This window's current chrome/filter must win over the remote session's
    // deliberately different saved display, through the real attach/adopt path.
    desk.update(cx, |desk, cx| {
        desk.update_prefs(
            |prefs| {
                prefs.sidebar_hidden_hosts.clear();
                prefs.sidebar_grouping = crate::settings::SidebarGrouping::Project;
                prefs.sidebar_width = 248.;
                prefs.tab_position = crate::settings::TabPosition::Top;
            },
            cx,
        )
    });
    let expected_display = desk.read_with(cx, |desk, _| desk.prefs.clone());
    let target = desk.read_with(cx, |desk, _| {
        desk.work_catalog()
            .into_iter()
            .find(|work| {
                work.destination.host.as_deref() == Some("journey-remote")
                    && work.destination.key == remote_key
            })
            .unwrap()
            .destination
    });
    desk.update(cx, |desk, cx| {
        desk.open_work_terminal(target, remote_terminal.clone(), cx)
    });
    wait_for(cx, |cx| {
        attached(&desk, cx)
            && home_key(&desk, cx) == remote_key
            && desk.read_with(cx, |desk, _| {
                desk.focused_terminal().as_ref() == Some(&remote_terminal)
            })
    });
    desk.read_with(cx, |desk, _| {
        assert_eq!(
            desk.prefs, expected_display,
            "destination layout restores work, not old sidebar preferences"
        )
    });
    let back = desk.read_with(cx, |desk, _| {
        desk.work_catalog()
            .into_iter()
            .find(|work| work.destination.host.is_none() && work.destination.key == local_key)
            .unwrap()
            .destination
    });
    desk.update(cx, |desk, cx| desk.navigate_work(back, cx));
    wait_for(cx, |cx| {
        attached(&desk, cx) && home_key(&desk, cx) == local_key && placement_count(&desk, cx) == 2
    });
    assert_eq!(snapshot(&desk, cx).tabs, expected.tabs);
    desk.read_with(cx, |desk, _| assert_eq!(desk.prefs, expected_display));
    // A view of a sibling session must also recover, but only after the home
    // authority/key has been freshly validated on the new socket.
    let other = desk.read_with(cx, |desk, _| {
        desk.link
            .as_ref()
            .unwrap()
            .connection
            .topology
            .as_ref()
            .unwrap()
            .sessions
            .iter()
            .find(|session| session.name == "journey-B")
            .unwrap()
            .id
    });
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.new_terminal_in(other, window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&desk, cx) == 3);
    let before_reconnect = snapshot(&desk, cx);
    let epoch = desk.read_with(cx, |desk, _| {
        desk.link.as_ref().unwrap().connection.connection_epoch()
    });
    desk.update(cx, |desk, _| {
        desk.link.as_ref().unwrap().connection.reconnect().unwrap()
    });
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            let connection = &desk.link.as_ref().unwrap().connection;
            connection.connection_epoch() > epoch
                && connection.status == Status::Attached
                && connection.session_validated()
                && desk
                    .link
                    .as_ref()
                    .unwrap()
                    .workspace
                    .tabs
                    .iter()
                    .flat_map(|tab| crate::workspace::layout::placements(&tab.root))
                    .all(|placement| {
                        placement
                            .terminal_id
                            .as_ref()
                            .is_none_or(|id| connection.ready(id))
                    })
        })
    });
    assert_eq!(
        snapshot(&desk, cx).tabs,
        before_reconnect.tabs,
        "foreign views survive reconnect with their layout unchanged"
    );
    let _remote = remote;
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
    std::fs::remove_file(config.join("phux/config.toml")).unwrap();
}
