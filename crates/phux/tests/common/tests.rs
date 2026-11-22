//! Shared integration harness contracts.
use super::AutoSpawnedServer;

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
/// [`super::PtyAttach::start_dialing`] spawns.
#[test]
fn attach_command_keeps_the_release_check_off_after_env_clear() {
    let config = tempfile::tempdir().expect("config");
    let command = super::isolated_attach_command(&["work"], config.path(), |_config, command| {
        command.env_clear();
        command.env("PATH", "/usr/bin:/bin");
    });
    assert_eq!(
        command.get_env(super::ambient::NO_UPDATE_CHECK_ENV),
        Some(std::ffi::OsStr::new(super::ambient::NO_UPDATE_CHECK_VALUE)),
        "env_clear in configure must not drop the release-check opt-out"
    );
}
