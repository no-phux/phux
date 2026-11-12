//! What the server does once around its start, beside serving.

use std::path::Path;

/// `[restore] on-start`: run `phux restore` against this server as its own
/// process, so a slow relaunch never holds up the listener. After an upgrade
/// handoff nothing ended, and it finds nothing to do.
pub(super) fn restore_in_background(socket: &Path) {
    let spawned = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .arg("--socket")
            .arg(socket)
            .arg("restore")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    });
    match spawned {
        // Reaped on a thread, so it leaves no zombie.
        Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
        Err(err) => warn!("restore on start: could not run phux restore: {err}"),
    }
}

/// `[defaults] shell-integration`: write the files that make zsh and fish
/// mark their prompts, and have every default shell started from here load
/// them (bash needs no file). A failure leaves panes as they were.
pub(super) fn shell_integration(enabled: bool) {
    if !enabled {
        return;
    }
    let dir = phux_config::instance::state_dir().join("shell-integration");
    match crate::commands::shell_integration::write_files(&dir) {
        Ok(()) => phux_terminal_api::launch::enable_shell_integration(dir),
        Err(err) => warn!(
            "shell integration: could not write {}: {err}",
            dir.display()
        ),
    }
}
