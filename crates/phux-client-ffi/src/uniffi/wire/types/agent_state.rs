//! Agent record publications and agent state/attention readings.

#![allow(
    clippy::wildcard_imports,
    reason = "wire records intentionally share the parent UniFFI enum namespace"
)]

use super::*;

/// What a `WireEvent::AgentRecords` carries.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRecordsKind {
    /// The whole retained stream; replaces every record held.
    Retained,
    /// An incremental batch from live output or a retained resume suffix, appended.
    Live,
    /// The session ended; no records follow.
    Closed,
}

/// One coherent sample of the session for a polling UI (`take_publication`):
/// the ordered events since the last drain plus, from the same instant, the
/// status, error and topology they lead to. Unlike calling `take_events`,
/// `status`, `last_error` and `topology` separately, a reconnect cannot land
/// between the parts.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WirePublication {
    /// The runtime's connection incarnation, bumped by every transport it
    /// opens and never reused: equal across two publications means no
    /// reconnect happened between them. `0` before the first dial.
    pub connection_epoch: u64,
    /// Every event since the last drain, in order, including the
    /// `ConnectionLost` / `ConnectionOpened` lifecycle `take_events` omits.
    pub events: Vec<WireEvent>,
    /// The bounded event queue dropped events since the last drain. The
    /// latest `ConnectionOpened` and every correlated reply survive; a
    /// `TopologyChanged` follows, and agent records and asked flags are
    /// re-read and arrive as later events, so re-derive from those rather
    /// than resetting what this batch cannot show.
    pub events_dropped: bool,
    pub status: WireStatus,
    pub last_error: Option<String>,
    pub topology: Option<SessionTopology>,
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

/// How an `AgentStateChanged`'s `state` was read (`projection::agent`).
/// Provenance only: every case but `Recognized` carries `state: Unknown`.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum AgentStateReading {
    /// No record: absent, deleted, malformed, or nameless. No agent.
    NoRecord,
    /// The record declared no `state`.
    Undeclared,
    /// The record declared `"unknown"` explicitly.
    Indeterminate,
    /// A word this build recognizes.
    Recognized,
    /// A newer or malformed value; `raw` is its bounded, display-safe text.
    Unsupported { raw: String },
}

impl From<agent::StateReading> for AgentStateReading {
    fn from(value: agent::StateReading) -> Self {
        match value {
            agent::StateReading::NoRecord => Self::NoRecord,
            agent::StateReading::Undeclared => Self::Undeclared,
            agent::StateReading::Indeterminate => Self::Indeterminate,
            agent::StateReading::Recognized => Self::Recognized,
            agent::StateReading::Unsupported(raw) => Self::Unsupported { raw },
        }
    }
}

/// How an `AgentStateChanged`'s effective `attention` was arrived at.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum AgentAttentionReading {
    /// Undeclared: derived from `state`.
    Derived,
    /// Declared with a word this build recognizes.
    Declared,
    /// A newer or malformed value, read as `Normal`; `raw` is its bounded text.
    Unsupported { raw: String },
}

impl From<agent::AttentionReading> for AgentAttentionReading {
    fn from(value: agent::AttentionReading) -> Self {
        match value {
            agent::AttentionReading::Derived => Self::Derived,
            agent::AttentionReading::Declared => Self::Declared,
            agent::AttentionReading::Unsupported(raw) => Self::Unsupported { raw },
        }
    }
}
