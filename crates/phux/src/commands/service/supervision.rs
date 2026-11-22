//! Whether the init system keeps a server alive across logout and reboot,
//! for `phux doctor`'s checklist.

use std::path::Path;

use super::{Manager, armed_adoption_unit, profile_suffix, unit_is_running, unit_supervises};

/// Whether the init system keeps the server on `socket_path` running across
/// logout and reboot, for `phux doctor`'s checklist. Read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Supervision {
    /// No unit generator for this platform.
    Unsupported,
    /// No unit supervises this socket.
    Unsupervised,
    /// Installed with `--adopt`, waiting for the running server to exit.
    Armed,
    /// Installed, but the init system is not running it.
    Stopped,
    /// Running under the init system. `linger` is false only for a systemd
    /// user unit without lingering, which stops at logout.
    Running {
        /// `launchd` or `systemd`.
        manager: &'static str,
        /// Whether it outlives the login session.
        linger: bool,
    },
}

/// See [`Supervision`].
pub(crate) fn supervision(socket_path: &Path) -> Supervision {
    let Some(manager) = Manager::host() else {
        return Supervision::Unsupported;
    };
    if armed_adoption_unit(socket_path).is_some() {
        return Supervision::Armed;
    }
    let supervises = manager
        .unit_path(profile_suffix().as_deref())
        .and_then(|unit| std::fs::read_to_string(unit).map_err(|err| err.to_string()))
        .is_ok_and(|body| unit_supervises(manager, &body, socket_path));
    if !supervises {
        return Supervision::Unsupervised;
    }
    if !unit_is_running(manager) {
        return Supervision::Stopped;
    }
    match manager {
        Manager::Launchd => Supervision::Running {
            manager: "launchd",
            linger: true,
        },
        Manager::Systemd => Supervision::Running {
            manager: "systemd",
            linger: super::probe_linger(),
        },
    }
}
