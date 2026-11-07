//! [`Activation`]: one concrete execution of a resource, and the lifecycle
//! vocabulary around it ([`ExecState`], [`Outcome`], [`ExitPolicy`],
//! [`SessionLifetime`]).
//!
//! A resource record outlives its executions. Each launch or relaunch opens
//! a new activation with the next [`ActivationId`]; the record keeps the
//! current or most recent one.

use serde::{Deserialize, Serialize};

use crate::keys::{ActivationId, InstanceId};
use crate::process::ExitOutcome;

/// Wall-clock milliseconds since the Unix epoch. Core never reads a clock;
/// callers pass the stamp in.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Stamp(pub u64);

impl Stamp {
    /// Milliseconds since the Unix epoch.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }
}

/// Where an activation is in its lifecycle.
///
/// ```text
/// Starting -> Running -> Completed
///     |          |-> Stopping -> Completed
///     |          '-> Lost -> Completed
///     '-> Failed
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecState {
    /// Launch intent committed; the executor has not reported a process yet.
    Starting,
    /// The executor holds a live handle.
    Running,
    /// Termination was requested and has not been observed yet.
    Stopping,
    /// The execution ended and its outcome is recorded.
    Completed,
    /// The execution never started.
    Failed,
    /// The owner lost its handle while the process may still be running.
    /// Not stopped: removal and relaunch are refused until proven inactive.
    Lost,
    /// A lost activation released without proof it ended. No command
    /// produces it; it stays so stored and listed states keep decoding.
    Abandoned,
}

impl ExecState {
    /// Whether the executor holds, or is acquiring, a live handle.
    #[must_use]
    pub const fn is_current(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }

    /// Short lowercase name for logs and JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Lost => "lost",
            Self::Abandoned => "abandoned",
        }
    }
}

/// How an activation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The executor observed the exit: status, signal, or neither known.
    Exit(ExitOutcome),
    /// The process ended while no owner watched it (a crash or reboot);
    /// nothing about how it ended is known.
    Unobserved,
}

impl Outcome {
    /// Whether the executor saw the end itself.
    #[must_use]
    pub const fn is_observed(self) -> bool {
        matches!(self, Self::Exit(_))
    }
}

/// A process identity that survives server restarts (R12): pid reuse is
/// detected by `start_raw`, a reboot by `boot_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcIdentity {
    /// Process id.
    pub pid: i32,
    /// Process group id; equal to `pid` for a Terminal child.
    pub pgid: i32,
    /// The platform's raw process start time, comparable across server
    /// restarts within one boot.
    pub start_raw: u64,
    /// Boot identity: Linux `boot_id`, or the macOS `kern.boottime`
    /// encoded into 16 bytes. A different boot means the pid is gone.
    pub boot_id: [u8; 16],
}

/// One concrete execution of a resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activation {
    /// Monotonic per resource, from [`ActivationId::FIRST`].
    pub id: ActivationId,
    /// The server run that started it.
    pub instance: InstanceId,
    /// Current lifecycle state.
    pub state: ExecState,
    /// When the launch intent committed.
    pub started: Stamp,
    /// When the activation left the current states, if it has.
    #[serde(default)]
    pub ended: Option<Stamp>,
    /// How it ended, once known.
    #[serde(default)]
    pub outcome: Option<Outcome>,
    /// The host process, for kinds that run one.
    #[serde(default)]
    pub host_proc: Option<ProcIdentity>,
    /// Bounded launch evidence; never contains command lines or environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<LaunchDiagnostic>,
}

impl Activation {
    /// A fresh activation in [`ExecState::Starting`].
    #[must_use]
    pub const fn starting(id: ActivationId, instance: InstanceId, now: Stamp) -> Self {
        Self {
            id,
            instance,
            state: ExecState::Starting,
            started: now,
            ended: None,
            outcome: None,
            host_proc: None,
            diagnostic: None,
        }
    }
}

/// The stage at which acquiring the requested execution failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchStage {
    /// PTY allocation or bridge setup.
    Pty,
    /// OS spawn of the launcher.
    Spawn,
    /// Entering the requested directory.
    Cwd,
    /// Executing the requested program (including its interpreter).
    Exec,
    /// Launcher status was not trustworthy before the bounded deadline.
    Handshake,
    /// Terminal engine construction.
    Engine,
}

/// Safe, fixed-size failure evidence; errno is platform-specific, not an exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchDiagnostic {
    /// Failed launch stage.
    pub stage: LaunchStage,
    /// OS errno, when available.
    pub errno: Option<i32>,
}

impl std::fmt::Display for LaunchDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "launch {:?} failed", self.stage)?;
        if let Some(errno) = self.errno {
            write!(f, ": {}", std::io::Error::from_raw_os_error(errno))?;
        }
        Ok(())
    }
}

/// What happens to a resource record when its activation completes with an
/// observed exit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitPolicy {
    /// Remove the record in the same transaction (an interactive shell).
    #[default]
    Remove,
    /// Keep the record with its outcome (a launched command or agent).
    Retain,
}

/// Whether a session survives having no members.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifetime {
    /// Survives empty; the default for named sessions.
    #[default]
    Persistent,
    /// Deleted in the same transaction that removes its last member.
    Ephemeral,
}

impl SessionLifetime {
    /// Short lowercase name for logs and JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Persistent => "persistent",
            Self::Ephemeral => "ephemeral",
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod compatibility_tests {
    use super::*;

    #[test]
    fn activation_without_new_diagnostic_remains_readable() {
        let old = Activation::starting(
            ActivationId::FIRST,
            InstanceId::from_bytes([1; 16]),
            Stamp(1),
        );
        let value = serde_json::to_value(&old).unwrap();
        assert!(value.get("diagnostic").is_none());
        assert_eq!(serde_json::from_value::<Activation>(value).unwrap(), old);
    }
}
