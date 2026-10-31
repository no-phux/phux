//! Actor types: extracted from the shared module.
use super::{CancellationToken, CommandBuilder, ResourceHandle, TerminalActor, oneshot};

/// Errors surfaced while constructing a [`TerminalActor`].
#[derive(Debug, thiserror::Error)]
pub enum TerminalActorError {
    /// Libghostty refused to allocate a Terminal or input encoder.
    #[error("libghostty allocation failed: {0}")]
    Terminal(#[from] libghostty_vt::Error),
    /// Failed to allocate the [`SnapshotSynthesizer`](crate::grid::SnapshotSynthesizer).
    #[error("SnapshotSynthesizer::new failed: {0}")]
    Synth(#[from] crate::grid::SynthesisError),
    /// Could not open a PTY pair via `portable_pty`.
    #[error("openpty failed: {0}")]
    OpenPty(String),
    /// Could not spawn the command on the PTY slave. The text is the whole
    /// wire refusal reason; the client already prefixes "spawn failed".
    #[error("{0}")]
    Spawn(String),
    /// Structured, payload-free requested-launch evidence.
    #[error("{0}")]
    Launch(phux_core::LaunchDiagnostic),
    /// Could not take the master halves or start the bridge threads.
    #[error("pty io setup failed: {0}")]
    PtyIo(String),
    /// The organization refused the Terminal's record (a taken session
    /// name, a store failure), so nothing was spawned.
    #[error("{0}")]
    Refused(String),
}

impl TerminalActorError {
    /// Safe diagnostic retained on the activation, without error text payloads.
    #[must_use]
    pub const fn launch_diagnostic(&self) -> phux_core::LaunchDiagnostic {
        use phux_core::{LaunchDiagnostic, LaunchStage};
        let stage = match self {
            Self::Launch(diagnostic) => return *diagnostic,
            Self::OpenPty(_) | Self::PtyIo(_) => LaunchStage::Pty,
            Self::Spawn(_) | Self::Refused(_) => LaunchStage::Spawn,
            Self::Terminal(_) | Self::Synth(_) => LaunchStage::Engine,
        };
        LaunchDiagnostic { stage, errno: None }
    }
}

/// The actor plus its [`CancellationToken`]. Cancellation is explicit:
/// dropping the token does not stop the actor.
#[must_use]
pub struct TerminalActorBundle {
    /// The actor; pass to `tokio::task::spawn_local`.
    pub actor: TerminalActor,
    /// Cross-task handle: generic resource channels plus the Terminal facet.
    pub handle: ResourceHandle,
    /// Cancel to shut the actor down.
    pub token: CancellationToken,
    /// Fires with the child's [`ExitOutcome`](phux_core::process::ExitOutcome)
    /// at PTY EOF; the runtime's EOF watcher drives client detach from it.
    /// `Option` so it can be taken once.
    pub exit_notify: Option<oneshot::Receiver<phux_core::process::ExitOutcome>>,
    /// The child process's identity (R12), recorded on its activation so a
    /// later start can tell a surviving orphan from a reused pid. `None`
    /// without a PTY child or when its start time cannot be read.
    pub proc: Option<phux_core::ProcIdentity>,
}

impl std::fmt::Debug for TerminalActorBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalActorBundle")
            .field("actor", &self.actor)
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

/// Where a [`TerminalActor`]'s backing PTY comes from.
pub(super) enum PtySource {
    /// No PTY (test / projection-only actors).
    None,
    /// Open a fresh PTY and spawn `cmd` on the slave.
    Spawn(CommandBuilder),
    /// Re-adopt a PTY master fd and child pid after a graceful-upgrade exec
    /// (ADR-0032).
    Adopt {
        /// Inherited master descriptor (`FD_CLOEXEC` cleared before the exec).
        master_fd: std::os::fd::RawFd,
        /// Surviving child PID on the slave side.
        child_pid: i32,
    },
}
