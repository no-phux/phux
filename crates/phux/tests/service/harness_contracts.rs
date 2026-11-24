//! Shared integration harness contracts, run once in the service target.
use std::ffi::OsStr;

use crate::common::{
    AutoSpawnedServer, NO_UPDATE_CHECK_ENV, NO_UPDATE_CHECK_VALUE, ServerSpawn, ambient_phux_keys,
    is_ambient_phux_key, isolated_attach_command, phux_cmd,
};

#[test]
fn ambient_phux_keys_cover_the_service_manager_exports() {
    let keys = ambient_phux_keys();
    for key in [
        "PHUX_SOCKET",
        "PHUX_WS_TOKENS",
        "PHUX_WS_TLS_CERT",
        "PHUX_WS_TLS_KEY",
        "PHUX_SERVICE_MANAGED",
    ] {
        assert!(
            keys.contains(&key.into()),
            "{key} must be scrubbed so a suite run from a live pane cannot \
             touch the operator store (phux-lru0)"
        );
    }
}

#[test]
fn every_phux_prefixed_key_is_ambient() {
    for key in ["PHUX_WORKLOAD_CA_KEY", "PHUX_PROFILE", "PHUX_UPLOAD_DIR"] {
        assert!(is_ambient_phux_key(OsStr::new(key)), "{key}");
    }
    for key in [
        "HOME",
        "XDG_STATE_HOME",
        "MY_PHUX_THING",
        "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE",
        NO_UPDATE_CHECK_ENV,
    ] {
        assert!(!is_ambient_phux_key(OsStr::new(key)), "{key}");
    }
}

#[test]
fn phux_cmd_disables_the_release_check() {
    assert_release_check_off(&phux_cmd("phux"));
}

#[test]
fn server_spawn_keeps_the_release_check_off_after_env_clear() {
    let (cmd, socket, _xdg, _home) = ServerSpawn::new("notice").env_clear().into_command(|cmd| {
        cmd.env_clear();
        cmd.env("PATH", "/usr/bin:/bin");
    });
    let _ = std::fs::remove_file(&socket);
    assert_release_check_off(&cmd);
}

fn assert_release_check_off(cmd: &std::process::Command) {
    let value = cmd
        .get_envs()
        .find_map(|(key, value)| (key == NO_UPDATE_CHECK_ENV).then_some(value));
    assert_eq!(value, Some(Some(OsStr::new(NO_UPDATE_CHECK_VALUE))));
}

/// The backstop must be the variable the server actually reads.
#[test]
fn the_idle_backstop_names_the_variable_the_server_reads() {
    let (key, value) = AutoSpawnedServer::IDLE_BACKSTOP;
    assert_eq!(key, phux::AUTO_SPAWN_IDLE_ENV);
    // The justfile's `AUTO_SPAWN_BACKSTOP` and docs/reference spell it
    // out, so a rename is a documentation change too, not a silent one.
    assert_eq!(key, "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE");
    assert!(
        value
            .parse::<u64>()
            .is_ok_and(|secs| (1..=86_400).contains(&secs)),
        "the backstop must be in the range the server accepts: {value}"
    );
}

/// A caller that clears the environment must not reopen the release check.
/// The opt-out is sealed after `configure`, which is what
/// [`crate::common::PtyAttach::start_dialing`] spawns.
#[test]
fn attach_command_keeps_the_release_check_off_after_env_clear() {
    let config = tempfile::tempdir().expect("config");
    let command = isolated_attach_command(&["work"], config.path(), |_config, command| {
        command.env_clear();
        command.env("PATH", "/usr/bin:/bin");
    });
    assert_eq!(
        command.get_env(NO_UPDATE_CHECK_ENV),
        Some(OsStr::new(NO_UPDATE_CHECK_VALUE)),
        "env_clear in configure must not drop the release-check opt-out"
    );
}
