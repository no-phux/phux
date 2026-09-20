//! The engine owner thread (ADR-0133 decision 4).
//!
//! `SessionKernel` and every engine replica live on one dedicated thread.
//! Nothing that is not `Send` ever leaves it: the control plane hands it
//! owned [`EngineEvent`]s over a channel and gets owned [`EngineOutcome`]s
//! back, and the grid it projects reaches consumers only through the
//! `Publication` table. With the `engine` feature the adapter is
//! libghostty's `GhosttyAdapter` and every damaged terminal is re-projected
//! and published before the outcome is answered; without it a bounded
//! `ByteAdapter` keeps the same kernel driving a byte buffer per
//! terminal, so the transport and control-plane lanes build and test with
//! no Zig toolchain (phux-mobile's headless lane).

#[cfg(feature = "engine")]
use std::collections::HashMap;
use std::collections::HashSet;
#[cfg(feature = "engine")]
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};

#[cfg(feature = "engine")]
use libghostty_vt::terminal::ScrollViewport;
#[cfg(feature = "engine")]
use phux_client_core::engine::ghostty::{GhosttyAdapter, GhosttyReplica};
use phux_client_core::engine::{CanonicalGeometry, EngineAdapter};
#[cfg(feature = "engine")]
use phux_client_core::grid::GridProjector;
use phux_client_core::history::HistoryCacheConfig;
#[cfg(feature = "engine")]
use phux_client_core::session::ClosedReplica;
use phux_client_core::session::{
    AgentSessionDeclaration, EffectBuffer, HistoryRejectionReason, HistoryUnavailableReason,
    InputBlockReason, InputEligibility, KernelDamageKind, KernelEffect, KernelInput, KernelStatus,
    SessionKernel,
};
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, BootstrapStreamProfile};
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::{AgentEvent, CloseReason, TombstoneReason};

#[cfg(feature = "engine")]
use crate::publication::{FrameColors, GridBuffer, GridFrame, Publication, Rgb, Scrollbar};

#[cfg(feature = "engine")]
type Adapter = GhosttyAdapter;
#[cfg(not(feature = "engine"))]
type Adapter = byte_adapter::ByteAdapter;

mod apply;
/// The headless replica: a bounded byte buffer per terminal.
#[cfg(not(feature = "engine"))]
pub mod byte_adapter;
mod owner;

use owner::Owner;

/// One owned kernel input, as the control plane decodes it from a frame.
#[derive(Debug, Clone)]
#[allow(
    missing_docs,
    reason = "each field mirrors the `KernelInput` field of the same name"
)]
pub enum EngineEvent {
    /// `ATTACHED`: the aggregate attach and its terminal inventory.
    AttachStarted {
        attach_id: u32,
        terminals: Vec<ResourceId>,
    },
    /// `ATTACH_READY`.
    AttachReady { attach_id: u32 },
    /// `BOOTSTRAP_BEGIN`.
    BootstrapBegin {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        profile: BootstrapStreamProfile,
        cols: u16,
        rows: u16,
        base_seq: u64,
    },
    /// `BOOTSTRAP_CHUNK`.
    BootstrapChunk {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        chunk_seq: u32,
        payload: Vec<u8>,
    },
    /// `BOOTSTRAP_READY`.
    BootstrapReady {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        history_cursor: Option<Vec<u8>>,
    },
    /// `HISTORY_PAGE`.
    HistoryPage {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        page_seq: u64,
        rows: u32,
        cursor: Vec<u8>,
        next_cursor: Option<Vec<u8>>,
        payload: Vec<u8>,
    },
    /// `HISTORY_TOMBSTONE`.
    HistoryTombstone {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        cursor: Vec<u8>,
        reason: HistoryUnavailableReason,
    },
    /// `HISTORY_REJECTED`.
    HistoryRejected {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        cursor: Vec<u8>,
        reason: HistoryRejectionReason,
        required_bytes: u32,
        required_rows: u32,
    },
    /// `RESOURCE_OUTPUT`.
    Output {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        seq: u64,
        bytes: Vec<u8>,
    },
    /// `BOOTSTRAP_TOMBSTONE`.
    Tombstone {
        terminal_id: ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        reason: TombstoneReason,
        last_valid_seq: u64,
    },
    /// `RESOURCE_CLOSED`, or a close inferred from a topology that no
    /// longer lists the terminal.
    Closed {
        terminal_id: ResourceId,
        exit_status: Option<i32>,
        signal: Option<i32>,
        reason: CloseReason,
    },
    /// One subscribed `EVENT` scoped to a terminal.
    Agent {
        terminal_id: ResourceId,
        event: AgentEvent,
    },
    /// An `AgentSession` resource the attach snapshot declared.
    AgentSessionDeclared {
        terminal_id: ResourceId,
        parent: Option<ResourceId>,
        provider: Option<String>,
        native_id: Option<String>,
        state: Option<String>,
    },
}

impl EngineEvent {
    /// A close the control plane inferred (a pane missing from a fresh
    /// topology) rather than received.
    #[must_use]
    pub const fn closed_unknown(terminal_id: ResourceId) -> Self {
        Self::Closed {
            terminal_id,
            exit_status: None,
            signal: None,
            reason: CloseReason::Unknown,
        }
    }

    /// The terminal the event concerns, if it is terminal-scoped.
    #[must_use]
    pub const fn terminal_id(&self) -> Option<&ResourceId> {
        match self {
            Self::AttachStarted { .. } | Self::AttachReady { .. } => None,
            Self::BootstrapBegin { terminal_id, .. }
            | Self::BootstrapChunk { terminal_id, .. }
            | Self::BootstrapReady { terminal_id, .. }
            | Self::HistoryPage { terminal_id, .. }
            | Self::HistoryTombstone { terminal_id, .. }
            | Self::HistoryRejected { terminal_id, .. }
            | Self::Output { terminal_id, .. }
            | Self::Tombstone { terminal_id, .. }
            | Self::Closed { terminal_id, .. }
            | Self::Agent { terminal_id, .. }
            | Self::AgentSessionDeclared { terminal_id, .. } => Some(terminal_id),
        }
    }
}

/// What one applied event produced: the kernel's declarative effects, in
/// order, and the update error if the kernel refused.
///
/// Effects are handed back even when the update failed: a codec error can
/// require an acknowledgement and a resync in the same outcome, and
/// dropping them would break the kernel contract.
#[derive(Debug, Default)]
pub struct EngineOutcome {
    /// The kernel effects this event produced.
    pub effects: Vec<KernelEffect>,
    /// The kernel's refusal, if any.
    pub error: Option<String>,
}

impl EngineOutcome {
    /// Whether an effect asked for a fresh bootstrap.
    #[must_use]
    pub fn resync_required(&self) -> bool {
        self.effects.iter().any(|effect| {
            matches!(
                effect,
                KernelEffect::Status(KernelStatus::ResyncRequired { .. })
            )
        })
    }
}

/// A viewport scroll a consumer asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// The top of the scrollback.
    Top,
    /// The live tail.
    Bottom,
    /// Relative rows; negative scrolls toward the top.
    Delta(i64),
    /// An absolute row offset from the top of the scrollable area.
    Row(u64),
}

/// Why an engine request failed.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The owner thread is gone; the runtime is unusable.
    #[error("the engine owner thread stopped")]
    Stopped,
    /// The owner thread could not be spawned.
    #[error("could not start the engine owner thread: {0}")]
    Spawn(std::io::Error),
    /// The engine refused a render or a viewport operation.
    #[error("engine: {0}")]
    Engine(String),
}

/// What the owner thread needs to build its kernel.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// The bootstrap profile `HELLO_OK` selected.
    pub profile: BootstrapProfile,
    /// The payload limits `HELLO_OK` selected.
    pub limits: BootstrapLimits,
    /// The scrollback depth the attach requested; bounds the history cache.
    pub scrollback_lines: u32,
}

impl EngineConfig {
    fn history(&self) -> HistoryCacheConfig {
        HistoryCacheConfig {
            max_materialized_rows: self.scrollback_lines as usize,
            request_max_bytes: self.limits.max_history_page_bytes(),
            request_max_rows: self.scrollback_lines,
            ..HistoryCacheConfig::default()
        }
    }
}

enum Command {
    Apply(EngineEvent, Sender<EngineOutcome>),
    Lifecycle(Lifecycle),
    Query(Query),
}

enum Lifecycle {
    Detach(ResourceId),
    #[cfg(feature = "engine")]
    Retain(ResourceId, bool),
    #[cfg(feature = "engine")]
    Release(ResourceId),
}

enum Query {
    HasProjection(ResourceId, Sender<bool>),
    IsClosed(ResourceId, Sender<bool>),
    InputReady(ResourceId, Sender<bool>),
    #[cfg(feature = "engine")]
    Scroll(ResourceId, Scroll, Sender<Result<(), EngineError>>),
    #[cfg(feature = "engine")]
    IsAltScreen(ResourceId, Sender<bool>),
    #[cfg(feature = "engine")]
    Republish(ResourceId, Sender<Result<bool, EngineError>>),
    #[cfg(not(feature = "engine"))]
    TakeOutput(ResourceId, Sender<Vec<u8>>),
}

/// The thread-safe handle on the owner thread. Cloning it shares the
/// thread; dropping the last clone stops it.
#[derive(Clone, Debug)]
pub struct EngineHandle {
    commands: Sender<Command>,
}

impl EngineHandle {
    /// Spawn the owner thread with a kernel built from `config`; with the
    /// `engine` feature it publishes every render into `publication`.
    pub(crate) fn start(
        config: &EngineConfig,
        #[cfg(feature = "engine")] publication: Arc<Publication>,
    ) -> Result<Self, EngineError> {
        let (commands, receiver) = mpsc::channel();
        let history = config.history();
        let profile = config.profile;
        #[cfg(feature = "engine")]
        let limits = config.limits;
        std::thread::Builder::new()
            .name("phux-engine-owner".to_owned())
            .spawn(move || {
                #[cfg(feature = "engine")]
                let adapter = GhosttyAdapter::new(limits);
                #[cfg(not(feature = "engine"))]
                let adapter = byte_adapter::ByteAdapter;
                let kernel = SessionKernel::with_history_config(adapter, profile, history);
                Owner::new(
                    kernel,
                    #[cfg(feature = "engine")]
                    publication,
                )
                .run(&receiver);
            })
            .map_err(EngineError::Spawn)?;
        Ok(Self { commands })
    }

    /// Apply one event and wait for its outcome.
    pub(crate) fn apply(&self, event: EngineEvent) -> Result<EngineOutcome, EngineError> {
        self.request(|reply| Command::Apply(event, reply))
    }

    /// Forget a terminal and its projection.
    pub(crate) fn detach(&self, terminal_id: ResourceId) {
        let _ = self
            .commands
            .send(Command::Lifecycle(Lifecycle::Detach(terminal_id)));
    }

    /// Whether the terminal has a live (or explicitly retained) replica.
    #[must_use]
    pub fn has_projection(&self, terminal_id: &ResourceId) -> bool {
        self.request(|reply| Command::Query(Query::HasProjection(terminal_id.clone(), reply)))
            .unwrap_or(false)
    }

    /// Whether the kernel has permanently closed the terminal.
    #[must_use]
    pub fn is_closed(&self, terminal_id: &ResourceId) -> bool {
        self.request(|reply| Command::Query(Query::IsClosed(terminal_id.clone(), reply)))
            .unwrap_or(false)
    }

    /// Whether the kernel would accept input for the terminal right now.
    #[must_use]
    pub fn input_ready(&self, terminal_id: &ResourceId) -> bool {
        self.request(|reply| Command::Query(Query::InputReady(terminal_id.clone(), reply)))
            .unwrap_or(false)
    }

    /// Keep the final replica of a terminal after it closes, so a consumer
    /// can still scroll and read it until [`Self::release`].
    #[cfg(feature = "engine")]
    pub fn set_retain_on_close(&self, terminal_id: &ResourceId, retain: bool) {
        let _ = self.commands.send(Command::Lifecycle(Lifecycle::Retain(
            terminal_id.clone(),
            retain,
        )));
    }

    /// Release a retained closed replica and its projection.
    #[cfg(feature = "engine")]
    pub fn release(&self, terminal_id: &ResourceId) {
        let _ = self
            .commands
            .send(Command::Lifecycle(Lifecycle::Release(terminal_id.clone())));
    }

    /// Scroll the terminal's viewport and publish the result before
    /// returning.
    #[cfg(feature = "engine")]
    pub fn scroll(&self, terminal_id: &ResourceId, scroll: Scroll) -> Result<(), EngineError> {
        self.request(|reply| Command::Query(Query::Scroll(terminal_id.clone(), scroll, reply)))?
    }

    /// Whether the terminal's alternate screen is active.
    #[cfg(feature = "engine")]
    #[must_use]
    pub fn is_alt_screen(&self, terminal_id: &ResourceId) -> bool {
        self.request(|reply| Command::Query(Query::IsAltScreen(terminal_id.clone(), reply)))
            .unwrap_or(false)
    }

    /// Re-project and publish the terminal now, whether or not the kernel
    /// reported damage. Returns whether a frame was published.
    #[cfg(feature = "engine")]
    pub fn republish(&self, terminal_id: &ResourceId) -> Result<bool, EngineError> {
        self.request(|reply| Command::Query(Query::Republish(terminal_id.clone(), reply)))?
    }

    /// Drain the bytes the headless replica retained since the last take.
    #[cfg(not(feature = "engine"))]
    #[must_use]
    pub fn take_output(&self, terminal_id: &ResourceId) -> Vec<u8> {
        self.request(|reply| Command::Query(Query::TakeOutput(terminal_id.clone(), reply)))
            .unwrap_or_default()
    }

    fn request<T>(&self, command: impl FnOnce(Sender<T>) -> Command) -> Result<T, EngineError> {
        let (reply, response) = mpsc::channel();
        self.commands
            .send(command(reply))
            .map_err(|_| EngineError::Stopped)?;
        response.recv().map_err(|_| EngineError::Stopped)
    }
}

#[cfg(test)]
mod tests;
