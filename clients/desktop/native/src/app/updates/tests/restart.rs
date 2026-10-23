//! Explicit restart, including the deferred window snapshot.

use super::*;

/// An empty desktop whose restart file lands under a disposable socket hash.
/// The returned handle must stay alive until the restart snapshot runs.
fn attach_restorable(
    cx: &mut gpui::TestAppContext,
    updater: &gpui::Entity<Updater>,
    socket: &str,
) -> gpui::Entity<super::super::super::session::Desktop> {
    let desktop = cx.update(super::super::super::session::Desktop::install);
    desktop.update(cx, |desktop, _| {
        desktop.set_socket_for_test(std::path::PathBuf::from(socket));
    });
    updater.update(cx, |updater, _| {
        updater.desktop = Some(desktop.downgrade());
    });
    desktop
}

fn remove_restart_state(socket: &str) {
    let paths = crate::workspace::persist::layout_paths(socket, "");
    if let Some(directory) = paths.layout.parent() {
        let _ = std::fs::remove_dir_all(directory);
    }
}

#[gpui::test]
fn a_staged_update_restarts_only_on_the_next_click(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext;
    if cfg!(target_os = "linux") {
        return;
    }
    let socket = format!("phux-update-click-{}", std::process::id());
    let updater = cx.new(|_| installed("0.2.0"));
    let _desktop = attach_restorable(cx, &updater, &socket);
    updater.update(cx, |updater, cx| {
        updater.prefs.manage_runtime = false;
        updater.record_stage(Ok(staged("0.3.0", &updates::own_protocol())));
        assert!(!updater.relaunch);
        updater.act(cx);
        assert!(
            !updater.relaunch,
            "the window snapshot waits until the click releases its desk"
        );
    });
    cx.run_until_parked();
    updater.read_with(cx, |updater, _| {
        assert!(
            updater.relaunch,
            "the next click restarts after windows are saved"
        );
        assert!(!updater.busy);
    });
    remove_restart_state(&socket);
}

/// Drive the actual async CLI completion callback using only a private shell
/// fixture. No installed phux, server socket, or application bundle is used.
fn runtime_callback(cx: &mut gpui::TestAppContext, desktop: bool) {
    use gpui::AppContext;
    use std::os::unix::fs::PermissionsExt;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "phux-update-callback-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let phux = directory.join("fake-phux");
    std::fs::write(&phux, "#!/bin/sh\n: > \"${0%/*}/invoked\"\nprintf '%s\\n' '{\"schema_version\":2,\"server_handoff\":{\"result\":\"upgrading\"}}'\n").unwrap();
    std::fs::set_permissions(&phux, std::fs::Permissions::from_mode(0o755)).unwrap();
    let updater = cx.new(|_| installed("0.2.0"));
    let socket = directory.display().to_string();
    let _desktop = desktop.then(|| attach_restorable(cx, &updater, &socket));
    updater.update(cx, |updater, cx| {
        updater.phux = phux;
        updater.socket = directory.join("never-connected.sock");
        updater.current = Some(release("0.2.0", "1.0"));
        let mut runtime = runtime(Some("1.0"));
        runtime.protocol_target = Some(if desktop { "2.0" } else { "1.0" }.into());
        updater.runtime = Some(runtime);
        if desktop {
            updater.target = Some(release("0.3.0", "2.0"));
            updater.bundle = Some(directory.join("Phux.app"));
            let mut staged = staged("0.3.0", "2.0");
            staged.app = directory.join("staged/Phux.app");
            updater.record_stage(Ok(staged));
            updater.record_check(
                Ok(release("0.3.0", "2.0")),
                Some(Err(anyhow::anyhow!("offline"))),
            );
            assert!(
                updater.phux_due(),
                "a failed recheck cannot erase the staged server prerequisite"
            );
            assert_eq!(
                updater
                    .runtime
                    .as_ref()
                    .unwrap()
                    .protocol_current
                    .as_deref(),
                Some("1.0")
            );
            assert_eq!(updater.action_label(), Some("Restart & update server"));
            assert!(
                !directory.join("invoked").exists(),
                "staging cannot hand off the old Desktop's server"
            );
            updater.apply_staged();
            assert!(
                !updater.handed_off,
                "plain quit cannot install Desktop before its server moves"
            );
        }
        assert!(!updater.relaunch);
        updater.act(cx);
        assert!(updater.busy);
    });
    cx.run_until_parked();
    updater.read_with(cx, |updater, _| {
        assert!(!updater.busy);
        assert_eq!(updater.runtime_generation, 1);
        assert_eq!(
            updater.relaunch, desktop,
            "only an explicitly requested combined restart can quit Desktop"
        );
        assert_eq!(
            updater
                .runtime
                .as_ref()
                .unwrap()
                .protocol_current
                .as_deref(),
            Some(if desktop { "2.0" } else { "1.0" })
        );
    });
    assert!(directory.join("invoked").exists());
    if desktop {
        remove_restart_state(&socket);
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(not(target_os = "linux"))]
#[gpui::test]
fn a_protocol_major_change_hands_off_and_restarts_only_after_consent(
    cx: &mut gpui::TestAppContext,
) {
    runtime_callback(cx, true);
}

#[gpui::test]
fn the_server_only_async_completion_never_restarts_desktop(cx: &mut gpui::TestAppContext) {
    runtime_callback(cx, false);
}

#[gpui::test]
fn failed_restart_snapshot_keeps_the_app_open_and_the_update_staged(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext;
    let updater = cx.new(|_| installed("0.2.0"));
    updater.update(cx, |updater, cx| {
        updater.prefs.manage_runtime = false;
        updater.staged = Some(staged("0.3.0", "0.9"));
        updater.restart_to_update(cx);
    });
    cx.run_until_parked();
    updater.read_with(cx, |updater, _| {
        assert!(!updater.relaunch);
        assert!(!updater.busy);
        assert!(updater.staged.is_some());
        assert!(
            updater
                .message
                .starts_with("Could not save desktop restart:")
        );
    });
}
