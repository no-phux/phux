//! Reloading the local hub after a registry change, and reporting it.

use super::super::service;

/// What telling the running local server about a `[[satellites]]` edit did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HubReload {
    /// The server confirmed the config-reload doorbell; a running hub has
    /// already re-read `[[satellites]]` (a non-hub server ignores it).
    Signalled,
    /// No server is running here; a hub reads the registry when it starts.
    NoServer,
    /// The doorbell could not be rung or confirmed.
    Failed(String),
}

impl HubReload {
    /// Stable token for the `hub_reload` field of the `--json` documents.
    pub(super) const fn as_json_str(&self) -> &'static str {
        match self {
            Self::Signalled => "signalled",
            Self::NoServer => "no_server",
            Self::Failed(_) => "failed",
        }
    }

    /// Whether the edit still waits for a hub restart.
    pub(super) const fn requires_restart(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    /// The human line: what a running hub does with the edit now.
    pub(super) fn describe(&self) -> String {
        match self {
            Self::Signalled => {
                "signalled the running server: a hub re-reads [[satellites]] now, no restart"
                    .to_owned()
            }
            Self::NoServer => {
                "no server running; the hub reads [[satellites]] when it starts".to_owned()
            }
            Self::Failed(reason) => format!(
                "could not signal the running server ({reason}); \
                 run `phux config reload` or restart the hub"
            ),
        }
    }
}

/// Ring the config-reload doorbell on this machine's server so a running hub
/// re-reads `[[satellites]]` now: it dials added entries, stops removed ones,
/// and leaves every other link and pane alone (phux-lpn7.2). A development
/// build never rings the production server.
pub(super) fn reload_local_hub() -> HubReload {
    use super::super::config::{ReloadRingError, ring_config_reload};

    let socket_path = phux_server::runtime::default_socket_path();
    if let Err(refusal) = phux_config::socket::refuse_dev_on_production(&socket_path) {
        return HubReload::Failed(refusal);
    }
    let Ok(rt) = super::super::cli_runtime() else {
        return HubReload::Failed("could not build a runtime".to_owned());
    };
    match rt.block_on(ring_config_reload(&socket_path)) {
        Ok(()) => HubReload::Signalled,
        Err(ReloadRingError::NoServer(phux_client::attach::AttachError::Io(err)))
            if matches!(
                err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            HubReload::NoServer
        }
        Err(ReloadRingError::NoServer(err)) => HubReload::Failed(err.to_string()),
        Err(ReloadRingError::Unconfirmed(refusal)) => HubReload::Failed(refusal),
    }
}

pub(super) fn report_local_hub(hub: Option<&service::LocalHub>, reload: Option<&HubReload>) {
    if let Some(reload) = reload {
        outln!("  {}", reload.describe());
    }
    match hub {
        Some(service::LocalHub::Already) => {
            outln!("  local hub service already runs with --hub");
        }
        Some(service::LocalHub::Patched) => {
            outln!("  local hub service: --hub added; existing listeners kept");
            outln!("  a server already running without --hub dials it after its next restart");
        }
        Some(service::LocalHub::Installed) => {
            outln!("  local hub service installed with --hub");
        }
        Some(service::LocalHub::Skipped(reason)) => {
            warn!("host add: could not enable local --hub: {reason}");
            outln!("  Hub route ready once this host runs `phux service install --hub`.");
        }
        None => {
            outln!("  Hub route ready once this host runs `phux service install --hub`.");
        }
    }
}

/// Add the `hub_reload` token to a satellite edit's `--json` document.
pub(super) fn add_hub_reload(doc: &mut serde_json::Value, reload: Option<&HubReload>) {
    if let Some(reload) = reload {
        doc["hub_reload"] = serde_json::Value::String(reload.as_json_str().to_owned());
    }
}
