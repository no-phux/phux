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
use phux_client_core::engine::{DocumentPoint, DocumentSpace, EngineDocumentSelection};
#[cfg(feature = "engine")]
use phux_client_core::grid::GridProjector;
use phux_client_core::history::HistoryCacheConfig;
#[cfg(feature = "engine")]
use phux_client_core::history::{DocumentAnchorId, HistoryStatus};
#[cfg(feature = "engine")]
use phux_client_core::session::ClosedReplica;
use phux_client_core::session::{
    AgentSessionDeclaration, EffectBuffer, HistoryRejectionReason, HistoryUnavailableReason,
    InputBlockReason, InputEligibility, KernelDamageKind, KernelEffect, KernelError, KernelInput,
    KernelStatus, SessionKernel,
};
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, BootstrapStreamProfile};
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::{AgentEvent, CloseReason, TombstoneReason};

#[cfg(feature = "engine")]
use crate::publication::{
    FrameColors, GridBuffer, GridDamage, GridFrame, Publication, Rgb, Scrollbar,
};

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

/// A kernel refusal classified by protocol semantics rather than display text.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum EngineApplyError {
    /// A stale, retired, or duplicate generation. The frame is rejected but
    /// the connection and current replica remain valid.
    #[error("{0}")]
    InvalidState(String),
    /// Any other kernel refusal means the peer violated the active session
    /// contract and the connection must be replaced.
    #[error("{0}")]
    Protocol(String),
}

impl EngineApplyError {
    fn from_kernel<E: std::fmt::Display>(error: &KernelError<E>) -> Self {
        let invalid_state = matches!(
            error,
            KernelError::GenerationMismatch { .. }
                | KernelError::RetiredGeneration { .. }
                | KernelError::DuplicateGeneration { .. }
        );
        let message = error.to_string();
        if invalid_state {
            Self::InvalidState(message)
        } else {
            Self::Protocol(message)
        }
    }
}

/// Declarative effects plus any typed refusal produced by one engine event.
///
/// Effects are handed back even when the update failed: a codec error can
/// require an acknowledgement and a resync in the same outcome, and
/// dropping them would break the kernel contract.
#[derive(Debug, Default)]
pub struct EngineOutcome {
    /// The kernel effects this event produced.
    pub effects: Vec<KernelEffect>,
    /// The kernel's typed refusal, if any.
    pub error: Option<EngineApplyError>,
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

/// One document coordinate accepted by the owner thread.
#[cfg(feature = "engine")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineDocumentPoint {
    /// `0` history, `1` viewport, `2` active screen.
    pub space: u32,
    /// Column.
    pub column: u16,
    /// Row in the selected space.
    pub row: u32,
}

/// Owned facts about one published replica.
#[cfg(feature = "engine")]
#[derive(Debug, Clone)]
pub struct ReplicaInfo {
    /// Negotiated payload profile for this generation.
    pub profile: BootstrapStreamProfile,
    /// Logical stream.
    pub stream_id: u64,
    /// Bootstrap generation.
    pub bootstrap_id: u64,
    /// Highest applied live sequence.
    pub last_seq: u64,
    /// Progressive history state, when enabled.
    pub history: Option<HistoryStatus>,
    /// Revision of the document visible to document handles.
    pub document_revision: u64,
}

/// Mouse tracking mode, matching the C ABI's stable numeric vocabulary.
#[cfg(feature = "engine")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    /// Tracking disabled.
    None,
    /// X10 press tracking.
    X10,
    /// Normal press/release tracking.
    Normal,
    /// Button-motion tracking.
    Button,
    /// Any-motion tracking.
    Any,
}

/// A search result represented by owner-thread document-anchor handles.
#[cfg(feature = "engine")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    /// Start anchor.
    pub start: u64,
    /// End anchor.
    pub end: u64,
}

/// Provider-owned pointer gesture input. Positions and geometry share units.
#[cfg(feature = "engine")]
#[derive(Debug, Clone, Copy)]
pub struct SelectionGestureEvent {
    /// 0 press, 1 drag, 2 release.
    pub phase: u32,
    /// 1, 2, or 3 for a press.
    pub clicks: u32,
    /// Existing gesture handle for drag/release.
    pub handle: u64,
    /// Viewport column.
    pub column: u16,
    /// Rectangular selection.
    pub rectangle: bool,
    /// Viewport row.
    pub row: u32,
    /// Surface x.
    pub x: f64,
    /// Surface y.
    pub y: f64,
    /// Surface columns.
    pub columns: u32,
    /// Cell width.
    pub cell_width: u32,
    /// Screen height.
    pub screen_height: u32,
    /// Left padding.
    pub padding_left: u32,
}

/// Pointer gesture output.
#[cfg(feature = "engine")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SelectionGestureResult {
    /// Runtime gesture handle.
    pub handle: u64,
    /// Start anchor; zero means no current range.
    pub start: u64,
    /// End anchor; zero means no current range.
    pub end: u64,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    /// The bootstrap profile `HELLO_OK` selected.
    pub profile: BootstrapProfile,
    /// The payload limits `HELLO_OK` selected.
    pub limits: BootstrapLimits,
    /// The scrollback depth the attach requested; bounds the default history cache.
    pub scrollback_lines: u32,
    /// An exact binding-supplied history policy, when present.
    pub history: Option<HistoryCacheConfig>,
}

impl EngineConfig {
    fn history(&self) -> HistoryCacheConfig {
        self.history.unwrap_or_else(|| HistoryCacheConfig {
            max_materialized_rows: self.scrollback_lines as usize,
            request_max_bytes: self.limits.max_history_page_bytes(),
            request_max_rows: self.scrollback_lines,
            ..HistoryCacheConfig::default()
        })
    }
}

enum Command {
    Apply(EngineEvent, Sender<EngineOutcome>),
    Lifecycle(Lifecycle),
    Query(Query),
}

enum Lifecycle {
    Detach(ResourceId, Sender<bool>),
    Reset(Sender<()>),
    #[cfg(feature = "engine")]
    Retain(ResourceId, bool),
    #[cfg(feature = "engine")]
    Release(ResourceId),
}

enum Query {
    HasProjection(ResourceId, Sender<bool>),
    IsClosed(ResourceId, Sender<bool>),
    InputEligibility(ResourceId, Sender<InputEligibility>),
    InputReady(ResourceId, Sender<bool>),
    #[cfg(feature = "engine")]
    Scroll(
        ResourceId,
        Scroll,
        Sender<Result<EngineOutcome, EngineError>>,
    ),
    #[cfg(feature = "engine")]
    IsAltScreen(ResourceId, Sender<bool>),
    #[cfg(feature = "engine")]
    Republish(ResourceId, Sender<Result<bool, EngineError>>),
    #[cfg(feature = "engine")]
    ReplicaInfo(ResourceId, Sender<Result<ReplicaInfo, EngineError>>),
    #[cfg(feature = "engine")]
    MouseMode(ResourceId, Sender<Result<MouseMode, EngineError>>),
    #[cfg(feature = "engine")]
    TrackAnchor(
        ResourceId,
        EngineDocumentPoint,
        Sender<Result<u64, EngineError>>,
    ),
    #[cfg(feature = "engine")]
    ReleaseAnchor(ResourceId, u64, Sender<Result<(), EngineError>>),
    #[cfg(feature = "engine")]
    ClearPresentation(ResourceId, u64, u64, Sender<Result<(), EngineError>>),
    #[cfg(feature = "engine")]
    PinViewport(ResourceId, u64, Sender<Result<EngineOutcome, EngineError>>),
    #[cfg(feature = "engine")]
    FollowLive(ResourceId, Sender<Result<EngineOutcome, EngineError>>),
    #[cfg(feature = "engine")]
    SetSelection(ResourceId, u64, u64, bool, Sender<Result<(), EngineError>>),
    #[cfg(feature = "engine")]
    ClearSelection(ResourceId, Sender<Result<(), EngineError>>),
    #[cfg(feature = "engine")]
    SelectionText(ResourceId, Sender<Result<Vec<u8>, EngineError>>),
    #[cfg(feature = "engine")]
    Search(
        ResourceId,
        String,
        bool,
        Sender<Result<Vec<SearchMatch>, EngineError>>,
    ),
    #[cfg(feature = "engine")]
    Gesture(
        ResourceId,
        SelectionGestureEvent,
        Sender<Result<SelectionGestureResult, EngineError>>,
    ),
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

    /// Forget a terminal and its projection after the owner thread applies
    /// the lifecycle transition. Returns `false` while an aggregate attach
    /// barrier still owns the terminal.
    #[must_use]
    pub fn detach(&self, terminal_id: ResourceId) -> bool {
        self.request(|reply| Command::Lifecycle(Lifecycle::Detach(terminal_id, reply)))
            .unwrap_or(false)
    }

    /// Release every connection-scoped replica and projection.
    pub(crate) fn reset_connection(&self) {
        let _ = self.request(|reply| Command::Lifecycle(Lifecycle::Reset(reply)));
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

    /// The kernel's current input eligibility for one terminal.
    #[must_use]
    pub fn input_eligibility(&self, terminal_id: &ResourceId) -> InputEligibility {
        self.request(|reply| Command::Query(Query::InputEligibility(terminal_id.clone(), reply)))
            .unwrap_or(InputEligibility::Ineligible(
                InputBlockReason::UnknownTerminal,
            ))
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
    pub(crate) fn scroll(
        &self,
        terminal_id: &ResourceId,
        scroll: Scroll,
    ) -> Result<EngineOutcome, EngineError> {
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

    /// Owned facts about the current replica.
    #[cfg(feature = "engine")]
    pub fn replica_info(&self, terminal_id: &ResourceId) -> Result<ReplicaInfo, EngineError> {
        self.request(|reply| Command::Query(Query::ReplicaInfo(terminal_id.clone(), reply)))?
    }

    /// The terminal's full DEC mouse-tracking mode.
    #[cfg(feature = "engine")]
    pub fn mouse_mode(&self, terminal_id: &ResourceId) -> Result<MouseMode, EngineError> {
        self.request(|reply| Command::Query(Query::MouseMode(terminal_id.clone(), reply)))?
    }

    /// Track a document coordinate on the owner thread.
    #[cfg(feature = "engine")]
    pub fn track_anchor(
        &self,
        terminal_id: &ResourceId,
        point: EngineDocumentPoint,
    ) -> Result<u64, EngineError> {
        self.request(|reply| Command::Query(Query::TrackAnchor(terminal_id.clone(), point, reply)))?
    }

    /// Release one document anchor.
    #[cfg(feature = "engine")]
    pub fn release_anchor(&self, terminal_id: &ResourceId, anchor: u64) -> Result<(), EngineError> {
        self.request(|reply| {
            Command::Query(Query::ReleaseAnchor(terminal_id.clone(), anchor, reply))
        })?
    }

    /// Clear local presentation for an exact replica generation.
    #[cfg(feature = "engine")]
    pub fn clear_presentation(
        &self,
        terminal_id: &ResourceId,
        stream_id: u64,
        bootstrap_id: u64,
    ) -> Result<(), EngineError> {
        self.request(|reply| {
            Command::Query(Query::ClearPresentation(
                terminal_id.clone(),
                stream_id,
                bootstrap_id,
                reply,
            ))
        })?
    }

    /// Pin the viewport to an existing document anchor.
    #[cfg(feature = "engine")]
    pub(crate) fn pin_viewport(
        &self,
        terminal_id: &ResourceId,
        anchor: u64,
    ) -> Result<EngineOutcome, EngineError> {
        self.request(|reply| {
            Command::Query(Query::PinViewport(terminal_id.clone(), anchor, reply))
        })?
    }

    /// Follow the live history tail.
    #[cfg(feature = "engine")]
    pub(crate) fn follow_live(
        &self,
        terminal_id: &ResourceId,
    ) -> Result<EngineOutcome, EngineError> {
        self.request(|reply| Command::Query(Query::FollowLive(terminal_id.clone(), reply)))?
    }

    /// Set a document selection from two runtime anchor handles.
    #[cfg(feature = "engine")]
    pub fn set_selection(
        &self,
        terminal_id: &ResourceId,
        start: u64,
        end: u64,
        rectangle: bool,
    ) -> Result<(), EngineError> {
        self.request(|reply| {
            Command::Query(Query::SetSelection(
                terminal_id.clone(),
                start,
                end,
                rectangle,
                reply,
            ))
        })?
    }

    /// Clear a document selection.
    #[cfg(feature = "engine")]
    pub fn clear_selection(&self, terminal_id: &ResourceId) -> Result<(), EngineError> {
        self.request(|reply| Command::Query(Query::ClearSelection(terminal_id.clone(), reply)))?
    }

    /// Format the active selection as owned UTF-8 bytes.
    #[cfg(feature = "engine")]
    pub fn selection_text(&self, terminal_id: &ResourceId) -> Result<Vec<u8>, EngineError> {
        self.request(|reply| Command::Query(Query::SelectionText(terminal_id.clone(), reply)))?
    }

    /// Search loaded history and return owner-thread anchor handles.
    #[cfg(feature = "engine")]
    pub fn search(
        &self,
        terminal_id: &ResourceId,
        query: String,
        case_sensitive: bool,
    ) -> Result<Vec<SearchMatch>, EngineError> {
        self.request(|reply| {
            Command::Query(Query::Search(
                terminal_id.clone(),
                query,
                case_sensitive,
                reply,
            ))
        })?
    }

    /// Apply one provider-owned selection gesture.
    #[cfg(feature = "engine")]
    pub fn selection_gesture(
        &self,
        terminal_id: &ResourceId,
        event: SelectionGestureEvent,
    ) -> Result<SelectionGestureResult, EngineError> {
        self.request(|reply| Command::Query(Query::Gesture(terminal_id.clone(), event, reply)))?
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
