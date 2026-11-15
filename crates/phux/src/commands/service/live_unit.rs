//! A unit rewritten under the server it already runs.
//!
//! `phux service install --adopt` over this unit's own running server
//! rewrites a changed unit, and when the unit now names a listener the old
//! one did not, re-execs that server with `phux upgrade`'s hand-over, panes
//! kept. The re-exec keeps the old process's argv and environment, so the
//! resumed image reads the listener addresses from the unit itself.

use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;

use super::{
    Manager, ServicePlan, profile_suffix, render_unit, run_tool, settle_policy, systemd_unit,
    systemd_unquote, unit_supervises, write_unit_files, xml_unescape,
};

/// The listener variables a unit carries, in the order the flags take them.
const LISTENER_KEYS: [&str; 2] = ["PHUX_WS_ADDR", "PHUX_QUIC_ADDR"];

/// The value of `key` in a unit's environment.
pub(super) fn unit_env(manager: Manager, body: &str, key: &str) -> Option<String> {
    match manager {
        Manager::Launchd => {
            let key_line = format!("<key>{key}</key>");
            let at = body.lines().position(|line| line.trim() == key_line)?;
            let value = body.lines().nth(at + 1)?.trim();
            let inner = value.strip_prefix("<string>")?.strip_suffix("</string>")?;
            Some(xml_unescape(inner))
        }
        Manager::Systemd => {
            let prefix = format!("Environment=\"{key}=");
            let value = body
                .lines()
                .find_map(|line| line.trim().strip_prefix(prefix.as_str())?.strip_suffix('"'))?;
            Some(systemd_unquote(value))
        }
    }
}

/// `--adopt` over the server this unit already runs: write a changed unit,
/// and hand the server over live when it lacks a listener the unit names.
pub(super) fn refresh_running(manager: Manager, plan: &ServicePlan) -> ExitCode {
    // launchd keeps the loaded job's argv until bootout, and a resume reads
    // only a systemd unit.
    if manager != Manager::Systemd {
        return ExitCode::SUCCESS;
    }
    let unit_path = match manager.writable_unit_path(profile_suffix().as_deref()) {
        Ok(path) => path,
        Err(err) => {
            error!("service: {err}");
            return ExitCode::FAILURE;
        }
    };
    let old = std::fs::read_to_string(&unit_path).unwrap_or_default();
    if old != render_unit(manager, plan) {
        if let Err(err) =
            settle_policy(plan).and_then(|()| write_unit_files(manager, plan, &unit_path))
        {
            error!("service: {err}");
            return ExitCode::FAILURE;
        }
        // Re-reads the unit without touching the running server.
        if let Err(err) = run_tool(
            "systemctl",
            &["--user".to_owned(), "daemon-reload".to_owned()],
        ) {
            warn!("service: {err}");
        }
        outln!("  updated {}", unit_path.display());
    }
    if !lacks_listener(plan) {
        return ExitCode::SUCCESS;
    }
    match crate::commands::pair::rebind_server(&plan.socket_path) {
        Ok(()) => {
            outln!("Restarted the server in place on its new listener (panes kept).");
            ExitCode::SUCCESS
        }
        Err(err) => {
            error!(
                "service: the unit is updated, but {err}; `phux upgrade` brings the listener up"
            );
            ExitCode::FAILURE
        }
    }
}

/// Whether the running server has no bound listener for a transport the
/// plan names. A server that cannot be asked counts as having them.
fn lacks_listener(plan: &ServicePlan) -> bool {
    use phux_protocol::wire::RemoteListenerTransport::{Quic, Wss};

    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    let Ok(view) = runtime.block_on(phux_client::state::get_state(&plan.socket_path)) else {
        return false;
    };
    let bound = |transport| {
        view.snapshot().listeners().is_some_and(|report| {
            report
                .listeners
                .iter()
                .any(|slot| slot.transport == transport && slot.bound)
        })
    };
    (plan.listen.is_some() && !bound(Wss)) || (plan.quic.is_some() && !bound(Quic))
}

/// The `--listen` / `--quic` a resuming server binds: the flags it was
/// handed, else the ones its systemd unit names when this process is that
/// unit's main process.
pub(crate) fn listeners_on_resume(
    socket_path: &Path,
    listen: Option<SocketAddr>,
    quic: Option<SocketAddr>,
) -> (Option<SocketAddr>, Option<SocketAddr>) {
    if listen.is_some() && quic.is_some() {
        return (listen, quic);
    }
    own_unit(socket_path).map_or((listen, quic), |body| unit_listeners(&body, listen, quic))
}

/// The flags, each absent one filled from the unit's environment.
fn unit_listeners(
    body: &str,
    listen: Option<SocketAddr>,
    quic: Option<SocketAddr>,
) -> (Option<SocketAddr>, Option<SocketAddr>) {
    let addr = |key| unit_env(Manager::Systemd, body, key).and_then(|value| value.parse().ok());
    let [ws_key, quic_key] = LISTENER_KEYS;
    (
        listen.or_else(|| addr(ws_key)),
        quic.or_else(|| addr(quic_key)),
    )
}

/// This profile's systemd unit, when it supervises `socket_path` and this
/// process is its main process (an upgrade keeps the pid).
fn own_unit(socket_path: &Path) -> Option<String> {
    if Manager::host() != Some(Manager::Systemd) {
        return None;
    }
    let unit = Manager::Systemd
        .unit_path(profile_suffix().as_deref())
        .ok()?;
    let body = std::fs::read_to_string(unit).ok()?;
    if !unit_supervises(Manager::Systemd, &body, socket_path) {
        return None;
    }
    let main_pid = std::process::Command::new("systemctl")
        .args(["--user", "show", "--property=MainPID", "--value"])
        .arg(systemd_unit())
        .output()
        .ok()?;
    (String::from_utf8_lossy(&main_pid.stdout).trim() == std::process::id().to_string())
        .then_some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resume_takes_the_listener_its_unit_names_unless_handed_one() {
        let body = "[Service]\nEnvironment=\"PHUX_QUIC_ADDR=0.0.0.0:8788\"\n";
        let flag: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        assert_eq!(
            unit_listeners(body, None, None),
            (None, Some("0.0.0.0:8788".parse().unwrap()))
        );
        assert_eq!(unit_listeners(body, None, Some(flag)), (None, Some(flag)));
        assert_eq!(unit_listeners("[Service]\n", None, None), (None, None));
    }
}
