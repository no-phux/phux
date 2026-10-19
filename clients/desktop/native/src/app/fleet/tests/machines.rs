use super::*;

#[gpui::test]
fn machine_rows_report_reach_and_open_the_machines_own_work(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, cx| {
        install(desk, connection(1, vec![session(7, "main")]));
        for name in ["mini", "empty", "gone", "pending"] {
            desk.hosts.push(super::super::host::RemoteHost {
                name: name.into(),
                endpoint: format!("quic://{name}.invalid:1"),
            });
        }
        observe(
            &mut desk.fleet,
            Some("mini"),
            connection(2, vec![session(8, "other"), session(9, "more")]),
        );
        let mut empty = connection(3, vec![session(10, "placeholder")]);
        empty.topology.as_mut().unwrap().sessions.clear();
        observe(&mut desk.fleet, Some("empty"), empty);
        desk.fleet.observers.insert(
            Some("gone".into()),
            Observer {
                connection: None,
                last: Vec::new(),
                error: Some("connection refused".into()),
                _wake: None,
            },
        );

        assert_eq!(desk.machine_reach(None), Reach::Connected);
        assert_eq!(desk.machine_reach(Some("mini")), Reach::Connected);
        assert_eq!(desk.machine_reach(Some("empty")), Reach::Connected);
        assert_eq!(
            desk.machine_reach(Some("gone")),
            Reach::Unavailable("connection refused".into())
        );
        assert_eq!(desk.machine_reach(Some("pending")), Reach::Connecting);

        // An empty or still-connecting machine never replaces the active work.
        desk.open_machine(Some("empty".into()), cx);
        assert!(desk.remote.is_none());
        desk.open_machine(Some("pending".into()), cx);
        assert!(desk.remote.is_none());

        // With no intake, the machine's first session in catalog order opens.
        desk.open_machine(Some("mini".into()), cx);
        assert_eq!(desk.remote.as_deref(), Some("mini"));
        assert_eq!(
            desk.link.as_ref().unwrap().connection.home_key(),
            Some(key(9))
        );
    });
}

#[gpui::test]
fn this_windows_own_route_reports_its_link_and_reconnects_from_open(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, cx| {
        install(desk, connection(1, vec![session(7, "main")]));
        let link = &mut desk.link.as_mut().unwrap().connection;
        link.status = Status::Failed;
        link.error = Some("handshake refused".into());
        assert_eq!(
            desk.machine_reach(None),
            Reach::Unavailable("handshake refused".into())
        );
        desk.link.as_mut().unwrap().connection.error = None;
        assert_eq!(
            desk.machine_reach(None),
            Reach::Unavailable("Disconnected. Reconnect this window.".into())
        );

        // Open on this window's unavailable route reconnects it rather than
        // only redialing observers. With no server to start in this fixture,
        // the reconnect path itself reports the failure.
        desk.open_machine(None, cx);
        let titles: Vec<_> = desk.feed.items().map(|entry| entry.title.clone()).collect();
        assert!(titles.iter().any(|title| title == "Reconnecting Local"));
        assert!(titles.iter().any(|title| title == "Could not reconnect"));
        assert!(!titles.iter().any(|title| title.starts_with("Retrying")));

        desk.link = None;
        desk.connect_error = Some("socket missing".into());
        assert_eq!(
            desk.machine_reach(None),
            Reach::Unavailable("socket missing".into())
        );
        desk.connect_error = None;
        assert_eq!(desk.machine_reach(None), Reach::Connecting);
    });
}

#[gpui::test]
fn open_prefers_the_machines_intake_over_its_first_session(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, cx| {
        install(desk, connection(1, vec![session(7, "main")]));
        let mut mini = connection(2, vec![session(11, "alpha"), session(12, "zulu")]);
        mini.topology.as_mut().unwrap().inbox = Some(key(12));
        observe(&mut desk.fleet, Some("mini"), mini);
        desk.open_machine(Some("mini".into()), cx);
        assert_eq!(desk.remote.as_deref(), Some("mini"));
        assert_eq!(
            desk.link.as_ref().unwrap().connection.home_key(),
            Some(key(12))
        );
    });
}

#[gpui::test]
fn a_disconnected_machine_stays_down_until_retried(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, cx| {
        install(desk, connection(1, vec![session(7, "main")]));
        desk.hosts.push(super::super::host::RemoteHost {
            name: "mini".into(),
            endpoint: "quic://mini.invalid:1".into(),
        });
        observe(
            &mut desk.fleet,
            Some("mini"),
            connection(2, vec![session(8, "other")]),
        );
        observe(
            &mut desk.fleet,
            None,
            connection(1, vec![session(7, "main")]),
        );

        // This window's own route is never disconnected from a row.
        desk.disconnect_machine(None, cx);
        assert_eq!(desk.machine_reach(None), Reach::Connected);

        let mini = Some("mini".to_owned());
        desk.disconnect_machine(mini.clone(), cx);
        assert_eq!(
            desk.machine_reach(Some("mini")),
            Reach::Unavailable(DISCONNECTED.into())
        );
        let observer = &desk.fleet.observers[&mini];
        assert!(observer.connection.is_none());
        assert!(observer.last.iter().all(|work| !work.online));

        desk.sync_fleet(cx);
        assert!(desk.fleet.observers[&mini].connection.is_none());

        desk.retry_fleet(cx);
        let observer = &desk.fleet.observers[&mini];
        assert!(observer.connection.is_some());
        assert!(observer.error.is_none());
    });
}

#[gpui::test]
fn settings_rows_offer_each_machines_action_and_sidebar_rows_retry(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = launch(cx);
    let cx = &mut gpui::VisualTestContext::from_window(handle.into(), cx);
    desk.update(cx, |desk, _| {
        install(desk, connection(1, vec![session(7, "main")]));
        for name in ["mini", "gone", "pending"] {
            desk.hosts.push(super::super::host::RemoteHost {
                name: name.into(),
                endpoint: format!("quic://{name}.invalid:1"),
            });
        }
        observe(
            &mut desk.fleet,
            Some("mini"),
            connection(2, vec![session(8, "other")]),
        );
        desk.fleet
            .observers
            .insert(Some("gone".into()), unreachable("connection refused"));
    });
    draw(cx);

    // The unavailable machine is in the sidebar; a click redials it.
    click_on(cx, "unavailable-gone");
    desk.read_with(cx, |desk, _| {
        let observer = &desk.fleet.observers[&Some("gone".to_owned())];
        assert!(observer.connection.is_some());
        assert!(observer.error.is_none());
    });

    desk.update_in(cx, |desk, window, cx| {
        desk.fleet
            .observers
            .insert(Some("gone".into()), unreachable("connection refused"));
        desk.fleet.observers.remove(&Some("pending".to_owned()));
        desk.execute("machines", window, cx);
    });
    draw(cx);
    // This Mac is this window and connected: nothing to open or reconnect.
    for action in ["Open", "Retry", "Reconnect", "Disconnect"] {
        assert!(!shown(cx, &format!("settings-machine-{action}-local")));
    }
    assert!(shown(cx, "settings-machine-Open-mini"));
    assert!(shown(cx, "settings-machine-Disconnect-mini"));
    assert!(shown(cx, "settings-machine-Retry-gone"));
    assert!(!shown(cx, "settings-machine-Disconnect-gone"));
    for action in ["Open", "Retry", "Reconnect"] {
        assert!(!shown(cx, &format!("settings-machine-{action}-pending")));
    }
    assert!(shown(cx, "settings-machine-Disconnect-pending"));
    assert!(shown(cx, "settings-remove-host-pending"));

    // This window's route failing turns its row into Reconnect.
    desk.update(cx, |desk, cx| {
        desk.link.as_mut().unwrap().connection.status = Status::Failed;
        cx.notify();
    });
    draw(cx);
    assert!(shown(cx, "settings-machine-Reconnect-local"));

    click_on(cx, "settings-machine-Open-mini");
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.remote.as_deref(), Some("mini"));
        assert_eq!(desk.modal, super::super::desk::Modal::None);
    });
}

#[test]
fn machine_actions_follow_state_and_ownership() {
    use super::super::settings_panel::machine_action;
    let down = Reach::Unavailable("x".into());
    assert_eq!(machine_action(&down, true), Some("Reconnect"));
    assert_eq!(machine_action(&down, false), Some("Retry"));
    assert_eq!(machine_action(&Reach::Connected, false), Some("Open"));
    assert_eq!(machine_action(&Reach::Connected, true), None);
    assert_eq!(machine_action(&Reach::Connecting, false), None);
}
