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

/// The stand-in browser: the link on the person's clipboard (OSC 52, the
/// path copy uses) and in the pane. It opens nothing.
const BROWSER_SHIM: &str = "#!/bin/sh
printf '\\033]52;c;%s\\a' \"$(printf %s \"$1\" | base64 | tr -d '\\n')\" > /dev/tty
printf 'Open this on your own machine (copied to your clipboard):\\n%s\\n' \"$1\" > /dev/tty
";

/// `[defaults] browser-shim`: write the stand-in and point every pane's
/// `BROWSER` at it. A failure leaves panes as they were.
pub(super) fn browser_shim(enabled: bool) {
    use std::os::unix::fs::PermissionsExt as _;
    if !enabled {
        return;
    }
    let path = phux_config::instance::state_dir().join("phux-browser");
    let written = std::fs::write(&path, BROWSER_SHIM)
        .and_then(|()| std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)));
    match written {
        Ok(()) => phux_terminal_api::launch::set_pane_env(vec![("BROWSER".into(), path.into())]),
        Err(err) => warn!("browser shim: could not write {}: {err}", path.display()),
    }
}
