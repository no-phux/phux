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
        window_count: 0,
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
    (desk, desktop)
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
