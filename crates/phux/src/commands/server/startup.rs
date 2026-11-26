//! What the server does once around its start, beside serving.

use std::ffi::OsString;
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

/// Make the state directory owner-only before anything writes into it: every
/// other writer takes it as it finds it. Best effort; what needs it private
/// checks for itself.
pub(super) fn private_state_dir() {
    let dir = phux_config::instance::state_dir();
    let made =
        phux_server::owner_dir::create(&dir).and_then(|()| phux_server::owner_dir::tighten(&dir));
    if let Err(err) = made {
        tracing::debug!("could not make {} owner-only: {err}", dir.display());
    }
}

/// The stand-in browser: ask the person typing in the pane to open the link
/// on their machine, with the `phux` this server runs (`exe`). When nothing
/// opened it, put it on their clipboard (OSC 52, the path copy uses) and in
/// the pane.
fn browser_stand_in(exe: &str) -> String {
    format!(
        "#!/bin/sh
{} open-url -- \"$1\" </dev/null >/dev/null 2>&1 && exit 0
printf '\\033]52;c;%s\\a' \"$(printf %s \"$1\" | base64 | tr -d '\\n')\" > /dev/tty
printf 'Open this on your own machine (copied to your clipboard):\\n%s\\n' \"$1\" > /dev/tty
",
        shell_words::quote(exe)
    )
}

/// `[defaults] browser-shim`: write the stand-in, point every pane's
/// `BROWSER` at it, and put it last on their `PATH` as `xdg-open`, for a host
/// without one. A failure leaves panes as they were.
pub(super) fn browser_shim(enabled: bool) {
    if !enabled {
        return;
    }
    let dir = phux_config::instance::state_dir();
    match write_stand_ins(&dir) {
        Ok(env) => phux_terminal_api::launch::set_pane_env(env),
        Err(err) => warn!("browser shim: could not write to {}: {err}", dir.display()),
    }
}

/// Write both stand-ins under `dir`; the variables that point panes at them.
fn write_stand_ins(dir: &Path) -> std::io::Result<Vec<(OsString, OsString)>> {
    use std::os::unix::fs::PermissionsExt as _;
    let exe = std::env::current_exe()?;
    let script = browser_stand_in(&exe.to_string_lossy());
    let bin = dir.join("browser-shim");
    let browser = dir.join("phux-browser");
    phux_server::owner_dir::create(dir)?;
    phux_server::owner_dir::create(&bin)?;
    for path in [&browser, &bin.join("xdg-open")] {
        std::fs::write(path, &script)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    let mut env = vec![("BROWSER".into(), browser.into())];
    if let Some(path) = std::env::var_os("PATH") {
        let dirs = std::env::split_paths(&path).filter(|dir| *dir != bin);
        if let Ok(path) = std::env::join_paths(dirs.chain([bin.clone()])) {
            env.push(("PATH".into(), path));
        }
    }
    Ok(env)
}
