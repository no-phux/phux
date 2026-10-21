//! Kill confirmation through a real connection and disposable server.

use super::*;
use crate::app::desk::Modal;

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn kill_session_confirmation_deletes_an_inactive_session(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let handle = server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && home_key(&desk, cx).is_some()
    });
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            let session = desk
                .link
                .as_ref()
                .unwrap()
                .connection
                .topology
                .as_ref()
                .unwrap()
                .session_named("journey-B")
                .unwrap()
                .id;
            let target = desk.rename_session_target(session);
            desk.open_modal(
                Modal::KillSession {
                    name: "journey-B".into(),
                    session,
                    target,
                },
                window,
                cx,
            );
        });
    })
    .unwrap();
    press(cx, *handle, "enter");
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.link
                .as_ref()
                .unwrap()
                .connection
                .topology
                .as_ref()
                .unwrap()
                .session_named("journey-B")
                .is_none()
        })
    });
    assert_session_absent(&server, "journey-B");
    assert!(
        attached(&desk, cx),
        "killing an inactive session keeps this connection alive"
    );
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn kill_current_session_removes_its_terminals(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let handle = server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx) && placement_count(&desk, cx) == 1
    });
    let resources = desk.read_with(cx, |desk, _| {
        desk.link
            .as_ref()
            .unwrap()
            .connection
            .panes()
            .iter()
            .filter(|pane| pane.session_name == "journey-A")
            .map(|pane| pane.terminal_id.clone())
            .collect::<Vec<_>>()
    });
    assert!(!resources.is_empty());
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.execute("kill-session", window, cx));
    })
    .unwrap();
    desk.read_with(cx, |desk, _| {
        assert!(matches!(desk.modal, Modal::KillSession { .. }))
    });
    press(cx, *handle, "enter");
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            let link = desk.link.as_ref().unwrap();
            resources
                .iter()
                .all(|id| link.connection.pane(id).is_none())
        })
    });
    assert_session_absent(&server, "journey-A");
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}

fn assert_session_absent(server: &Server, name: &str) {
    let state: serde_json::Value =
        serde_json::from_slice(&server.output(&["ls", "--json"], b"")).unwrap();
    assert!(
        state["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|session| session["name"] != name),
        "server must delete the session: {state}"
    );
}
