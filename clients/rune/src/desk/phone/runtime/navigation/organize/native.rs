use super::*;

fn asked() -> (std::sync::Arc<Client>, Runtime<Desk>, u64) {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    keyed(&mut rt);
    rt.update(|d, _| d.native = Some(crate::mobile::Platform::test()));
    open(&mut rt);
    hold(&mut rt, "phone-session-2", 0.1);
    let request = crate::mobile::take_requests(&mut rt)["menu"]["request"]
        .as_u64()
        .expect("native request");
    (client, rt, request)
}

fn refused(rt: &mut Runtime<Desk>, request: u64) {
    assert!(!crate::mobile::menu_pick(rt, request, "fold"));
    assert!(!rt.state.phone.as_ref().unwrap().folded.contains(&2));
}

#[test]
fn native_menu_navigation_and_inactive_round_trips_revoke_the_old_answer() {
    let (_, mut rt, request) = asked();
    crate::mobile::menu_dismiss(&mut rt, request);
    rt.update(|d, _| d.close_phone_navigation());
    open(&mut rt);
    refused(&mut rt, request);

    let (_, mut rt, request) = asked();
    crate::mobile::set_active(&mut rt, false);
    crate::mobile::set_active(&mut rt, true);
    refused(&mut rt, request);
}

#[test]
fn native_menu_reconnect_and_replacement_revoke_the_old_answer() {
    let (client, mut rt, request) = asked();
    rt.update(|d, _| assert!(d.reopen_after_reconnect(&client)));
    refused(&mut rt, request);

    let (_, mut rt, request) = asked();
    rt.update(|d, _| d.replace_connection(crate::desk::link::Replacement::Retry));
    refused(&mut rt, request);
}

#[test]
fn native_menu_checks_live_transport_before_the_next_poll() {
    let (client, mut rt, request) = asked();
    client.with_control(|control| control.connection_lost(None));
    refused(&mut rt, request);
    assert!(crate::mobile::take_requests(&mut rt)["menu_request"].is_null());
}

#[test]
fn native_menu_rejects_changed_client_epoch_server_and_target_identity() {
    let (_, mut rt, request) = asked();
    rt.state.connection.epoch += 1;
    refused(&mut rt, request);

    let (_, mut rt, request) = asked();
    rt.state.link.server += 1;
    refused(&mut rt, request);

    let (_, mut rt, request) = asked();
    let (other, _) = fixture();
    rt.state.connection.client = Some(other);
    refused(&mut rt, request);

    let (_, mut rt, request) = asked();
    rt.state
        .connection
        .topology
        .as_mut()
        .unwrap()
        .sessions
        .iter_mut()
        .find(|s| s.id == 2)
        .unwrap()
        .key = Some(phux_protocol::ids::SessionKey::new([99; 16]).unwrap());
    refused(&mut rt, request);
}

#[test]
fn native_menu_rejects_unoffered_verbs_without_consuming_the_request() {
    let (_, mut rt, _) = asked();
    hold(&mut rt, "phone-session-pane-@3", 2.0);
    let request = crate::mobile::take_requests(&mut rt)["menu"]["request"]
        .as_u64()
        .unwrap();
    assert!(!crate::mobile::menu_pick(&mut rt, request, "fold"));
    assert_eq!(
        menu(&rt),
        Some(Held::Pane(ResourceId::local(3), "phux".into()))
    );
    assert!(crate::mobile::menu_pick(&mut rt, request, "open"));
}

#[test]
fn native_menu_can_dismiss_without_selection_and_a_new_hold_gets_new_identity() {
    let (_, mut rt, request) = asked();
    crate::mobile::menu_dismiss(&mut rt, request);
    crate::mobile::menu_dismiss(&mut rt, request);
    assert_eq!(menu(&rt), None);
    assert!(!rt.state.phone.as_ref().unwrap().folded.contains(&2));
    hold(&mut rt, "phone-session-2", 2.0);
    let next = crate::mobile::take_requests(&mut rt)["menu"]["request"]
        .as_u64()
        .unwrap();
    assert_ne!(request, next);
    refused(&mut rt, request);
    assert!(crate::mobile::menu_pick(&mut rt, next, "fold"));
}

#[test]
fn native_menu_preserves_a_satellite_panes_open_only_offer() {
    let (_, mut rt) = fixture();
    set_inventory(
        &mut rt,
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
            .with_resources(vec![
                ResourceInfo::new(
                    ResourceId::satellite("build-box", 7),
                    WindowId::new(0),
                    80,
                    24,
                )
                .with_title(Some("editor".into())),
            ]),
    );
    rt.update(|d, _| d.native = Some(crate::mobile::Platform::test()));
    open(&mut rt);
    hold(&mut rt, "phone-session-pane-build-box/@7", 0.1);
    let ask = crate::mobile::take_requests(&mut rt)["menu"].clone();
    let request = ask["request"].as_u64().expect("satellite request");
    let items = ask["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "open");
    assert!(!crate::mobile::menu_pick(&mut rt, request, "close"));
    assert!(crate::mobile::menu_pick(&mut rt, request, "open"));
}

#[test]
fn native_menu_close_after_visual_dismissal_still_names_the_original_pane() {
    let (_, mut rt, _) = asked();
    rt.update(|d, _| d.settings.confirm_close = true);
    hold(&mut rt, "phone-session-pane-@3", 2.0);
    let request = crate::mobile::take_requests(&mut rt)["menu"]["request"]
        .as_u64()
        .unwrap();
    crate::mobile::menu_dismiss(&mut rt, request);
    assert!(crate::mobile::menu_pick(&mut rt, request, "close"));
    assert_eq!(
        rt.state.pending,
        Some(crate::desk::Pending::Close(ResourceId::local(3)))
    );
    assert!(!crate::mobile::menu_pick(&mut rt, request, "close"));
    assert!(rt.state.phone_page().is_none());
}
