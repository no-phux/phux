//! `--adopt` (ADR-0088): the marker an armed unit leaves, what it says
//! about supervision, and the hand-over the auto-spawn path completes.

use std::path::{Path, PathBuf};

use super::{
    Manager, launchd_target, profile_suffix, start_armed_unit, systemd_unit, unit_socket_override,
};

/// Where an armed-but-unloaded unit is recorded (profile-scoped). A file,
/// because an armed unit and a deliberately stopped one look identical on
/// disk and to the init system.
pub(super) fn adoption_marker_path() -> PathBuf {
    phux_server::telemetry::state_dir().join("service-adopt-pending")
}

/// Record that `unit_path` is armed and waiting for the incumbent to exit.
pub(super) fn mark_adoption_pending(unit_path: &Path) -> Result<(), String> {
    let marker = adoption_marker_path();
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    std::fs::write(&marker, format!("{}\n", unit_path.display())).map_err(|err| {
        format!(
            "unit armed, but {} could not be written ({err}), so the hand-over will not happen \
             on its own",
            marker.display()
        )
    })
}

/// Forget a pending adoption (best-effort).
pub(super) fn clear_adoption_pending() {
    let _ = std::fs::remove_file(adoption_marker_path());
}

/// The user-facing explanation of an armed supervision unit, shared by
/// `phux service status` and `phux doctor`. Prose without newlines, so each
/// caller wraps it.
pub(crate) const ARMED_SUPERVISION_EXPLANATION: &str = "the running server keeps its panes and stays unsupervised, so a crash before the \
     hand-over is not caught by anything; supervision begins at the next login, or at the \
     first `phux` command after that server exits, and `phux service uninstall` cancels it";

/// Whose supervision a [`supervision_state`] question is about. A unit armed
/// for another profile or `--socket` must not affect this server.
#[derive(Clone, Copy)]
pub(super) enum Subject<'a> {
    /// The instance that would bind `socket_path`.
    Server(&'a Path),
    /// This profile's unit, whatever socket it was installed against.
    Unit,
}

/// What an adoption marker says about supervision (not whether the unit runs).
pub(super) enum SupervisionState {
    /// No pending adoption for this subject.
    NotArmed,
    /// The marker's unit is gone; owners of the state sweep it.
    MarkerWithoutUnit,
    /// Armed: written and deliberately unloaded, waiting for the incumbent to exit.
    Armed { manager: Manager, unit: PathBuf },
}

/// The single predicate behind every "is supervision armed?" question: a
/// marker exists, its unit is readable, and (for a server subject) the unit's
/// socket is that server's socket. Never sweeps; callers that own the state
/// act on [`SupervisionState::MarkerWithoutUnit`].
pub(super) fn supervision_state(subject: Subject<'_>) -> SupervisionState {
    if !adoption_marker_path().exists() {
        return SupervisionState::NotArmed;
    }
    let Some(manager) = Manager::host() else {
        return SupervisionState::NotArmed;
    };
    let Ok(unit) = manager.unit_path(profile_suffix().as_deref()) else {
        return SupervisionState::NotArmed;
    };
    let Ok(body) = std::fs::read_to_string(&unit) else {
        return SupervisionState::MarkerWithoutUnit;
    };
    match subject {
        Subject::Unit => SupervisionState::Armed { manager, unit },
        Subject::Server(socket_path) if unit_supervises(manager, &body, socket_path) => {
            SupervisionState::Armed { manager, unit }
        }
        Subject::Server(_) => SupervisionState::NotArmed,
    }
}

/// The unit an armed adoption is recorded against for the server on
/// `socket_path`, for `phux doctor`. Read-only: a marker whose unit the init
/// system already runs (it started at boot) is spent, not armed, though nothing
/// has swept it yet.
pub(crate) fn armed_adoption_unit(socket_path: &Path) -> Option<PathBuf> {
    match supervision_state(Subject::Server(socket_path)) {
        SupervisionState::Armed { manager, unit } if !unit_is_running(manager) => Some(unit),
        SupervisionState::Armed { .. }
        | SupervisionState::NotArmed
        | SupervisionState::MarkerWithoutUnit => None,
    }
}

/// Does `body` describe a unit whose server would bind `socket_path`?
pub(super) fn unit_supervises(manager: Manager, body: &str, socket_path: &Path) -> bool {
    unit_socket_override(manager, body).unwrap_or_else(phux_server::runtime::default_socket_path)
        == socket_path
}

/// Whether a pending-adoption record is spent: its unit vanished, or the init
/// system is already running it.
pub(super) const fn adoption_marker_is_spent(state: &SupervisionState, unit_running: bool) -> bool {
    match state {
        SupervisionState::MarkerWithoutUnit => true,
        SupervisionState::Armed { .. } => unit_running,
        SupervisionState::NotArmed => false,
    }
}

/// Ask the init system whether it is running this profile's unit.
pub(super) fn unit_is_running(manager: Manager) -> bool {
    probe_unit(manager).is_ok_and(|output| output.status.success())
}

/// The captured init-system probe `phux service status` already ran.
pub(super) fn probe_unit(manager: Manager) -> std::io::Result<std::process::Output> {
    match manager {
        Manager::Launchd => std::process::Command::new("launchctl")
            .args(["print", &launchd_target()])
            .output(),
        Manager::Systemd => std::process::Command::new("systemctl")
            .args(["--user", "status", &systemd_unit()])
            .output(),
    }
}

/// Sweep an adoption marker that can no longer be pending for `socket_path`
/// (login bootstrap starts the unit without going through
/// [`complete_pending_adoption`]).
pub(crate) fn sweep_stale_adoption_marker(socket_path: &Path) {
    let state = supervision_state(Subject::Server(socket_path));
    let running = match state {
        SupervisionState::Armed { manager, .. } => unit_is_running(manager),
        SupervisionState::MarkerWithoutUnit | SupervisionState::NotArmed => false,
    };
    if adoption_marker_is_spent(&state, running) {
        clear_adoption_pending();
    }
}

/// Outcome of trying to complete an armed adoption from the auto-spawn path.
pub(crate) enum Handover {
    /// The init system accepted the start; the caller must wait for the socket
    /// rather than spawn a competing server.
    Started,
    /// Nothing was started; the caller auto-spawns as usual.
    NotTaken,
}

/// Complete an armed adoption if one is pending for `socket_path`, called
/// from the auto-spawn path once the incumbent has exited. Only a pending
/// adoption diverts, and only once, so a deliberately stopped server stays
/// stopped (ADR-0080). `quiet` honors the `--json` stderr contract.
pub(crate) fn complete_pending_adoption(socket_path: &Path, quiet: bool) -> Handover {
    let (manager, unit_path) = match supervision_state(Subject::Server(socket_path)) {
        SupervisionState::Armed { manager, unit } => (manager, unit),
        SupervisionState::MarkerWithoutUnit => {
            clear_adoption_pending();
            return Handover::NotTaken;
        }
        SupervisionState::NotArmed => return Handover::NotTaken,
    };
    match start_armed_unit(manager, &unit_path) {
        Ok(()) => {
            // Cleared once the start request succeeds; a supervised server that then
            // fails is a crash-loop for `phux doctor`, not something to retrigger.
            clear_adoption_pending();
            if !quiet {
                info!(
                    "handing this server over to {} — the unit armed by `phux service \
                     install --adopt` is now live",
                    match manager {
                        Manager::Launchd => "launchd",
                        Manager::Systemd => "systemd",
                    }
                );
            }
            Handover::Started
        }
        Err(err) => {
            // Fall through to an ordinary spawn; the marker stays for the next try.
            if !quiet {
                warn!("could not start the armed service unit ({err}); starting a server");
            }
            Handover::NotTaken
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Manager, SupervisionState, adoption_marker_is_spent};

    /// The live-server path sweeps spent records and leaves a pending adopt alone.
    #[test]
    fn a_spent_adoption_marker_is_the_live_path_sweep() {
        let armed = || SupervisionState::Armed {
            manager: Manager::Systemd,
            unit: PathBuf::from("/tmp/phux-test.service"),
        };
        assert!(
            adoption_marker_is_spent(&SupervisionState::MarkerWithoutUnit, false),
            "a marker whose unit has vanished can never complete"
        );
        assert!(
            adoption_marker_is_spent(&armed(), true),
            "armed + running is the completed login hand-over"
        );
        assert!(
            !adoption_marker_is_spent(&armed(), false),
            "armed + not-running is still waiting for the incumbent"
        );
        assert!(
            !adoption_marker_is_spent(&SupervisionState::NotArmed, true),
            "no marker means there is nothing to sweep, even if a unit is running"
        );
    }
}
