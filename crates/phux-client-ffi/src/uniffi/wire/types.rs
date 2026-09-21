#![allow(
    clippy::wildcard_imports,
    reason = "wire records intentionally share the parent UniFFI enum namespace"
)]

//! The Swift/Kotlin-facing records, and the lowering from
//! [`crate::projection`] that fills them.
//!
//! Every type here is a mirror of a projected value, kept because the foreign
//! names are a published surface: renaming `SessionTopology` to the
//! projection's `SessionGraph` would rewrite generated Swift for no product
//! reason. `UniFFI` 0.28's `remote` derives only reach types in *other* crates,
//! so they cannot remove these mirrors without putting `uniffi` derives into
//! the binding-neutral layer — which is exactly what ADR-0135 forbids. The
//! mirrors are therefore `From` impls and nothing else: no decision is taken
//! twice, only spelled twice.

use super::*;

/// Connection status, polled by Swift to drive UI.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireStatus {
    Idle,
    Connecting,
    Attached,
    Closed,
    Failed,
}

impl From<status::Connection> for WireStatus {
    fn from(value: status::Connection) -> Self {
        match value {
            status::Connection::Connecting => Self::Connecting,
            status::Connection::Attached => Self::Attached,
            status::Connection::Closed => Self::Closed,
            status::Connection::Failed => Self::Failed,
        }
    }
}

#[derive(uniffi::Error, Debug, thiserror::Error)]
pub enum WireError {
    #[error("already connected")]
    AlreadyConnected,
    #[error("runtime error: {reason}")]
    Runtime { reason: String },
}

/// Product-facing outcome of one acknowledged input operation.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireInputDeliveryOutcome {
    Delivered,
    Refused,
    Unknown,
}

impl From<outcome::Delivery> for WireInputDeliveryOutcome {
    fn from(value: outcome::Delivery) -> Self {
        match value {
            outcome::Delivery::Delivered => Self::Delivered,
            outcome::Delivery::Refused => Self::Refused,
            outcome::Delivery::Unknown => Self::Unknown,
        }
    }
}

/// One terminal result from the acknowledged-input lane. `delivery_id` is a
/// bridge-local correlation only; the secret wire operation id is never
/// exposed across FFI.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireInputDelivery {
    pub delivery_id: u64,
    pub outcome: WireInputDeliveryOutcome,
    pub code: Option<u16>,
    pub message: String,
}

impl From<outcome::InputDelivery> for WireInputDelivery {
    fn from(value: outcome::InputDelivery) -> Self {
        Self {
            delivery_id: value.delivery_id,
            outcome: value.outcome.into(),
            code: value.code,
            message: value.message,
        }
    }
}

/// Product-facing outcome of one chunked file upload.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireFileUploadOutcome {
    Completed,
    Refused,
    Unknown,
}

impl From<outcome::Upload> for WireFileUploadOutcome {
    fn from(value: outcome::Upload) -> Self {
        match value {
            outcome::Upload::Completed => Self::Completed,
            outcome::Upload::Refused => Self::Refused,
            outcome::Upload::Unknown => Self::Unknown,
        }
    }
}

/// Terminal result for one bridge-managed file upload. `transfer_id` is local;
/// the secret, retry-stable wire upload id never crosses FFI.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireFileUploadReceipt {
    pub transfer_id: u64,
    pub outcome: WireFileUploadOutcome,
    pub path: Option<String>,
    pub code: Option<u16>,
    pub message: String,
}

impl From<outcome::UploadReceipt> for WireFileUploadReceipt {
    fn from(value: outcome::UploadReceipt) -> Self {
        Self {
            transfer_id: value.transfer_id,
            outcome: value.outcome.into(),
            path: value.path,
            code: value.code,
            message: value.message,
        }
    }
}

/// Product-facing outcome of one `TRANSCRIBE` request (phux-ctf).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireTranscribeOutcome {
    /// The server answered: `text` is what it heard, `pasted` says whether it
    /// reached the pane (false for silence).
    Completed,
    /// The server refused: `code`/`message` carry the reason and its remedy.
    Refused,
    /// The connection ended before the server answered. The clip may or may
    /// not have been pasted; read the pane before retrying.
    Unknown,
}

impl From<outcome::Transcribe> for WireTranscribeOutcome {
    fn from(value: outcome::Transcribe) -> Self {
        match value {
            outcome::Transcribe::Completed => Self::Completed,
            outcome::Transcribe::Refused => Self::Refused,
            outcome::Transcribe::Unknown => Self::Unknown,
        }
    }
}

/// Terminal result for one `TRANSCRIBE` request. `transfer_id` is the local
/// upload handle the request was made for; `request_id` the wire correlation.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireTranscribeReceipt {
    pub request_id: u32,
    pub transfer_id: u64,
    pub outcome: WireTranscribeOutcome,
    pub text: Option<String>,
    pub pasted: bool,
    pub code: Option<u16>,
    pub message: String,
}

impl From<outcome::TranscribeResult> for WireTranscribeReceipt {
    fn from(value: outcome::TranscribeResult) -> Self {
        Self {
            request_id: value.request_id,
            transfer_id: value.transfer_id,
            outcome: value.outcome.into(),
            text: value.text,
            pasted: value.pasted,
            code: value.code,
            message: value.message,
        }
    }
}

/// Why a `LIST_DIRECTORY` produced no listing. The first four mirror the
/// wire's `DirectoryErrorCode` one for one (an unallocated wire value already
/// reads as `Other`); `Unanswered` is bridge-local: the connection carrying
/// the request ended before the server replied. Listing is read-only, so a
/// retry is always safe.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireDirectoryErrorCode {
    NotFound,
    PermissionDenied,
    NotADirectory,
    Other,
    Unanswered,
}

impl From<outcome::DirectoryError> for WireDirectoryErrorCode {
    fn from(value: outcome::DirectoryError) -> Self {
        match value {
            outcome::DirectoryError::NotFound => Self::NotFound,
            outcome::DirectoryError::PermissionDenied => Self::PermissionDenied,
            outcome::DirectoryError::NotADirectory => Self::NotADirectory,
            outcome::DirectoryError::Other => Self::Other,
            outcome::DirectoryError::Unanswered => Self::Unanswered,
        }
    }
}

/// One child directory in a listing.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireDirectoryEntry {
    pub name: String,
    /// True when the entry is a symbolic link that resolves to a directory.
    pub is_symlink: bool,
}

impl From<outcome::DirectoryEntry> for WireDirectoryEntry {
    fn from(value: outcome::DirectoryEntry) -> Self {
        Self {
            name: value.name,
            is_symlink: value.is_symlink,
        }
    }
}

/// The correlated answer to one `list_directory` call, drained losslessly
/// through `take_directory_listings`. `error` is `None` on success; then
/// `path` is the resolved absolute path, `parent` its lexical parent (`None`
/// at the root), and `entries` the child directories sorted by name, capped
/// server-side with `truncated` set. On a refusal `path` is the path the
/// server attempted, the listing fields are empty, and `message` is
/// diagnostic text that must not be parsed.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireDirectoryListing {
    pub request_id: u32,
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<WireDirectoryEntry>,
    pub truncated: bool,
    pub error: Option<WireDirectoryErrorCode>,
    pub message: String,
}

impl From<outcome::Directory> for WireDirectoryListing {
    fn from(value: outcome::Directory) -> Self {
        Self {
            request_id: value.request_id,
            path: value.path,
            parent: value.parent,
            entries: value.entries.into_iter().map(Into::into).collect(),
            truncated: value.truncated,
            error: value.error.map(Into::into),
            message: value.message,
        }
    }
}

/// One session visible in the server's ATTACHED snapshot.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct SessionDescriptor {
    pub id: u32,
    pub name: String,
    pub window_count: u16,
    pub attached_client_count: u16,
}

impl From<topology::Session> for SessionDescriptor {
    fn from(value: topology::Session) -> Self {
        Self {
            id: value.id,
            name: value.name,
            window_count: value.window_count,
            attached_client_count: value.attached_client_count,
        }
    }
}

/// One pane (terminal) visible in the server's ATTACHED snapshot,
/// denormalized with its window/session context so Swift needs no joins.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PaneDescriptor {
    /// Stable string form of the wire `ResourceId` (e.g. "local:7"); the key
    /// Swift hands back to target this pane.
    pub terminal_id: String,
    pub session_id: u32,
    pub session_name: String,
    pub window_id: u32,
    pub window_index: u16,
    pub window_name: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    /// Whether this pane is the attaching client's initial focus.
    pub is_focused: bool,
}

impl From<topology::Pane> for PaneDescriptor {
    fn from(value: topology::Pane) -> Self {
        Self {
            terminal_id: value.terminal_id,
            session_id: value.session_id,
            session_name: value.session_name,
            window_id: value.window_id,
            window_index: value.window_index,
            window_name: value.window_name,
            title: value.title,
            cwd: value.cwd,
            is_focused: value.is_focused,
        }
    }
}

/// The session/window/pane graph from the ATTACHED frame, projected for the
/// home screen. One connection sees exactly one session (SPEC §13), so the
/// sessions list is the attached session (plus any the server adds later).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct SessionTopology {
    pub sessions: Vec<SessionDescriptor>,
    pub panes: Vec<PaneDescriptor>,
    /// String form of the attaching client's initial focused pane.
    pub focused_pane: String,
}

impl From<topology::SessionGraph> for SessionTopology {
    fn from(value: topology::SessionGraph) -> Self {
        Self {
            sessions: value.sessions.into_iter().map(Into::into).collect(),
            panes: value.panes.into_iter().map(Into::into).collect(),
            focused_pane: value.focused_pane,
        }
    }
}

/// A state change pushed by the server, drained by Swift via `take_events()`
/// on the poll cadence. Wire-faithful naming: `OutputStarted`/`OutputSettled`
/// mirror the server's coalesced `Dirty`/`Idle` burst events — Swift maps
/// them onto its own status vocabulary.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum WireEvent {
    /// The session graph changed (a fresh ATTACHED arrived) — re-read
    /// `topology()`.
    TopologyChanged,
    /// A new pane appeared mid-session. The server pumps output only to the
    /// spawning client; when the spawner is someone else, the bridge has
    /// already sent `ATTACH_TERMINAL` for the pane on the live socket
    /// (phux-gt1) plus a `GET_STATE` topology refresh — no resync needed.
    /// Consumers just wait for the follow-up `TerminalAttached` /
    /// `TopologyChanged`.
    PaneSpawned { terminal_id: String },
    /// The server answered one of OUR `SPAWN_TERMINAL` requests
    /// (`TERMINAL_SPAWNED`, correlated by `request_id` — the value
    /// `spawn_terminal_with_command()` returned). Exactly one of
    /// `terminal_id` / `error` is set. This is how the hidden utility pane
    /// for image paste (ADR-0018) learns its target id; the server pumps a
    /// spawned pane's output to its spawner, so no resync is needed.
    TerminalSpawned {
        request_id: u32,
        terminal_id: Option<String>,
        error: Option<String>,
    },
    /// The server answered one of OUR `ATTACH_TERMINAL` commands
    /// (`COMMAND_RESULT`, correlated by `request_id` — the value
    /// `attach_terminal()` returned, or a bridge-internal id for the
    /// automatic foreign-pane pickup). `error` is `None` on success; by
    /// then the pane's authoritative `TERMINAL_SNAPSHOT` has already been
    /// routed into its buffer (the server sends it before the Ok).
    TerminalAttached {
        request_id: u32,
        terminal_id: String,
        error: Option<String>,
    },
    /// The server answered one of OUR `DETACH_TERMINAL` commands. The
    /// verb is idempotent server-side, so `error` is rare (transport-level
    /// failures surface elsewhere); it is carried for symmetry.
    TerminalDetached {
        request_id: u32,
        terminal_id: String,
        error: Option<String>,
    },
    /// A pane closed (process exit, signal, or kill). `exit_status` is the
    /// process exit code, `None` for signals/unknown.
    PaneClosed {
        terminal_id: String,
        exit_status: Option<i32>,
    },
    /// The pane received a BEL.
    Bell { terminal_id: String },
    /// The pane's title changed (OSC 0/2). The topology's pane entry is
    /// updated in place as well.
    TitleChanged { terminal_id: String, title: String },
    /// Output burst began (server `Dirty`): the pane is actively producing.
    OutputStarted { terminal_id: String },
    /// Output settled (server `Idle`): the pane went quiet.
    OutputSettled { terminal_id: String },
    /// A shell command began executing (OSC-133 C mark) — server-truth
    /// "running", strictly better than the burst heuristic for plain shells.
    CommandStarted { terminal_id: String },
    /// A shell command finished (OSC-133 D mark). `exit_code` is present when
    /// the shell integration reported one (`OSC 133 ; D ; n`).
    CommandFinished {
        terminal_id: String,
        exit_code: Option<i32>,
    },
    /// The pane's working directory changed (kernel cwd re-queried at prompt
    /// boundaries). The topology's pane entry is updated in place as well.
    CwdChanged { terminal_id: String, cwd: String },
    /// An agent in the pane is waiting on a human answer (phux-2sl6). Projects
    /// the wire `AgentEvent::Asked` so Swift can populate its `AgentQuestion`
    /// without re-deriving the prompt from the grid: `id`/`question`/`suggestions`
    /// map one-for-one and `waiting_seconds` is the optional `elapsed_seconds`.
    AgentAsked {
        terminal_id: String,
        question_id: String,
        text: String,
        suggestions: Vec<String>,
        waiting_seconds: Option<u64>,
    },
    /// The pane's `phux.agent/v1` L3 record changed, or its current value
    /// arrived on (re)attach (phux-cck / ADR-0046: the server-side detector
    /// derives `state` from the live screen and publishes edge-filtered
    /// writes; the bridge's per-pane GET replays the CURRENT record on every
    /// attach, so the level survives reconnects — phux-q7e.21). A tombstone,
    /// an absent record, or malformed bytes all project as `Unknown` with an
    /// empty `name`: "no declared agent", never a stale badge.
    AgentStateChanged {
        terminal_id: String,
        /// Human-facing agent name; empty when no record is declared.
        name: String,
        /// Open-vocabulary kind slug (e.g. "claude"), when declared.
        kind: Option<String>,
        /// Free-form association label declared by the agent record.
        session: Option<String>,
        state: AgentState,
        /// The EFFECTIVE attention: the record's declared level, or the
        /// spec's derivation from `state` when absent (L3.md §3.7).
        attention: AgentAttention,
    },
    /// The server reported an ERROR frame.
    ServerError { message: String },
}

impl From<agent::AgentBadge> for WireEvent {
    fn from(value: agent::AgentBadge) -> Self {
        Self::AgentStateChanged {
            terminal_id: id::encode(&value.terminal_id),
            name: value.name,
            kind: value.kind,
            session: value.session,
            state: value.state.into(),
            attention: value.attention.into(),
        }
    }
}

impl From<event::TerminalSignal> for Option<WireEvent> {
    /// The Swift surface has no vocabulary for replica tombstones or
    /// progressive history, so those signals project to nothing. They are
    /// the runtime's business and the C lane's; a phone re-reads the pane.
    fn from(value: event::TerminalSignal) -> Self {
        Some(match value {
            event::TerminalSignal::Bell { terminal_id } => WireEvent::Bell {
                terminal_id: id::encode(&terminal_id),
            },
            event::TerminalSignal::TitleChanged { terminal_id, title } => WireEvent::TitleChanged {
                terminal_id: id::encode(&terminal_id),
                title,
            },
            event::TerminalSignal::CwdChanged { terminal_id, cwd } => WireEvent::CwdChanged {
                terminal_id: id::encode(&terminal_id),
                cwd,
            },
            event::TerminalSignal::OutputStarted { terminal_id } => WireEvent::OutputStarted {
                terminal_id: id::encode(&terminal_id),
            },
            event::TerminalSignal::OutputSettled { terminal_id } => WireEvent::OutputSettled {
                terminal_id: id::encode(&terminal_id),
            },
            event::TerminalSignal::CommandStarted { terminal_id } => WireEvent::CommandStarted {
                terminal_id: id::encode(&terminal_id),
            },
            event::TerminalSignal::CommandFinished {
                terminal_id,
                exit_code,
            } => WireEvent::CommandFinished {
                terminal_id: id::encode(&terminal_id),
                exit_code,
            },
            event::TerminalSignal::Resync { .. }
            | event::TerminalSignal::History { .. }
            | event::TerminalSignal::HistoryUnavailable { .. } => return None,
        })
    }
}

impl From<event::Lifecycle> for Option<WireEvent> {
    /// `Exited` projects to nothing: the runtime follows it with a
    /// `Closed` for a pane that is really gone, and a retained pane
    /// (ADR-0124) must not disappear from the phone's list.
    fn from(value: event::Lifecycle) -> Self {
        Some(match value {
            event::Lifecycle::PaneSpawned { terminal_id } => WireEvent::PaneSpawned {
                terminal_id: id::encode(&terminal_id),
            },
            event::Lifecycle::SpawnAnswered {
                request_id,
                terminal_id,
                error,
            } => WireEvent::TerminalSpawned {
                request_id,
                terminal_id: terminal_id.as_ref().map(id::encode),
                error,
            },
            event::Lifecycle::AttachAnswered {
                request_id,
                terminal_id,
                error,
            } => WireEvent::TerminalAttached {
                request_id,
                terminal_id: id::encode(&terminal_id),
                error,
            },
            event::Lifecycle::DetachAnswered {
                request_id,
                terminal_id,
                error,
            } => WireEvent::TerminalDetached {
                request_id,
                terminal_id: id::encode(&terminal_id),
                error,
            },
            event::Lifecycle::Closed {
                terminal_id,
                exit_status,
                ..
            } => WireEvent::PaneClosed {
                terminal_id: id::encode(&terminal_id),
                exit_status,
            },
            event::Lifecycle::ServerError { message, .. } => WireEvent::ServerError { message },
            event::Lifecycle::Exited { .. } | event::Lifecycle::Detached { .. } => return None,
        })
    }
}

/// Lifecycle state a `phux.agent/v1` record declares (ADR-0040/0046).
/// OPEN enum on the wire: an unrecognized (newer) word projects as
/// `Unknown`, mirroring the reference client's `AgentMetaState`.
#[derive(uniffi::Enum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentState {
    /// No state declared, or an unrecognized (newer) vocabulary value.
    #[default]
    Unknown,
    Idle,
    Working,
    Blocked,
    Done,
}

impl From<agent::AgentState> for AgentState {
    fn from(value: agent::AgentState) -> Self {
        match value {
            agent::AgentState::Unknown => Self::Unknown,
            agent::AgentState::Idle => Self::Idle,
            agent::AgentState::Working => Self::Working,
            agent::AgentState::Blocked => Self::Blocked,
            agent::AgentState::Done => Self::Done,
        }
    }
}

/// Attention priority for a `phux.agent/v1` record. OPEN enum on the wire:
/// an unrecognized word projects as `Normal`, mirroring the reference
/// client's `AgentAttention`.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentAttention {
    None,
    Low,
    Normal,
    High,
}

impl From<agent::AgentAttention> for AgentAttention {
    fn from(value: agent::AgentAttention) -> Self {
        match value {
            agent::AgentAttention::None => Self::None,
            agent::AgentAttention::Low => Self::Low,
            agent::AgentAttention::Normal => Self::Normal,
            agent::AgentAttention::High => Self::High,
        }
    }
}

/// Touch-producible mouse actions, mirrored for FFI (ADR-0024: the wire owns
/// the atoms; these are projections).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

impl MouseAction {
    pub(super) fn wire(self) -> WireMouseAction {
        match self {
            Self::Press => WireMouseAction::Press,
            Self::Release => WireMouseAction::Release,
            Self::Motion => WireMouseAction::Motion,
        }
    }
}

/// Touch-producible mouse buttons. Wheel maps to the xterm wheel buttons so
/// a two-finger scroll can drive TUIs with mouse tracking.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    WheelUp,
    WheelDown,
}

impl MouseButton {
    pub(super) fn wire(self) -> WireMouseButton {
        match self {
            Self::Left => WireMouseButton::Left,
            Self::Right => WireMouseButton::Right,
            Self::Middle => WireMouseButton::Middle,
            Self::WheelUp => WireMouseButton::Four,
            Self::WheelDown => WireMouseButton::Five,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ResourceId;
    use phux_protocol::wire::frame::CloseReason;

    #[test]
    fn a_badge_lowers_with_its_string_identity() {
        let badge = agent::badge(
            &ResourceId::local(9),
            Some(br#"{"name":"Claude","kind":"claude","state":"newer"}"#),
        );
        assert_eq!(
            WireEvent::from(badge),
            WireEvent::AgentStateChanged {
                terminal_id: "local:9".to_owned(),
                name: "Claude".to_owned(),
                kind: Some("claude".to_owned()),
                session: None,
                state: AgentState::Unknown,
                attention: AgentAttention::Low,
            }
        );
    }

    #[test]
    fn a_malformed_record_lowers_to_an_empty_badge() {
        let badge = agent::badge(&ResourceId::local(2), Some(b"not-json"));
        assert_eq!(
            WireEvent::from(badge),
            WireEvent::AgentStateChanged {
                terminal_id: "local:2".to_owned(),
                name: String::new(),
                kind: None,
                session: None,
                state: AgentState::Unknown,
                attention: AgentAttention::Low,
            }
        );
    }

    #[test]
    fn replica_and_history_signals_have_no_swift_vocabulary() {
        let signal = event::TerminalSignal::HistoryUnavailable {
            terminal_id: ResourceId::local(1),
            reason: phux_client_core::session::HistoryUnavailableReason::Pruned,
        };
        assert_eq!(Option::<WireEvent>::from(signal), None);
    }

    #[test]
    fn a_burst_signal_lowers_to_its_event() {
        let signal = event::TerminalSignal::OutputStarted {
            terminal_id: ResourceId::local(1),
        };
        assert_eq!(
            Option::<WireEvent>::from(signal),
            Some(WireEvent::OutputStarted {
                terminal_id: "local:1".to_owned(),
            })
        );
    }

    #[test]
    fn a_close_becomes_a_pane_closed_and_an_exit_does_not() {
        let closed = event::Lifecycle::Closed {
            terminal_id: ResourceId::local(4),
            exit_status: Some(1),
            signal: None,
            reason: CloseReason::Exited,
        };
        assert_eq!(
            Option::<WireEvent>::from(closed),
            Some(WireEvent::PaneClosed {
                terminal_id: "local:4".to_owned(),
                exit_status: Some(1),
            })
        );
        let exited = event::Lifecycle::Exited {
            terminal_id: ResourceId::local(4),
            exit_status: Some(1),
            signal: None,
            reason: CloseReason::Exited,
        };
        assert_eq!(Option::<WireEvent>::from(exited), None);
    }

    #[test]
    fn a_spawn_answer_keeps_its_optional_terminal() {
        let answered = event::Lifecycle::SpawnAnswered {
            request_id: 3,
            terminal_id: None,
            error: Some("refused".to_owned()),
        };
        assert_eq!(
            Option::<WireEvent>::from(answered),
            Some(WireEvent::TerminalSpawned {
                request_id: 3,
                terminal_id: None,
                error: Some("refused".to_owned()),
            })
        );
    }

    #[test]
    fn statuses_fold_the_pre_attach_states_together() {
        assert_eq!(
            WireStatus::from(status::Connection::Connecting),
            WireStatus::Connecting
        );
        assert_eq!(
            WireStatus::from(status::Connection::Failed),
            WireStatus::Failed
        );
    }
}
