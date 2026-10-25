//! Owned engine inputs, outcomes, configuration, and query results.

#[cfg(all(doc, feature = "engine"))]
use super::EngineHandle;
use super::{
    AgentEvent, BootstrapId, BootstrapLimits, BootstrapProfile, BootstrapStreamProfile,
    CloseReason, HistoryCacheConfig, HistoryRejectionReason, HistoryUnavailableReason,
    KernelEffect, KernelError, KernelStatus, ResourceId, StreamId, TombstoneReason,
};
#[cfg(feature = "engine")]
use super::{Arc, GridFrame, HistoryStatus};
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
        payload: bytes::Bytes,
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
        payload: bytes::Bytes,
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
        bytes: bytes::Bytes,
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
    /// A close the control plane inferred (a pane the same snapshot view
    /// listed before and no longer lists) rather than received.
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
    pub(super) fn from_kernel<E: std::fmt::Display>(error: &KernelError<E>) -> Self {
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
    /// Put the `n`th shell prompt (OSC 133) above (negative) or below
    /// (positive) the viewport's top row at the top, as Ghostty's
    /// `jump_to_prompt` does; a prompt in the active area follows the tail.
    /// Without that many prompts, the farthest one found is used; with none,
    /// the viewport stays put.
    Prompt(i64),
}

/// A whole-document text range a consumer can copy out.
#[cfg(feature = "engine")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRegion {
    /// Loaded scrollback plus the active screen.
    Screen,
    /// Loaded scrollback only; empty on the alternate screen.
    History,
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
    /// Whether the terminal accepts input now, as [`EngineHandle::input_ready`]
    /// answers, read in the same owner turn so a consumer validating a
    /// presentation needs one round trip, not two.
    pub input_ready: bool,
}

/// One view as a single owner turn saw it: its replica facts and the frame
/// published for exactly that state, caught up first if its projection was
/// deferred. See [`EngineHandle::present_view`].
#[cfg(feature = "engine")]
#[derive(Debug, Clone)]
pub struct ViewPresentation {
    /// The replica behind the view.
    pub replica: ReplicaInfo,
    /// The view's current frame, now marked read; `None` before its first
    /// publication.
    pub frame: Option<Arc<GridFrame>>,
}

/// Where a replica's shell is in its OSC 133 prompt cycle, read from the
/// replica's own marks rather than from events, so it survives a reconnect
/// and an attach while a command already runs.
#[cfg(feature = "engine")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaPrompt {
    /// The cursor sits on a prompt or input row: the shell reads a command
    /// line.
    AtPrompt,
    /// Prompt marks exist, but the cursor is off them: a command has the
    /// screen.
    Running,
    /// No marks, the alternate screen, or no replica: shell integration is
    /// off or unknowable here.
    Unknown,
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

/// The most matches one search registers; each match holds two anchors.
#[cfg(feature = "engine")]
pub const SEARCH_MATCH_LIMIT: usize = 4096;

/// One bounded search: its matches and whether a bound hid more.
#[cfg(feature = "engine")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResults {
    /// Matches in document order.
    pub matches: Vec<SearchMatch>,
    /// The requested bound, [`SEARCH_MATCH_LIMIT`], or the replica's anchor
    /// budget hid at least one more match.
    pub truncated: bool,
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
    /// The requested terminal has no available replica to project.
    #[error("no projection for this terminal")]
    ProjectionUnavailable,
    /// The owner thread could not be spawned.
    #[error("could not start the engine owner thread: {0}")]
    Spawn(std::io::Error),
    /// The engine refused a render or a viewport operation.
    #[error("engine: {0}")]
    Engine(String),
    /// A document-anchor handle no longer resolves: it was released, its
    /// history row was evicted, or a new replica generation invalidated it.
    #[error("engine: {0}")]
    AnchorUnavailable(String),
}

/// The complete result of a bounded selection copy.
#[cfg(feature = "engine")]
pub use phux_client_core::engine::BoundedSelectionText;

/// Client-side terminal colours (ADR-0157): palette entries 0..=15 plus
/// the default foreground, background, and cursor, each `[r, g, b]`.
///
/// Installed as engine *defaults* on every replica, so an application's OSC
/// 4 / 10 / 11 / 12 still override them and OSC 104 / 110 / 111 / 112 revert
/// to the theme. The mapping from a theme's semantic colours to these slots
/// is fixed by ADR-0157 decision 4 (`phux_config::theme::ANSI16_KEYS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalTheme {
    /// Palette entries 0..=15.
    pub ansi16: [[u8; 3]; 16],
    /// Default foreground.
    pub foreground: [u8; 3],
    /// Default background.
    pub background: [u8; 3],
    /// Default cursor colour.
    pub cursor: [u8; 3],
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
    pub(crate) fn history(&self) -> HistoryCacheConfig {
        self.history.unwrap_or_else(|| HistoryCacheConfig {
            max_materialized_rows: self.scrollback_lines as usize,
            request_max_bytes: self.limits.max_history_page_bytes(),
            request_max_rows: self.scrollback_lines,
            ..HistoryCacheConfig::default()
        })
    }
}
