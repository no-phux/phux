//! The lifecycle status an `AgentSession` stream derives, and its mapping
//! onto the shared `phux.agent/v1` state vocabulary.

use crate::agent_record::AgentState;

/// Lifecycle status derived from the record stream.
///
/// Idle is not derived here: an agent that stopped emitting records is not
/// thereby idle, and the server-side detector owns that judgement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentSessionStatus {
    /// No record has said anything about state yet.
    #[default]
    Unknown,
    /// The agent declared itself idle through an explicit `state` record.
    Idle,
    /// A prompt or tool call is in flight.
    Working,
    /// The agent is waiting on a human.
    Blocked,
    /// The agent finished its turn.
    Done,
    /// The provider session ended; the state is retracted.
    Ended,
}

impl AgentSessionStatus {
    /// Decode an explicit `state` word; anything outside the vocabulary is
    /// [`Self::Unknown`].
    #[must_use]
    pub fn parse(word: &str) -> Self {
        match word {
            "idle" => Self::Idle,
            "working" => Self::Working,
            "blocked" => Self::Blocked,
            "done" => Self::Done,
            _ => Self::Unknown,
        }
    }

    /// The kebab-case display word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Ended => "ended",
        }
    }

    /// This status in the `phux.agent/v1` state vocabulary. `None` for
    /// [`Self::Ended`]: the session is retracted and must not be shown.
    #[must_use]
    pub const fn as_agent_state(self) -> Option<AgentState> {
        Some(match self {
            Self::Unknown => AgentState::Unknown,
            Self::Idle => AgentState::Idle,
            Self::Working => AgentState::Working,
            Self::Blocked => AgentState::Blocked,
            Self::Done => AgentState::Done,
            Self::Ended => return None,
        })
    }
}
