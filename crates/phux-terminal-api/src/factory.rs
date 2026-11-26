//! Explicit application composition; there is no built-in/default engine.

use crate::agent_detect::{AgentDetectEvent, LiveSessionProbe, ProgramNote, ProgramStatusRecord};
use crate::launch::LaunchCommand;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum TerminalActorError {
    #[error("{0}")]
    Engine(String),
    #[error("{0}")]
    Refused(String),
    /// An engine that failed to start, with the payload-free stage it failed
    /// at, so the activation keeps that evidence across this boundary.
    #[error("{message}")]
    Launch {
        message: String,
        diagnostic: phux_core::LaunchDiagnostic,
    },
}

impl TerminalActorError {
    /// Safe diagnostic retained on the activation, without error text payloads.
    #[must_use]
    pub const fn launch_diagnostic(&self) -> phux_core::LaunchDiagnostic {
        use phux_core::{LaunchDiagnostic, LaunchStage};
        let stage = match self {
            Self::Launch { diagnostic, .. } => return *diagnostic,
            Self::Refused(_) => LaunchStage::Spawn,
            Self::Engine(_) => LaunchStage::Engine,
        };
        LaunchDiagnostic { stage, errno: None }
    }
}

/// Most bytes one saved screen may take (`[restore] scrollback`).
pub const KEPT_SCROLLBACK_MAX_BYTES: usize = 8 * 1024 * 1024;

/// `[restore] scrollback`: what the host keeps of a Terminal's primary
/// screen and history. The alternate screen is never reported.
#[derive(Clone)]
pub struct KeepScrollback {
    /// History rows to report beside the screen.
    pub lines: u32,
    /// Takes the styled text, at most [`KEPT_SCROLLBACK_MAX_BYTES`], once
    /// output settles and when the engine stops.
    pub save: std::rc::Rc<dyn Fn(Vec<u8>)>,
}
impl std::fmt::Debug for KeepScrollback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeepScrollback")
            .field("lines", &self.lines)
            .finish_non_exhaustive()
    }
}

/// Host-provided report bounds and observation. No host state escapes here.
pub struct EngineReports {
    pub event_capacity: usize,
    pub agent_capacity: usize,
    pub live_session: Option<LiveSessionProbe>,
    pub keep_scrollback: Option<KeepScrollback>,
    /// `OSC7577_TOKEN` for this pane. Absent when program resume is off.
    /// Never printed: [`Debug`] omits every field.
    pub program_resume_token: Option<String>,
    /// Last accepted `(pid, time)`, carried across an upgrade with the record.
    pub program_resume_clock: Option<(u32, u64)>,
}
impl std::fmt::Debug for EngineReports {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineReports").finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum EngineSource {
    /// `seed` is written to the grid before the command's first output, and
    /// never reaches its PTY.
    Fresh {
        command: Option<LaunchCommand>,
        seed: Vec<u8>,
    },
    /// Descriptor ownership follows the existing graceful-upgrade contract.
    Adopt {
        master_fd: std::os::fd::RawFd,
        child_pid: i32,
        seed: Vec<u8>,
    },
    Replay(Vec<u8>),
}

#[derive(Debug)]
pub struct EngineBuild {
    pub cols: u16,
    pub rows: u16,
    pub source: EngineSource,
    pub scrollback: phux_config_core::limits::ScrollbackLimits,
    pub token: CancellationToken,
    pub default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    pub reports: EngineReports,
}

pub struct EngineBundle {
    pub handle: phux_resource::ResourceHandle,
    pub token: CancellationToken,
    pub run: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>,
    pub exit_notify: Option<oneshot::Receiver<phux_core::ExitOutcome>>,
    pub proc: Option<phux_core::ProcIdentity>,
    pub events: phux_resource::event_sink::EventSource,
    pub agent_state: Option<mpsc::Receiver<AgentDetectEvent>>,
    pub program_status: watch::Receiver<Vec<ProgramStatusRecord>>,
    /// The OSC 7501 `resume=` token: server-only, so not in a record.
    pub resume: watch::Receiver<Option<phux_core::ResumeToken>>,
    /// OSC 7577 record changes. The token is not one of them.
    pub program_resume: tokio::sync::mpsc::UnboundedReceiver<phux_core::ProgramResumeUpdate>,
    /// The latest OSC 9 / OSC 777 notification; the newest replaces any
    /// the server has not read yet.
    pub program_note: watch::Receiver<Option<ProgramNote>>,
}
impl std::fmt::Debug for EngineBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineBundle")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

pub trait TerminalEngineFactory: std::fmt::Debug + Send + Sync {
    fn build(&self, request: EngineBuild) -> Result<EngineBundle, TerminalActorError>;
    fn bootstrap_capabilities(&self) -> phux_protocol::caps::BootstrapCapabilities;
    fn metrics(&self) -> &'static [phux_perf::Metric];
    fn host_metrics(&self) -> Option<HostMetrics>;
    fn presentation(&self) -> Presentation;
}

#[derive(Debug, Clone, Copy)]
pub struct HostMetrics {
    pub input_credit_waits: &'static phux_perf::Counter,
    pub input_credit_wait: &'static phux_perf::Histogram,
    pub input_credit_timeouts: &'static phux_perf::Counter,
    pub agent_publish: &'static phux_perf::Histogram,
}

/// Engine-owned capability adaptation, usable on any host thread.
#[derive(Debug, Clone, Copy)]
pub struct Presentation {
    pub adapt_bytes: fn(&bytes::Bytes, phux_protocol::ClientCapabilities) -> bytes::Bytes,
    pub source_ceiling: fn(usize, phux_protocol::ClientCapabilities) -> usize,
    pub adapt_snapshot: fn(
        crate::grid::SnapshotBytes,
        phux_protocol::ClientCapabilities,
        usize,
    ) -> Result<AdaptedBootstrap, ()>,
}
#[cfg(any(test, feature = "test-support"))]
impl Presentation {
    #[must_use]
    pub const fn detached() -> Self {
        Self {
            adapt_bytes: |bytes, _| bytes.clone(),
            source_ceiling: |_, _| 0,
            adapt_snapshot: |_, _, _| Err(()),
        }
    }
}

#[derive(Debug)]
pub struct AdaptedBootstrap {
    pub payloads: Vec<bytes::Bytes>,
    pub retained_bytes: usize,
    pub peak_bytes: usize,
}

#[cfg(test)]
mod tests {
    use phux_core::{LaunchDiagnostic, LaunchStage};

    use super::TerminalActorError;

    /// The stage an engine failed at survives the trip to the host; errors
    /// without one fall back to the stage their variant names.
    #[test]
    fn a_launch_failure_keeps_its_stage_across_the_engine_boundary() {
        let exec = LaunchDiagnostic {
            stage: LaunchStage::Exec,
            errno: Some(2),
        };
        let launch = TerminalActorError::Launch {
            message: "exec failed".to_owned(),
            diagnostic: exec,
        };
        assert_eq!(launch.launch_diagnostic(), exec);
        assert_eq!(launch.to_string(), "exec failed");
        assert_eq!(
            TerminalActorError::Refused("taken".into())
                .launch_diagnostic()
                .stage,
            LaunchStage::Spawn
        );
        assert_eq!(
            TerminalActorError::Engine("libghostty".into())
                .launch_diagnostic()
                .stage,
            LaunchStage::Engine
        );
    }
}
