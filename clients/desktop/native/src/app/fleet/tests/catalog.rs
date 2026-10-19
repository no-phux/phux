use super::*;

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
            target: None,
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
