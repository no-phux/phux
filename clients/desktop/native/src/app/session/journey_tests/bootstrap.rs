//! A failed first launch remains repairable in the same app process.
use super::*;

fn launch_unconnected(
    server: &Server,
    desktop: &Entity<Desktop>,
    phux: PathBuf,
    session: &str,
    cx: &mut TestAppContext,
) -> Entity<Desk> {
    desktop.update(cx, |desktop, cx| {
        desktop
            .launch(
                AppOptions {
                    socket: PathBuf::new(),
                    session: session.to_owned(),
                    phux,
                    layout_path: server.root.join("layout.json"),
                    fresh: false,
                    initial_terminal: None,
                    quick: false,
                    dev_telemetry: false,
                    startup_error: Some("The CLI is still being installed.".into()),
                },
                cx,
            )
            .unwrap();
    });
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    desk.read_with(cx, |desk, _| {
        assert_eq!(
            desk.connect_error.as_deref(),
            Some("The CLI is still being installed.")
        );
        assert!(desk.socket.as_os_str().is_empty());
    });
    desk
}

fn assert_recovered(
    server: &Server,
    desktop: &Entity<Desktop>,
    old: &Entity<Desk>,
    expected: &str,
    cx: &mut TestAppContext,
) {
    wait_for(cx, |cx| {
        let candidate = desktop.read_with(cx, |desktop, _| {
            desktop.windows.first().map(|window| window.desk.clone())
        });
        candidate.is_some_and(|desk| desk.entity_id() != old.entity_id() && attached(&desk, cx))
    });
    let recovered = desktop.read_with(cx, |desktop, cx| {
        assert_eq!(desktop.socket, server.socket);
        assert_eq!(desktop.updates.read(cx).local_socket(), server.socket);
        desktop.windows[0].desk.clone()
    });
    recovered.read_with(cx, |desk, _| {
        assert_eq!(desk.socket, server.socket);
        assert!(!desk.session.is_empty());
        if !expected.is_empty() {
            assert_eq!(desk.session, expected);
        }
        assert!(desk.connect_error.is_none());
    });
    recovered.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn retry_after_cli_install_recovers_the_socket_home_and_updater_without_relaunch(
    cx: &mut TestAppContext,
) {
    retry_after_install("", cx);
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn retry_after_cli_install_keeps_an_explicit_session_and_updates_new_windows(
    cx: &mut TestAppContext,
) {
    retry_after_install("journey-B", cx);
}

fn retry_after_install(session: &str, cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    let server = Server::new();
    let desktop = desktop(cx);
    let cli = server.root.join("installed-later");
    let old = launch_unconnected(&server, &desktop, cli.clone(), session, cx);
    // The fixed path appears only after the app has opened on its startup error.
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nexec '{}' --socket '{}' \"$@\"\n",
            server.binary.display(),
            server.socket.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    old.update(cx, |desk, cx| desk.reconnect(cx));
    assert_recovered(&server, &desktop, &old, session, cx);
    let inventory: serde_json::Value =
        serde_json::from_slice(&server.output(&["ls", "--json"], b"")).unwrap();
    assert!(
        inventory["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|session| session["name"] == "journey-A")
    );
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn a_failed_home_probe_keeps_named_first_launch_retryable(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    let server = Server::new();
    let desktop = desktop(cx);
    let cli = server.root.join("recovering-cli");
    let target = server.root.join("reported-socket");
    std::fs::write(
        &target,
        server.root.join("not-ready.sock").display().to_string(),
    )
    .unwrap();
    std::fs::write(&cli, format!(
        "#!/bin/sh\nif [ \"$*\" = 'status --json' ]; then printf '{{\"socket\":\"%s\"}}\\n' \"$(cat '{}')\"; else exec '{}' --socket '{}' \"$@\"; fi\n",
        target.display(), server.binary.display(), server.socket.display()
    )).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let old = launch_unconnected(&server, &desktop, cli, "journey-B", cx);
    old.update(cx, |desk, cx| desk.reconnect(cx));
    old.read_with(cx, |desk, _| {
        assert!(
            desk.socket.as_os_str().is_empty(),
            "A refused home probe must not adopt a partial target"
        )
    });
    desktop.read_with(cx, |desktop, _| {
        assert!(desktop.socket.as_os_str().is_empty())
    });
    std::fs::write(target, server.socket.display().to_string()).unwrap();
    old.update(cx, |desk, cx| desk.reconnect(cx));
    assert_recovered(&server, &desktop, &old, "journey-B", cx);
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn a_failed_update_home_probe_keeps_its_next_target_retryable(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let old = launch_unconnected(&server, &desktop, server.binary.clone(), "journey-B", cx);
    let updates = desktop.read_with(cx, |desktop, _| desktop.updates.clone());
    updates.update(cx, |updater, cx| {
        updater.retarget(server.binary.clone(), server.root.join("not-ready.sock"));
        updater.runtime_generation += 1;
        cx.notify();
    });
    wait_for(cx, |cx| {
        old.read_with(cx, |desk, _| {
            desk.connect_error.as_deref() != Some("The CLI is still being installed.")
        })
    });
    old.read_with(cx, |desk, _| assert!(desk.socket.as_os_str().is_empty()));
    updates.update(cx, |updater, cx| {
        updater.retarget(server.binary.clone(), server.socket.clone());
        updater.runtime_generation += 1;
        cx.notify();
    });
    assert_recovered(&server, &desktop, &old, "journey-B", cx);
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn updating_before_first_connection_opens_the_resolved_server_home(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let session = "journey-B";
    let old = launch_unconnected(&server, &desktop, server.binary.clone(), session, cx);
    let updates = desktop.read_with(cx, |desktop, _| desktop.updates.clone());
    updates.update(cx, |updater, cx| {
        updater.retarget(server.binary.clone(), server.socket.clone());
        updater.runtime_generation += 1;
        cx.notify();
    });
    assert_recovered(&server, &desktop, &old, session, cx);
}
