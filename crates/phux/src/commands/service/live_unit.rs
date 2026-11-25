//! A unit rewritten under the server it already runs.
//!
//! `phux service install --adopt` over a running server rewrites or arms the
//! unit, and when the unit names a listener that server does not have,
//! re-execs it with `phux upgrade`'s hand-over, panes kept. The re-exec keeps
//! the old process's argv and environment, so the resumed image reads the
//! listener addresses from the unit itself. A listener that cannot be opened
//! leaves the policy and the unit as they were.

use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use phux_protocol::wire::RemoteListenerSlot;
use phux_protocol::wire::RemoteListenerTransport::{self, Quic, Wss};

use super::{
    Manager, ServicePlan, Subject, SupervisionState, adoption_marker_path, clear_adoption_pending,
    launchd_target, profile_suffix, render_unit, run_tool, settle_policy, supervision_state,
    systemd_unit, systemd_unquote, unit_supervises, write_unit_files, xml_escape, xml_unescape,
};

/// Leads the refusal of a listener whose port is in use. `phux host add`
/// recognizes it over ssh, so it is a contract with that caller.
pub(crate) const PORT_TAKEN: &str = "another program on this machine already uses";

/// How long a re-executed server has to open the listeners its unit names.
const BIND_WAIT: Duration = Duration::from_secs(15);

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
    let unit_path = match manager.writable_unit_path(profile_suffix().as_deref()) {
        Ok(path) => path,
        Err(err) => {
            error!("service: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut before = Before::read(manager, &unit_path);
    let old = before.unit.clone().unwrap_or_default();
    let new = match manager {
        Manager::Systemd => render_unit(manager, plan),
        // launchd keeps the loaded job's argv until bootout, so only the
        // listeners a resume reads change here; its other flags stay.
        Manager::Launchd => launchd_listeners(&old, plan),
    };
    if old != new {
        if let Err(err) = before.settle_policy(plan).and_then(|()| match manager {
            Manager::Systemd => write_unit_files(manager, plan, &unit_path),
            Manager::Launchd => std::fs::write(&unit_path, &new)
                .map_err(|err| format!("could not write {}: {err}", unit_path.display())),
        }) {
            return before.refuse(plan, &err);
        }
        // Re-reads the unit without touching the running server.
        daemon_reload(manager);
        outln!("  updated {}", unit_path.display());
    }
    match bind_listeners(plan) {
        Ok(rebound) => {
            if rebound {
                outln!("Restarted the server in place on its new listener (panes kept).");
            }
            ExitCode::SUCCESS
        }
        Err(err) => before.refuse(plan, &err),
    }
}

/// `body` with the plan's listener addresses in its environment.
fn launchd_listeners(body: &str, plan: &ServicePlan) -> String {
    let mut lines: Vec<String> = body.split('\n').map(str::to_owned).collect();
    for (key, value) in LISTENER_KEYS.into_iter().zip([&plan.listen, &plan.quic]) {
        let Some(value) = value else { continue };
        let key_line = format!("<key>{key}</key>");
        let value_line = format!("<string>{}</string>", xml_escape(value));
        if let Some(at) = lines.iter().position(|line| line.trim() == key_line) {
            if let Some(line) = lines.get_mut(at + 1) {
                *line = format!("{}{value_line}", super::line_indent(line));
            }
        } else if let Some(at) = lines
            .iter()
            .position(|line| line.trim() == "<key>EnvironmentVariables</key>")
            && lines
                .get(at + 1)
                .is_some_and(|line| line.trim() == "<dict>")
        {
            let indent = format!("{}  ", super::line_indent(&lines[at + 1]));
            lines.insert(at + 2, format!("{indent}{value_line}"));
            lines.insert(at + 2, format!("{indent}{key_line}"));
        }
    }
    lines.join("\n")
}

fn daemon_reload(manager: Manager) {
    if manager == Manager::Systemd
        && let Err(err) = run_tool(
            "systemctl",
            &["--user".to_owned(), "daemon-reload".to_owned()],
        )
    {
        warn!("service: {err}");
    }
}

/// The unit and config as an install found them, so one that cannot open its
/// listener leaves nothing half applied.
pub(super) struct Before {
    manager: Manager,
    unit_path: std::path::PathBuf,
    unit: Option<String>,
    armed: bool,
    /// `config.toml` before and after this install wrote `[policy] mode`.
    config: Option<(Option<Vec<u8>>, Vec<u8>)>,
}

impl Before {
    pub(super) fn read(manager: Manager, unit_path: &Path) -> Self {
        Self {
            manager,
            unit_path: unit_path.to_owned(),
            unit: std::fs::read_to_string(unit_path).ok(),
            armed: adoption_marker_path().exists(),
            config: None,
        }
    }

    /// [`settle_policy`], remembering what it wrote.
    pub(super) fn settle_policy(&mut self, plan: &ServicePlan) -> Result<(), String> {
        let path = phux_config::loader::config_path();
        let old = std::fs::read(&path).ok();
        if settle_policy(plan)? {
            self.config = Some((old, std::fs::read(&path).unwrap_or_default()));
        }
        Ok(())
    }

    /// Report why the install stopped, with everything put back.
    pub(super) fn refuse(self, plan: &ServicePlan, why: &str) -> ExitCode {
        let restored = self.restore(plan);
        error!("service: {why}");
        match restored {
            Ok(()) => info!("service: the config and the unit are as they were"),
            Err(err) => warn!("service: could not put everything back: {err}"),
        }
        ExitCode::FAILURE
    }

    fn restore(self, plan: &ServicePlan) -> Result<(), String> {
        if let Some(unit) = &self.unit {
            std::fs::write(&self.unit_path, unit)
                .map_err(|err| format!("{}: {err}", self.unit_path.display()))?;
        } else {
            if self.manager == Manager::Systemd && self.unit_path.exists() {
                let _ = run_tool(
                    "systemctl",
                    &["--user".to_owned(), "disable".to_owned(), systemd_unit()],
                );
            }
            if self.unit_path.exists() {
                std::fs::remove_file(&self.unit_path)
                    .map_err(|err| format!("{}: {err}", self.unit_path.display()))?;
            }
            let _ = std::fs::remove_file(&plan.wrapper);
        }
        daemon_reload(self.manager);
        if !self.armed {
            clear_adoption_pending();
        }
        let Some((old, wrote)) = self.config else {
            return Ok(());
        };
        let path = phux_config::loader::config_path();
        // Someone else's edit since stays: only the mode goes.
        if std::fs::read(&path).is_ok_and(|now| now != wrote) {
            return phux_config::settings::write_edit(
                &path,
                "policy.mode",
                phux_config::settings::Edit::Unset,
            )
            .map(drop)
            .map_err(|err| err.to_string());
        }
        old.map_or_else(
            || std::fs::remove_file(&path),
            |old| std::fs::write(&path, old),
        )
        .map_err(|err| format!("{}: {err}", path.display()))
    }
}

/// Re-exec the server on the plan's socket when it lacks a listener the plan
/// names, and wait for it to open; whether it was re-executed.
pub(super) fn bind_listeners(plan: &ServicePlan) -> Result<bool, String> {
    if missing_listeners(plan).is_none_or(|missing| missing.is_empty()) {
        return Ok(false);
    }
    crate::commands::pair::rebind_server(&plan.socket_path)?;
    let deadline = Instant::now() + BIND_WAIT;
    loop {
        match missing_listeners(plan) {
            Some(missing) if missing.is_empty() => return Ok(true),
            Some(missing) if Instant::now() >= deadline => {
                return Err(format!(
                    "the running server could not open {}",
                    missing.join(", ")
                ));
            }
            None if Instant::now() >= deadline => {
                return Err("the server did not answer after it reloaded".to_owned());
            }
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// The listeners the plan names that the running server has not bound, each
/// in words; `None` when the server cannot be asked.
fn missing_listeners(plan: &ServicePlan) -> Option<Vec<String>> {
    let slots = server_slots(plan)?;
    let missing = |(transport, addr): (RemoteListenerTransport, &str)| {
        let slot = slots.iter().find(|slot| slot.transport == transport);
        match slot {
            Some(slot) if slot.bound => None,
            Some(slot) => Some(format!("{transport} on {addr}: {}", slot.unbound_label())),
            None => Some(format!("{transport} on {addr}")),
        }
    };
    Some(plan_listeners(plan).filter_map(missing).collect())
}

/// Every listener the server on the plan's socket reports.
fn server_slots(plan: &ServicePlan) -> Option<Vec<RemoteListenerSlot>> {
    let runtime = crate::commands::build_cli_runtime().ok()?;
    let view = runtime
        .block_on(phux_client::state::get_state(&plan.socket_path))
        .ok()?;
    let report = view.snapshot().listeners();
    Some(report.map_or_else(Vec::new, |report| report.listeners.clone()))
}

fn plan_listeners(plan: &ServicePlan) -> impl Iterator<Item = (RemoteListenerTransport, &str)> {
    [(Wss, &plan.listen), (Quic, &plan.quic)]
        .into_iter()
        .filter_map(|(transport, addr)| Some((transport, addr.as_deref()?)))
}

/// The plan's listener whose port another program holds, in words. Checked
/// before anything is written, because the supervised server could only wait
/// for it. The running server's own port is not another program's.
pub(super) fn taken_port(plan: &ServicePlan, incumbent_live: bool) -> Option<String> {
    let slots = if incumbent_live {
        server_slots(plan)?
    } else {
        Vec::new()
    };
    plan_listeners(plan).find_map(|(transport, addr)| {
        let addr: SocketAddr = addr.parse().ok()?;
        let held_here = |slot: &RemoteListenerSlot| {
            slot.transport == transport
                && slot.bound
                && slot
                    .addr
                    .as_deref()
                    .and_then(|bound| bound.parse::<SocketAddr>().ok())
                    .is_none_or(|bound| bound.port() == addr.port())
        };
        if slots.iter().any(held_here) {
            return None;
        }
        let (kind, bound) = match transport {
            Wss => ("TCP", std::net::TcpListener::bind(addr).map(drop)),
            _ => ("UDP", std::net::UdpSocket::bind(addr).map(drop)),
        };
        (bound.err()?.kind() == std::io::ErrorKind::AddrInUse)
            .then(|| format!("{PORT_TAKEN} {kind} port {}", addr.port()))
    })
}

/// The `--listen` / `--quic` a resuming server binds: the flags it was
/// handed, else the ones its unit names when that unit is armed for this
/// server or this process is the unit's main process.
pub(crate) fn listeners_on_resume(
    socket_path: &Path,
    listen: Option<SocketAddr>,
    quic: Option<SocketAddr>,
) -> (Option<SocketAddr>, Option<SocketAddr>) {
    if listen.is_some() && quic.is_some() {
        return (listen, quic);
    }
    own_unit(socket_path).map_or((listen, quic), |(manager, body)| {
        unit_listeners(manager, &body, listen, quic)
    })
}

/// The flags, each absent one filled from the unit's environment.
fn unit_listeners(
    manager: Manager,
    body: &str,
    listen: Option<SocketAddr>,
    quic: Option<SocketAddr>,
) -> (Option<SocketAddr>, Option<SocketAddr>) {
    let addr = |key| unit_env(manager, body, key).and_then(|value| value.parse().ok());
    let [ws_key, quic_key] = LISTENER_KEYS;
    (
        listen.or_else(|| addr(ws_key)),
        quic.or_else(|| addr(quic_key)),
    )
}

/// This profile's unit, when it supervises `socket_path` and is armed for
/// the server there or runs this process (an upgrade keeps the pid).
fn own_unit(socket_path: &Path) -> Option<(Manager, String)> {
    if let SupervisionState::Armed { manager, unit } =
        supervision_state(Subject::Server(socket_path))
    {
        return Some((manager, std::fs::read_to_string(unit).ok()?));
    }
    let manager = Manager::host()?;
    let unit = manager.unit_path(profile_suffix().as_deref()).ok()?;
    let body = std::fs::read_to_string(unit).ok()?;
    if !unit_supervises(manager, &body, socket_path) {
        return None;
    }
    (main_pid(manager)? == std::process::id()).then_some((manager, body))
}

/// The main process the init system runs this profile's unit as.
fn main_pid(manager: Manager) -> Option<u32> {
    let ask = |tool: &str, args: &[&str]| {
        let output = std::process::Command::new(tool).args(args).output().ok()?;
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    match manager {
        Manager::Systemd => ask(
            "systemctl",
            &[
                "--user",
                "show",
                "--property=MainPID",
                "--value",
                &systemd_unit(),
            ],
        )?
        .trim()
        .parse()
        .ok(),
        Manager::Launchd => ask("launchctl", &["print", &launchd_target()])?
            .lines()
            .find_map(|line| line.trim().strip_prefix("pid = ")?.parse().ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resume_takes_the_listener_its_unit_names_unless_handed_one() {
        let body = "[Service]\nEnvironment=\"PHUX_QUIC_ADDR=0.0.0.0:8788\"\n";
        let flag: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        assert_eq!(
            unit_listeners(Manager::Systemd, body, None, None),
            (None, Some("0.0.0.0:8788".parse().unwrap()))
        );
        assert_eq!(
            unit_listeners(Manager::Systemd, body, None, Some(flag)),
            (None, Some(flag))
        );
        assert_eq!(
            unit_listeners(Manager::Systemd, "[Service]\n", None, None),
            (None, None)
        );
    }
}
