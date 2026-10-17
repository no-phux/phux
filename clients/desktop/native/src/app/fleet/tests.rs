use super::*;
use crate::app::desk::Link;
use crate::app::{AppOptions, Desktop};
use crate::connection::DeterministicDial;
use crate::workspace::{Workspace, WorkspaceOptions};
use gpui::{Entity, TestAppContext};
use phux_client_core::layout::wire::HandleMap;
use phux_client_runtime::control::{SessionDescriptor, Topology};
use phux_protocol::ResourceId;

fn key(byte: u8) -> SessionKey {
    SessionKey::new([byte; 16]).unwrap()
}
fn authority(byte: u8) -> AuthorityId {
    AuthorityId::new([byte; 16]).unwrap()
}
fn session(byte: u8, name: &str) -> SessionDescriptor {
    SessionDescriptor {
        id: u32::from(byte),
        name: name.into(),
        attached_client_count: 0,
        keep_empty: true,
        created_at_unix_secs: 0,
        key: Some(key(byte)),
        lifetime: None,
        revision: None,
        members: 0,
        arrangement: None,
        awaiting_organization: false,
    }
}
fn connection(owner: u8, sessions: Vec<SessionDescriptor>) -> Connection {
    let first = &sessions[0];
    let mut connection =
        Connection::connect_with_key("/isolated-fleet.sock".into(), first.name.clone(), first.key)
            .unwrap();
    connection.status = Status::Attached;
    connection.topology = Some(Topology {
        focused_session: first.id,
        focused_pane: ResourceId::local(0),
        sessions,
        panes: Vec::new(),
        agent_sessions: Vec::new(),
        authority: Some(authority(owner)),
        inbox: None,
        handles: HandleMap::default(),
    });
    connection
}
fn fixture(cx: &mut TestAppContext) -> (Entity<Desk>, Entity<Desktop>) {
    let (_, desk, desktop) = launch(cx);
    (desk, desktop)
}
fn launch(
    cx: &mut TestAppContext,
) -> (
    gpui::WindowHandle<gpui_kit::component::Root>,
    Entity<Desk>,
    Entity<Desktop>,
) {
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!(
        "phux-fleet-{}-{}",
        std::process::id(),
        cx.test_function_name().unwrap()
    ));
    let mut prefs = crate::settings::default_display();
    prefs.ghostty_keys = false;
    std::fs::create_dir_all(&path).unwrap();
    let layout = crate::workspace::persist::save_layout("", &[], &prefs, None);
    std::fs::write(
        path.join("layout.json"),
        serde_json::to_vec(&crate::workspace::persist::layout_value(&layout)).unwrap(),
    )
    .unwrap();
    let handle = desktop.update(cx, |desktop, cx| {
        desktop
            .launch(
                AppOptions {
                    socket: path.join("isolated.sock"),
                    session: String::new(),
                    phux: path.join("not-installed"),
                    layout_path: path.join("layout.json"),
                    fresh: true,
                    initial_terminal: None,
                    quick: false,
                    dev_telemetry: false,
                    startup_error: None,
                },
                cx,
            )
            .unwrap()
    });
    let desk = handle
        .update(cx, |root, _, _| {
            root.view().clone().downcast::<Desk>().unwrap()
        })
        .unwrap();
    (handle, desk, desktop)
}
fn install(desk: &mut Desk, connection: Connection) {
    desk.session = connection.session_name().to_owned();
    desk.link = Some(Link {
        connection,
        workspace: Workspace::new(
            desk.session.clone(),
            WorkspaceOptions {
                fresh: false,
                initial_terminal: None,
            },
        ),
    });
}
fn observe(fleet: &mut Fleet, host: Option<&str>, connection: Connection) {
    let host = host.map(str::to_owned);
    let last = work_on(&connection, host.clone());
    fleet.observers.insert(
        host,
        Observer {
            connection: Some(connection),
            last,
            error: None,
            _wake: None,
        },
    );
}

#[gpui::test]
fn catalog_keeps_colliding_names_and_ids_on_their_own_authorities(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, _| {
        install(desk, connection(1, vec![session(7, "main")]));
        observe(
            &mut desk.fleet,
            Some("mini"),
            connection(2, vec![session(7, "main")]),
        );
        let work = desk.work_catalog();
        assert_eq!(work.len(), 2);
        assert_eq!(work[0].id, work[1].id);
        assert_ne!(work[0].destination, work[1].destination);
        assert_eq!(
            work.iter()
                .filter(|work| desk.work_is_active(&work.destination))
                .count(),
            1
        );
    });
}

#[gpui::test]
fn work_navigation_changes_exact_session_without_opening_a_native_window(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, desktop) = fixture(cx);
    desk.update(cx, |desk, cx| {
        install(
            desk,
            connection(1, vec![session(7, "same"), session(8, "other")]),
        );
        let target = desk
            .work_catalog()
            .into_iter()
            .find(|work| work.id == 8)
            .unwrap()
            .destination;
        desk.navigate_work(target, cx);
        assert_eq!(desk.session, "other");
        assert_eq!(
            desk.link.as_ref().unwrap().connection.home_key(),
            Some(key(8))
        );
    });
    cx.run_until_parked();
    assert_eq!(desktop.read_with(cx, |_, cx| cx.windows().len()), 1);
}

#[gpui::test]
fn switching_authorities_preserves_the_other_hosts_catalog_and_clears_old_dialogs(
    cx: &mut TestAppContext,
) {
    let _dial = DeterministicDial::arm();
    let (desk, desktop) = fixture(cx);
    desk.update(cx, |desk, cx| {
        install(desk, connection(1, vec![session(7, "main")]));
        observe(
            &mut desk.fleet,
            None,
            connection(1, vec![session(7, "main")]),
        );
        observe(
            &mut desk.fleet,
            Some("mini"),
            connection(2, vec![session(8, "main")]),
        );
        desk.hosts.push(super::super::host::RemoteHost {
            name: "mini".into(),
            endpoint: "quic://fixture.invalid:1".into(),
        });
        desk.modal = super::super::desk::Modal::KillSession {
            name: "main".into(),
            session: 7,
        };
        let target = desk
            .work_catalog()
            .into_iter()
            .find(|work| work.destination.host.is_some())
            .unwrap()
            .destination;
        desk.navigate_work(target, cx);
        assert_eq!(desk.remote.as_deref(), Some("mini"));
        assert_eq!(
            desk.link.as_ref().unwrap().connection.home_key(),
            Some(key(8))
        );
        assert_eq!(desk.modal, super::super::desk::Modal::None);
        assert!(
            desk.work_catalog()
                .iter()
                .any(|work| work.destination.host.is_none())
        );
    });
    assert_eq!(desktop.read_with(cx, |_, cx| cx.windows().len()), 1);
}

#[gpui::test]
fn offline_or_changed_catalog_targets_do_not_replace_active_work(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, cx| {
        install(desk, connection(1, vec![session(7, "main")]));
        observe(
            &mut desk.fleet,
            Some("mini"),
            connection(2, vec![session(8, "main")]),
        );
        let observer = desk.fleet.observers.get_mut(&Some("mini".into())).unwrap();
        observer.last[0].online = false;
        let stale = observer.last[0].destination.clone();
        desk.navigate_work(stale, cx);
        assert!(desk.remote.is_none());
        let mut replaced = desk
            .work_catalog()
            .into_iter()
            .find(|work| work.destination.host.is_none())
            .unwrap()
            .destination;
        replaced.authority = Some(authority(99));
        assert!(!desk.work_is_active(&replaced));
        desk.navigate_work(replaced, cx);
        assert_eq!(
            desk.link.as_ref().unwrap().connection.home_key(),
            Some(key(7))
        );
    });
}

#[gpui::test]
fn a_pending_active_dial_keeps_its_last_successful_catalog(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, _| {
        observe(
            &mut desk.fleet,
            Some("mini"),
            connection(2, vec![session(8, "retained")]),
        );
        desk.remote = Some("mini".into());
        desk.link = None;
        assert_eq!(desk.work_catalog().len(), 1);
        assert_eq!(desk.work_catalog()[0].destination.name, "retained");
    });
}

#[gpui::test]
fn refresh_replaces_terminal_observers_without_dropping_their_inventory(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, cx| {
        desk.hosts.push(super::super::host::RemoteHost {
            name: "mini".into(),
            endpoint: "ws://fixture.invalid:1".into(),
        });
        observe(
            &mut desk.fleet,
            Some("mini"),
            connection(2, vec![session(8, "retained")]),
        );
        let old = desk
            .fleet
            .observers
            .get(&Some("mini".into()))
            .unwrap()
            .connection
            .as_ref()
            .unwrap()
            .client
            .clone();
        old.close();
        desk.retry_fleet(cx);
        let next = desk.fleet.observers.get(&Some("mini".into())).unwrap();
        assert!(!std::sync::Arc::ptr_eq(
            &old,
            &next.connection.as_ref().unwrap().client
        ));
        assert_eq!(next.last.len(), 1);
        assert!(!next.last[0].online);
    });
}

#[test]
fn inventory_constructor_has_no_attach_or_execution_side_effects() {
    let _dial = DeterministicDial::arm();
    let connection =
        Connection::browse(std::path::Path::new("/isolated-fleet.sock"), None).unwrap();
    assert!(connection.client.attached_session().is_none());
    assert!(connection.client.selected_session().is_none());
    assert!(connection.topology.is_none());
    assert!(connection.home_key().is_none());
}

#[gpui::test]
fn a_drain_that_changes_no_work_reports_nothing(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, _| {
        observe(
            &mut desk.fleet,
            Some("mini"),
            connection(2, vec![session(7, "main")]),
        );
        let host = Some("mini".to_owned());
        desk.fleet.drain(&host);
        let revision = desk.fleet.revision;
        assert!(!desk.fleet.drain(&host));
        assert_eq!(desk.fleet.revision, revision);
    });
}

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

fn unreachable(error: &str) -> Observer {
    Observer {
        connection: None,
        last: Vec::new(),
        error: Some(error.into()),
        _wake: None,
    }
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

fn draw(cx: &mut gpui::VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn shown(cx: &mut gpui::VisualTestContext, selector: &str) -> bool {
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .is_some()
}

fn click_on(cx: &mut gpui::VisualTestContext, selector: &str) {
    let bounds = cx
        .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .unwrap_or_else(|| panic!("no {selector} on screen"));
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    draw(cx);
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
