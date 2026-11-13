//! Keep the machine awake while agents work (`defaults.keep-awake`).
//!
//! A ticker on the server's `LocalSet` decides every 15 seconds whether an
//! idle-sleep inhibitor should be held, and starts or stops it. The inhibitor
//! is a child process that watches the server's pid, so it exits by itself
//! if the server dies: `caffeinate` on macOS, `systemd-inhibit` on Linux,
//! nothing elsewhere. The outcome is published at [`STATUS_KEY`] for
//! `phux status`.

use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use phux_config::KeepAwake;
use phux_protocol::wire::frame::{RESOURCE_AGENT_KEY, Scope};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::state::SharedState;

/// How often the ticker re-decides.
const TICK: Duration = Duration::from_secs(15);

/// Global metadata key of the keep-awake state, published while the setting
/// is not `off`.
///
/// The value is `{"state": "on" | "idle" | "unavailable", "working": N}`;
/// `unavailable` means the inhibitor is wanted but could not be held.
pub const STATUS_KEY: &str = "phux.keep-awake/v1";

/// The running inhibitor. Process-wide so [`release`] can stop it from the
/// upgrade path.
static INHIBITOR: Mutex<Option<Child>> = Mutex::new(None);

fn inhibitor() -> MutexGuard<'static, Option<Child>> {
    INHIBITOR.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the inhibitor should be held with `working` agents at work.
#[must_use]
pub const fn wanted(mode: KeepAwake, working: usize) -> bool {
    match mode {
        KeepAwake::Off => false,
        KeepAwake::WhileAgentsWork => working > 0,
        KeepAwake::Always => true,
    }
}

/// The inhibitor for this platform, exiting when `pid` does; `None` where
/// there is none.
fn command(pid: u32) -> Option<Command> {
    let mut command;
    if cfg!(target_os = "macos") {
        command = Command::new("caffeinate");
        command.args(["-i", "-w", &pid.to_string()]);
    } else if cfg!(target_os = "linux") {
        command = Command::new("systemd-inhibit");
        command.args(["--what=idle", "--who=phux", "--why=an agent is working"]);
        command.args(["tail", &format!("--pid={pid}"), "-f", "/dev/null"]);
    } else {
        return None;
    }
    // Its own group, so stopping it takes `systemd-inhibit`'s `tail` too.
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Some(command)
}

/// Stop the inhibitor, if one runs, and reap it.
pub(crate) fn release() {
    stop(&mut inhibitor());
}

fn stop(slot: &mut Option<Child>) {
    let Some(mut child) = slot.take() else {
        return;
    };
    if let Ok(pgid) = i32::try_from(child.id()) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pgid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let _ = child.wait();
    info!("keep-awake: idle sleep allowed again");
}

/// Start or stop the inhibitor to match `want`; whether it is held now.
/// A failure is logged once and sets `failed`, which ends the retries.
fn hold(want: bool, failed: &mut bool) -> bool {
    let mut slot = inhibitor();
    if !want {
        stop(&mut slot);
        return false;
    }
    if let Some(child) = slot.as_mut()
        && !matches!(child.try_wait(), Ok(None))
    {
        warn!("keep-awake: the sleep inhibitor exited by itself; not restarting it");
        *slot = None;
        *failed = true;
    }
    if slot.is_none()
        && !*failed
        && let Some(mut command) = command(std::process::id())
    {
        match command.spawn() {
            Ok(child) => {
                info!(pid = child.id(), "keep-awake: holding off idle sleep");
                *slot = Some(child);
            }
            Err(err) => {
                warn!(error = %err, program = ?command.get_program(), "keep-awake: could not start the sleep inhibitor");
                *failed = true;
            }
        }
    }
    slot.is_some()
}

/// Start the ticker on the current `LocalSet`; with the setting `off` (or
/// on a platform with no inhibitor) only clear a state an upgrade carried.
pub(crate) fn spawn(state: &SharedState, root_token: &CancellationToken) {
    let mode = state.with(crate::state::ServerState::keep_awake);
    if mode == KeepAwake::Off || command(0).is_none() {
        state.with_mut(|s| drop(s.metadata_delete(&Scope::Global, STATUS_KEY)));
        return;
    }
    tokio::task::spawn_local(ticker(state.clone(), mode, root_token.clone()));
}

// ponytail: polled, so the inhibitor lags an agent's state by up to one
// TICK. The agent record has many writers and deleters; hook them
// (`ServerState::metadata_set_by` / `metadata_delete_by`) if that lag ever
// matters.
async fn ticker(state: SharedState, mode: KeepAwake, token: CancellationToken) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut failed = false;
    loop {
        tokio::select! {
            () = token.cancelled() => break,
            _ = tick.tick() => {}
        }
        let working = state.with(|s| {
            s.metadata()
                .resource_values(RESOURCE_AGENT_KEY)
                .filter(|value| {
                    phux_agent_session::state::stored_state(Some(value)).as_deref()
                        == Some("working")
                })
                .count()
        });
        let want = wanted(mode, working);
        let label = match (hold(want, &mut failed), want) {
            (true, _) => "on",
            (false, true) => "unavailable",
            (false, false) => "idle",
        };
        let record = serde_json::json!({ "state": label, "working": working }).to_string();
        state.with_mut(|s| drop(s.metadata_set(&Scope::Global, STATUS_KEY, record.into_bytes())));
    }
    release();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inhibitor_follows_the_mode_and_the_working_agents() {
        for working in [0, 1, 5] {
            assert!(!wanted(KeepAwake::Off, working));
            assert!(wanted(KeepAwake::Always, working));
            assert_eq!(wanted(KeepAwake::WhileAgentsWork, working), working > 0);
        }
    }
}
