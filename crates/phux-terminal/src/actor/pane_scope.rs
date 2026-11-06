//! Give each pane its own systemd scope on Linux (#2006).
//!
//! systemd-oomd and the kernel memory controller kill whole cgroups. A pane
//! left in the server's cgroup shares its fate, so one runaway pane gets the
//! server and every other pane killed with it. Moving the pane into a
//! transient `app-phux-pane-<pid>.scope` makes it a leaf of its own, the way
//! Ghostty scopes each surface.
//!
//! Best effort: with no user bus or no `busctl` the pane stays where it is.
//! The move runs off-thread after spawn, so anything the shell forks in its
//! first milliseconds stays behind in the server's cgroup.

use std::process::{Command, Stdio};

/// Ask the user manager to move `pid` into its own scope, without waiting.
pub(super) fn detach(pid: u32) {
    if std::env::var_os("PHUX_NO_PANE_SCOPE").is_some() || !user_bus_present() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("pane-scope".into())
        .spawn(move || {
            let result = Command::new("busctl")
                .args(busctl_args(pid))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            tracing::debug!(pid, ?result, "pane scope request");
        });
    if let Err(err) = spawned {
        tracing::debug!(pid, ?err, "pane scope thread failed to start");
    }
}

/// The user bus `busctl --user` dials: an explicit address, or the socket
/// systemd puts in the runtime dir.
fn user_bus_present() -> bool {
    std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
        || std::env::var_os("XDG_RUNTIME_DIR")
            .is_some_and(|dir| std::path::Path::new(&dir).join("bus").exists())
}

/// `StartTransientUnit(name, mode, properties, aux)` for a scope holding `pid`.
fn busctl_args(pid: u32) -> Vec<String> {
    [
        "--user",
        "call",
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
        "StartTransientUnit",
        "ssa(sv)a(sa(sv))",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain([
        format!("app-phux-pane-{pid}.scope"),
        "fail".into(),
        "3".into(),
        "PIDs".into(),
        "au".into(),
        "1".into(),
        pid.to_string(),
        "CollectMode".into(),
        "s".into(),
        "inactive-or-failed".into(),
        "Description".into(),
        "s".into(),
        "phux pane".into(),
        "0".into(),
    ])
    .collect()
}

#[cfg(test)]
mod tests {
    use super::busctl_args;

    #[test]
    fn scope_request_names_the_pane_and_carries_its_pid() {
        let args = busctl_args(4242);
        let tail = &args[7..];
        assert_eq!(tail[0], "app-phux-pane-4242.scope");
        assert_eq!(tail[1], "fail");
        // Three properties, PIDs first with one element: the pane.
        assert_eq!(&tail[2..7], ["3", "PIDs", "au", "1", "4242"]);
        assert_eq!(args.last().map(String::as_str), Some("0"));
    }
}
