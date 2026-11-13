//! "What works here": what a user expects of this host, each with a verdict,
//! the reason in plain words, and the fix. Built from the doctor checks and
//! a few read-only probes; `phux doctor --json` carries it as `checklist`,
//! and the server answers `HOST_CHECKLIST` by running exactly that, so the
//! CLI and every client show the same list (`docs/troubleshooting.md`).

use std::path::Path;

use phux_protocol::wire::{ListenerDisabledReason, RemoteListenersReport};

use super::{Check, Status, cli_runtime};
use crate::commands::service::supervision::Supervision;
use crate::skill::setup::Integration;

/// Version of the `checklist` document. Adding an item or a field is not a
/// bump; renaming or removing one is.
pub(crate) const CHECKLIST_SCHEMA_VERSION: u32 = 1;

/// One item's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Works.
    Ok,
    /// Does not work; `reason` says why.
    Off,
    /// Not working yet, and will without help (a listener waiting to bind).
    Waiting,
    /// Could not be checked.
    Unknown,
}

impl Verdict {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Off => "off",
            Self::Waiting => "waiting",
            Self::Unknown => "unknown",
        }
    }
}

/// One line of the checklist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Item {
    id: &'static str,
    title: &'static str,
    verdict: Verdict,
    reason: String,
    /// What to run on this host, one command per line.
    fix_command: Option<String>,
    /// What a client can do in one tap instead (`enable_notifications`).
    fix_action: Option<&'static str>,
    /// The notifications item only: the device ids holding a push grant,
    /// so a phone can tell whether it is one of them.
    push_devices: Option<Vec<String>>,
}

impl Item {
    fn new(
        id: &'static str,
        title: &'static str,
        verdict: Verdict,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id,
            title,
            verdict,
            reason: reason.into(),
            fix_command: None,
            fix_action: None,
            push_devices: None,
        }
    }

    fn fix(mut self, command: impl Into<String>) -> Self {
        self.fix_command = Some(command.into());
        self
    }
}

/// Everything the checklist is built from, gathered once.
#[derive(Debug)]
pub(super) struct Inputs {
    pub(super) integrations: Result<Vec<Integration>, String>,
    pub(super) shim: Option<Check>,
    pub(super) skill: Option<Check>,
    pub(super) reachable: Option<Check>,
    /// `None` when no server answered.
    pub(super) server: Option<ServerInputs>,
    pub(super) supervision: Supervision,
}

/// What the running server said.
#[derive(Debug)]
pub(super) struct ServerInputs {
    pub(super) listeners: Option<RemoteListenersReport>,
    pub(super) push_devices: Option<Vec<String>>,
    pub(super) file_download: bool,
}

/// Probe the server on `socket_path`; `checks` is the doctor run's own.
pub(super) fn gather(socket_path: &Path, checks: &[Check]) -> Inputs {
    let find = |name: &str| checks.iter().find(|check| check.name == name).cloned();
    let server = socket_path
        .exists()
        .then(cli_runtime)
        .and_then(Result::ok)
        .and_then(|rt| {
            let facts = rt
                .block_on(phux_client::checklist::host_facts(socket_path))
                .ok()?;
            let listeners = rt
                .block_on(phux_client::state::get_state(socket_path))
                .ok()
                .and_then(|view| view.snapshot().listeners().cloned());
            Some(ServerInputs {
                listeners,
                push_devices: facts.push_devices,
                file_download: facts.file_download,
            })
        });
    Inputs {
        integrations: crate::skill::setup::agent_integrations(),
        shim: find("agent-shim"),
        skill: find("skill"),
        reachable: find("remote-reachable"),
        server,
        supervision: crate::commands::service::supervision::supervision(socket_path),
    }
}

/// The checklist, in display order.
pub(super) fn items(inputs: &Inputs) -> Vec<Item> {
    vec![
        agent_status(&inputs.integrations, inputs.shim.as_ref()),
        notifications(inputs.server.as_ref()),
        remote_reach(inputs.server.as_ref(), inputs.reachable.as_ref()),
        file_transfer(inputs.server.as_ref()),
        restore(inputs.supervision, inputs.server.is_some()),
        skill(inputs.skill.as_ref()),
    ]
}

const NO_SERVER: &str = "no server is running on this host";
const START_SERVER: &str = "phux server --ensure";

fn agent_status(integrations: &Result<Vec<Integration>, String>, shim: Option<&Check>) -> Item {
    const ID: &str = "agent_status";
    const TITLE: &str = "Agent status";
    let integrations = match integrations {
        Ok(integrations) => integrations,
        Err(err) => return Item::new(ID, TITLE, Verdict::Unknown, err.clone()),
    };
    if integrations.is_empty() {
        return Item::new(
            ID,
            TITLE,
            Verdict::Off,
            "no coding agent found on this host",
        );
    }
    let names = |want: Option<bool>| {
        integrations
            .iter()
            .filter(|i| i.installed == want)
            .map(|i| i.agent)
            .collect::<Vec<_>>()
    };
    let missing = names(Some(false));
    let mut fixes: Vec<&str> = integrations
        .iter()
        .filter(|i| i.installed == Some(false))
        .map(|i| i.install)
        .collect();
    let mut reasons = Vec::new();
    if !missing.is_empty() {
        reasons.push(format!("no agent integration for {}", missing.join(", ")));
    }
    if shim.is_some_and(|check| check.status == Status::Warn) {
        reasons.push("the claude-in-phux wrapper is out of date".to_owned());
        fixes.push("phux agent install-claude");
    }
    if !reasons.is_empty() {
        return Item::new(ID, TITLE, Verdict::Off, reasons.join("; ")).fix(fixes.join("\n"));
    }
    let installed = names(Some(true));
    let screen = names(None);
    let mut parts = Vec::new();
    if !installed.is_empty() {
        parts.push(format!(
            "reported by the {} integration",
            installed.join(", ")
        ));
    }
    if !screen.is_empty() {
        parts.push(format!(
            "{} read from the screen (no integration exists yet)",
            screen.join(", ")
        ));
    }
    Item::new(ID, TITLE, Verdict::Ok, parts.join("; "))
}

fn notifications(server: Option<&ServerInputs>) -> Item {
    const ID: &str = "notifications";
    const TITLE: &str = "Asks and notifications";
    let Some(server) = server else {
        return Item::new(ID, TITLE, Verdict::Unknown, NO_SERVER).fix(START_SERVER);
    };
    let Some(devices) = server.push_devices.clone() else {
        return Item::new(
            ID,
            TITLE,
            Verdict::Unknown,
            "the server would not list which phones it notifies",
        );
    };
    let item = match devices.len() {
        0 => Item {
            fix_action: Some("enable_notifications"),
            ..Item::new(
                ID,
                TITLE,
                Verdict::Off,
                "no phone has turned on notifications for this host",
            )
        },
        1 => Item::new(
            ID,
            TITLE,
            Verdict::Ok,
            "1 phone gets notifications from this host",
        ),
        n => Item::new(
            ID,
            TITLE,
            Verdict::Ok,
            format!("{n} phones get notifications from this host"),
        ),
    };
    Item {
        push_devices: Some(devices),
        ..item
    }
}

fn remote_reach(server: Option<&ServerInputs>, reachable: Option<&Check>) -> Item {
    const ID: &str = "remote_reach";
    const TITLE: &str = "Remote reach";
    let Some(server) = server else {
        return Item::new(ID, TITLE, Verdict::Unknown, NO_SERVER).fix(START_SERVER);
    };
    let Some(report) = server
        .listeners
        .as_ref()
        .filter(|r| !r.listeners.is_empty())
    else {
        return Item::new(
            ID,
            TITLE,
            Verdict::Off,
            "no remote listener, so only this machine can reach it",
        )
        .fix("phux pair");
    };
    let unhealthy: Vec<_> = report.unhealthy().collect();
    if !unhealthy.is_empty() {
        let mut verdict = Verdict::Waiting;
        let mut fix = None;
        let reasons: Vec<_> = unhealthy
            .iter()
            .map(|slot| {
                let (words, waiting, command) = slot.disabled_reason.map_or(
                    ("the reason is unknown", false, Some("phux upgrade")),
                    listener_words,
                );
                if !waiting {
                    verdict = Verdict::Off;
                }
                fix = fix.or(command);
                format!("{} listener not bound: {words}", slot.transport)
            })
            .collect();
        let item = Item::new(ID, TITLE, verdict, reasons.join("; "));
        return match fix {
            Some(command) => item.fix(command),
            None => item,
        };
    }
    if let Some(check) = reachable.filter(|check| check.status == Status::Fail) {
        return Item::new(ID, TITLE, Verdict::Off, check.detail.clone());
    }
    let bound = report
        .listeners
        .iter()
        .filter(|slot| slot.bound)
        .map(|slot| {
            slot.addr.as_ref().map_or_else(
                || slot.transport.to_string(),
                |addr| format!("{} {addr}", slot.transport),
            )
        })
        .collect::<Vec<_>>();
    Item::new(
        ID,
        TITLE,
        Verdict::Ok,
        format!("listening on {}", bound.join(", ")),
    )
}

/// A disabled listener in plain words: why, whether it is only waiting, and
/// the command that re-binds it. `phux upgrade` re-binds with panes intact.
///
/// Hook for retried binds (#2401): a reason that means "will bind by itself
/// once the address exists" returns `waiting = true`, which turns the item
/// [`Verdict::Waiting`] instead of [`Verdict::Off`].
const fn listener_words(
    reason: ListenerDisabledReason,
) -> (&'static str, bool, Option<&'static str>) {
    use ListenerDisabledReason as R;
    match reason {
        R::BindFailed => (
            "the address could not be bound (in use, or not available yet)",
            false,
            Some("phux upgrade"),
        ),
        R::NoOverlayAddress => (
            "no private network address was found; start your VPN",
            false,
            Some("phux upgrade"),
        ),
        R::OverlayGateClosed => (
            "listening on the private network is turned off (PHUX_NO_AUTO_LISTEN, or a non-default profile)",
            false,
            None,
        ),
        R::TlsSetupFailed => ("TLS setup failed", false, Some("phux upgrade")),
        R::CertProvisionFailed => (
            "the certificate could not be created",
            false,
            Some("phux upgrade"),
        ),
        R::TokenStoreLoadFailed => (
            "the credential store did not load",
            false,
            Some("phux upgrade"),
        ),
        R::PolicyModeRefused => (
            "[policy] mode admits no remote listener; set mode = \"paired\" in config.toml",
            false,
            Some("phux upgrade"),
        ),
        // A reason newer than this binary.
        _ => ("see the server log", false, Some("phux upgrade")),
    }
}

fn file_transfer(server: Option<&ServerInputs>) -> Item {
    const ID: &str = "file_transfer";
    const TITLE: &str = "File transfer";
    match server {
        None => Item::new(ID, TITLE, Verdict::Unknown, NO_SERVER).fix(START_SERVER),
        Some(server) if server.file_download => Item::new(
            ID,
            TITLE,
            Verdict::Ok,
            "files can be sent to this host and fetched from it",
        ),
        Some(_) => Item::new(
            ID,
            TITLE,
            Verdict::Off,
            "this host's server is too old to send files",
        )
        .fix("phux upgrade"),
    }
}

fn restore(supervision: Supervision, server_up: bool) -> Item {
    const ID: &str = "restore";
    const TITLE: &str = "Restore after restart";
    // A running server must be adopted, or the unit would fight it for the
    // socket (ADR-0088).
    let install = if server_up {
        "phux service install --adopt"
    } else {
        "phux service install"
    };
    match supervision {
        Supervision::Unsupported => Item::new(
            ID,
            TITLE,
            Verdict::Unknown,
            "phux cannot install a service on this platform; run `phux server` under your own supervisor",
        ),
        Supervision::Unsupervised => Item::new(
            ID,
            TITLE,
            Verdict::Off,
            "server not supervised, sessions end at logout",
        )
        .fix(install),
        Supervision::Armed => Item::new(
            ID,
            TITLE,
            Verdict::Waiting,
            "supervision starts when this server next exits, or at the next login",
        ),
        Supervision::Stopped => Item::new(
            ID,
            TITLE,
            Verdict::Off,
            "a service is installed, but the init system is not running it",
        )
        .fix(install),
        Supervision::Running {
            manager,
            linger: false,
        } => Item::new(
            ID,
            TITLE,
            Verdict::Off,
            format!("supervised by {manager}, but sessions end at logout: lingering is off"),
        )
        .fix("loginctl enable-linger"),
        Supervision::Running { manager, .. } => Item::new(
            ID,
            TITLE,
            Verdict::Ok,
            format!("supervised by {manager}, so the server comes back after logout and reboot"),
        ),
    }
}

fn skill(check: Option<&Check>) -> Item {
    const ID: &str = "skill";
    const TITLE: &str = "Agent skill";
    let Some(check) = check else {
        return Item::new(ID, TITLE, Verdict::Unknown, "not checked");
    };
    // Not installed passes doctor with this hint; stale warns with it.
    if check
        .hint
        .as_deref()
        .is_some_and(|hint| hint.contains("phux skill setup"))
    {
        return Item::new(ID, TITLE, Verdict::Off, check.detail.clone()).fix("phux skill setup");
    }
    let verdict = if check.status == Status::Pass {
        Verdict::Ok
    } else {
        Verdict::Unknown
    };
    Item::new(ID, TITLE, verdict, check.detail.clone())
}

/// The `checklist` document.
pub(super) fn to_json(items: &[Item]) -> serde_json::Value {
    let rows: Vec<_> = items
        .iter()
        .map(|item| {
            let mut row = serde_json::json!({
                "id": item.id,
                "title": item.title,
                "status": item.verdict.as_str(),
                "reason": item.reason,
                "fix_command": item.fix_command,
                "fix_action": item.fix_action,
            });
            if item.id == "notifications" {
                row["push_devices"] =
                    serde_json::json!(item.push_devices.clone().unwrap_or_default());
            }
            row
        })
        .collect();
    serde_json::json!({ "schema_version": CHECKLIST_SCHEMA_VERSION, "items": rows })
}

/// The human section `phux doctor` ends with.
pub(super) fn render_human(items: &[Item], color: bool) -> String {
    use std::fmt::Write as _;

    let width = items.iter().map(|item| item.title.len()).max().unwrap_or(0);
    let mut out = String::from("\nWhat works here\n");
    for item in items {
        let severity = match item.verdict {
            Verdict::Ok => crate::output::Severity::Ok,
            Verdict::Off => crate::output::Severity::Error,
            Verdict::Waiting | Verdict::Unknown => crate::output::Severity::Warn,
        };
        let padding = 5 - severity.label().len();
        let line = crate::output::human_line(
            severity,
            format_args!("{:padding$}{:<width$} {}", "", item.title, item.reason),
            color,
        );
        let _ = writeln!(out, "{line}");
        for command in item.fix_command.iter().flat_map(|c| c.lines()) {
            let _ = writeln!(out, "  fix: {command}");
        }
    }
    out
}

#[cfg(test)]
mod tests;
