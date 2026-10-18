//! Persistent session identity across rename, reconnect, and Quick windows.

use super::*;

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn remote_authority_cannot_rename_or_resolve_the_local_quick_home(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && home_key(&desk, cx).is_some()
    });
    let key = home_key(&desk, cx).unwrap();
    // Model a remote authority exposing the same persistent key (for example
    // a copied server state), without touching the user's remote registry.
    desk.update(cx, |desk, _| desk.remote = Some("remote-fixture".into()));
    desktop.update(cx, |desktop, _| {
        desktop.windows[0].remote = Some("remote-fixture".into())
    });
    server.run(&["rename", "journey-A", "remote-renamed-A"]);
    desk.update(cx, |desk, _| {
        let _ = desk.link.as_ref().unwrap().connection.refresh_topology();
    });
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| desk.session == "remote-renamed-A")
    });
    desktop.update(cx, |desktop, cx| {
        assert_eq!(desktop.home_key, Some(key));
        assert_eq!(
            desktop.home_session, "journey-A",
            "remote topology cannot rename local home"
        );
        assert!(
            desktop.resolve_quick_home(cx).is_err(),
            "only a local authority can resolve Quick's pinned key"
        );
    });
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn explicit_reconnect_after_offline_rename_never_creates_the_old_name(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && home_key(&desk, cx).is_some()
    });
    let key = home_key(&desk, cx).unwrap();
    let sessions_before = desk.read_with(cx, |desk, _| {
        desk.link
            .as_ref()
            .unwrap()
            .connection
            .topology
            .as_ref()
            .unwrap()
            .sessions
            .len()
    });
    desk.update(cx, |desk, _| desk.link.as_mut().unwrap().connection.close());
    wait_for(cx, |cx| !attached(&desk, cx));
    server.run(&["rename", "journey-A", "renamed-A"]);
    desk.update(cx, |desk, cx| desk.reconnect(cx));
    wait_for(cx, |cx| {
        attached(&desk, cx) && desk.read_with(cx, |desk, _| desk.session == "renamed-A")
    });
    assert_eq!(home_key(&desk, cx), Some(key));
    desktop.read_with(cx, |desktop, _| {
        assert_eq!(desktop.windows[0].session, "renamed-A");
    });
    desk.read_with(cx, |desk, _| {
        let topology = desk
            .link
            .as_ref()
            .unwrap()
            .connection
            .topology
            .as_ref()
            .unwrap();
        assert!(topology.session_named("journey-A").is_none());
        assert_eq!(
            topology.sessions.len(),
            sessions_before,
            "reconnect must not create another session"
        );
    });
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
    assert!(
        server.root.join("layout.json").exists(),
        "explicit layout path is retained"
    );
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn project_window_reuse_follows_rename_without_adopting_the_reused_old_name(
    cx: &mut TestAppContext,
) {
    let server = Server::new();
    let desktop = desktop(cx);
    server.launch(&desktop, cx);
    let original = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&original, cx) && home_key(&original, cx).is_some()
    });
    let key = home_key(&original, cx).unwrap();
    server.run(&["rename", "journey-A", "renamed-A"]);
    server.run(&["new", "--json", "--empty", "-s", "journey-A"]);
    original.update(cx, |desk, _| {
        let _ = desk.link.as_ref().unwrap().connection.refresh_topology();
    });
    wait_for(cx, |cx| {
        original.read_with(cx, |desk, _| desk.session == "renamed-A")
    });
    desktop.update(cx, |desktop, cx| {
        desktop
            .open_project("renamed-A".into(), phux_protocol::ResourceId::local(1), cx)
            .unwrap();
        assert_eq!(
            desktop.windows.len(),
            1,
            "the renamed project reuses its window"
        );
        assert_eq!(desktop.windows[0].session, "renamed-A");
        assert_eq!(desktop.windows[0].desk.entity_id(), original.entity_id());
        desktop
            .open_project("journey-A".into(), phux_protocol::ResourceId::local(1), cx)
            .unwrap();
        assert_eq!(
            desktop.windows.len(),
            2,
            "a reused old name needs its own window"
        );
        assert_ne!(desktop.windows[1].desk.entity_id(), original.entity_id());
    });
    let replacement = desktop.read_with(cx, |desktop, _| desktop.windows[1].desk.clone());
    wait_for(cx, |cx| {
        attached(&replacement, cx) && home_key(&replacement, cx).is_some()
    });
    assert_eq!(home_key(&original, cx), Some(key));
    assert_ne!(home_key(&replacement, cx), Some(key));
    original.update(cx, |desk, cx| desk.prepare_close(cx));
    replacement.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn remaining_window_keeps_quick_home_identity_after_original_window_closes(
    cx: &mut TestAppContext,
) {
    let server = Server::new();
    let desktop = desktop(cx);
    let home_window = server.launch(&desktop, cx);
    let a = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| attached(&a, cx) && home_key(&a, cx).is_some());
    let key = home_key(&a, cx).unwrap();
    desktop.update(cx, |desktop, cx| {
        desktop.open_session("journey-B".to_owned(), None, cx)
    });
    let b = desktop.read_with(cx, |desktop, _| desktop.windows[1].desk.clone());
    wait_for(cx, |cx| attached(&b, cx));
    cx.update_window(*home_window, |_, window, _| window.remove_window())
        .unwrap();
    wait_for(cx, |cx| {
        desktop.read_with(cx, |desktop, _| desktop.windows.len() == 1)
    });
    server.run(&["rename", "journey-A", "renamed-A"]);
    server.run(&["new", "--json", "--empty", "-s", "journey-A"]);
    wait_for(cx, |cx| {
        desktop.read_with(cx, |desktop, _| desktop.home_session == "renamed-A")
    });
    desktop.update(cx, |desktop, cx| desktop.toggle_quick(cx));
    let quick = desktop.read_with(cx, |desktop, _| {
        desktop.quick.as_ref().unwrap().desk.clone()
    });
    wait_for(cx, |cx| {
        quick.read_with(cx, |desk, _| {
            let connection = &desk.link.as_ref().unwrap().connection;
            assert!(
                !matches!(connection.status, Status::Failed | Status::Closed),
                "quick attach refused: {:?}",
                connection.error
            );
        });
        attached(&quick, cx) && home_key(&quick, cx).is_some()
    });
    assert_eq!(home_key(&quick, cx), Some(key));
    assert_ne!(home_key(&quick, cx), home_key(&b, cx));
    desktop.read_with(cx, |desktop, _| assert_eq!(desktop.home_key, Some(key)));
    quick.update(cx, |desk, cx| desk.prepare_close(cx));
    b.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn managed_redial_after_offline_rename_does_not_adopt_a_reused_name(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && home_key(&desk, cx).is_some()
    });
    let key = home_key(&desk, cx).unwrap();
    desk.update(cx, |desk, _| desk.link.as_mut().unwrap().connection.close());
    wait_for(cx, |cx| !attached(&desk, cx));
    server.run(&["rename", "journey-A", "renamed-A"]);
    server.run(&["new", "--json", "--empty", "-s", "journey-A"]);
    let updates = desktop.read_with(cx, |desktop, _| desktop.updates.clone());
    updates.update(cx, |updates, cx| {
        updates.runtime_generation += 1;
        cx.notify();
    });
    wait_for(cx, |cx| {
        attached(&desk, cx) && desk.read_with(cx, |desk, _| desk.session == "renamed-A")
    });
    assert_eq!(home_key(&desk, cx), Some(key));
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn rename_dialog_keeps_the_original_key_when_its_old_name_is_reused(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let window = server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && placement_count(&desk, cx) == 1
    });
    let key = home_key(&desk, cx).unwrap();
    cx.update_window(*window, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.execute("rename-session", window, cx));
    })
    .unwrap();
    desk.read_with(cx, |desk, _| {
        assert!(matches!(&desk.modal, crate::app::desk::Modal::Session { target: Some(target), .. } if target.key == Some(key)));
    });
    server.run(&["rename", "journey-A", "moved-A"]);
    server.run(&["new", "--json", "--empty", "-s", "journey-A"]);
    // An empty foreign session has no terminal event to refresh the live
    // handle roster. Read it explicitly without changing the captured modal.
    desk.update(cx, |desk, _| {
        let _ = desk.link.as_ref().unwrap().connection.refresh_topology();
    });
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.link
                .as_ref()
                .and_then(|link| link.connection.topology.as_ref())
                .is_some_and(|topology| {
                    topology
                        .session_keyed(&key)
                        .is_some_and(|session| session.name == "moved-A")
                        && topology.session_named("journey-A").is_some()
                })
        })
    });
    cx.update_window(*window, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            desk.apply_session("final-A".to_owned(), None, window, cx)
        });
    })
    .unwrap();
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            let refusal = desk.feed.items().find(|entry| {
                entry.title == "Could not rename the session" || entry.title == "Session changed"
            });
            assert!(refusal.is_none(), "rename refused: {refusal:?}");
            desk.session == "final-A"
                && desk.feed.items().any(|entry| {
                    entry.kind == crate::app::notifications::Kind::Info
                        && entry.title == "Session renamed"
                })
        })
    });
    desk.read_with(cx, |desk, _| {
        let topology = desk
            .link
            .as_ref()
            .unwrap()
            .connection
            .topology
            .as_ref()
            .unwrap();
        assert_eq!(topology.session_keyed(&key).unwrap().name, "final-A");
        assert_ne!(topology.session_named("journey-A").unwrap().key, Some(key));
        assert!(
            desk.feed
                .items()
                .any(|entry| entry.kind == crate::app::notifications::Kind::Info
                    && entry.title == "Session renamed")
        );
    });
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI and disposable XDG_STATE_HOME"]
fn deleted_home_with_reused_name_cannot_open_a_replacement_quick_window(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let a_window = server.launch(&desktop, cx);
    let a = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| attached(&a, cx) && home_key(&a, cx).is_some());
    let original = home_key(&a, cx).unwrap();
    desktop.update(cx, |desktop, cx| {
        desktop.open_session("journey-B".to_owned(), None, cx)
    });
    let b = desktop.read_with(cx, |desktop, _| desktop.windows[1].desk.clone());
    wait_for(cx, |cx| attached(&b, cx));
    cx.update_window(*a_window, |_, window, _| window.remove_window())
        .unwrap();
    wait_for(cx, |cx| {
        desktop.read_with(cx, |desktop, _| desktop.windows.len() == 1)
    });
    server.run(&["kill", "--yes", "journey-A"]);
    server.run(&["new", "--json", "--empty", "-s", "journey-A"]);
    wait_for(cx, |cx| {
        b.read_with(cx, |desk, _| {
            let replica = desk.link.as_ref().unwrap().connection.client.organization();
            replica.session(&original).is_none()
                && replica
                    .sessions()
                    .iter()
                    .any(|session| session.name == "journey-A")
        })
    });
    desktop.update(cx, |desktop, cx| desktop.toggle_quick(cx));
    desktop.read_with(cx, |desktop, _| {
        assert_eq!(desktop.home_key, Some(original));
        assert!(
            desktop.quick.is_none(),
            "a reused name never replaces the pinned home"
        );
    });
    b.read_with(cx, |desk, _| {
        assert!(
            desk.feed
                .items()
                .any(|entry| entry.title == "Could not open quick terminal")
        )
    });
    b.update(cx, |desk, cx| desk.prepare_close(cx));
}
