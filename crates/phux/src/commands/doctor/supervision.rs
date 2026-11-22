//! Linux supervision: a server systemd did not start does not come back after
//! a reboot, and on many systems ends at logout with every session in it.

use super::Check;

/// Is the live server running in this profile's systemd unit, and does the
/// user linger? `None` off Linux, with no live server (the `server` check
/// says so), or while an `--adopt` unit is armed (`server-health` says so).
pub(super) fn check_supervision(socket_path: &std::path::Path) -> Option<Check> {
    if !cfg!(target_os = "linux")
        || phux_config::socket::probe(socket_path) != phux_config::socket::SocketState::Live
        || crate::commands::service::armed_adoption_unit(socket_path).is_some()
    {
        return None;
    }
    let pid = crate::commands::diagnostics::server_pid(socket_path);
    let cgroup = pid.and_then(|pid| std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok());
    Some(supervision_check(
        pid,
        cgroup.as_deref(),
        &crate::commands::service::systemd_unit(),
        crate::commands::service::probe_linger,
    ))
}

/// The pure half: `cgroup` is `/proc/PID/cgroup`, `unit` the unit name the
/// server must run in. `linger` is only asked once the server is supervised.
pub(super) fn supervision_check(
    pid: Option<i32>,
    cgroup: Option<&str>,
    unit: &str,
    linger: impl FnOnce() -> bool,
) -> Check {
    let Some(cgroup) = cgroup else {
        return Check::warn(
            "supervision",
            "could not read the server's cgroup to tell whether systemd supervises it",
            "`phux service status` shows whether the unit is running",
        );
    };
    let supervised = cgroup
        .lines()
        .any(|line| line.rsplit('/').next() == Some(unit));
    let pid = pid.map_or_else(String::new, |pid| format!(" (pid {pid})"));
    if !supervised {
        return Check::warn(
            "supervision",
            format!(
                "the server{pid} is not supervised by systemd: it will not come back after a \
                 reboot, and may end at logout with its sessions"
            ),
            "run `phux service install --adopt`: the running server keeps its panes, and \
             systemd takes over at the next reboot or when this server exits",
        );
    }
    if !linger() {
        return Check::warn(
            "supervision",
            format!("supervised by {unit}{pid}, but linger is off"),
            "the server stops at logout and starts only at the next login, not at boot; \
             run `loginctl enable-linger` (or `sudo loginctl enable-linger $USER`)",
        );
    }
    Check::pass(
        "supervision",
        format!("supervised by {unit}{pid}; linger on"),
    )
}
