//! Attention grading and the triage order, lowered from
//! [`crate::projection::attention`] (ADR-0166). Pure functions: no client
//! object, no I/O.

use crate::projection::{agent, attention};

use super::wire::{AgentAttention, AgentState};

/// A pane's urgency, ordered from `None` to `High`.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttentionGrade {
    /// An explicit down-claim on a pane no one is waiting for.
    None,
    Low,
    /// No record, or any waiting pane that does not flag itself.
    Normal,
    /// A record declaring more than its state derives.
    High,
}

impl From<attention::AttentionGrade> for AttentionGrade {
    fn from(value: attention::AttentionGrade) -> Self {
        match value {
            attention::AttentionGrade::None => Self::None,
            attention::AttentionGrade::Low => Self::Low,
            attention::AttentionGrade::Normal => Self::Normal,
            attention::AttentionGrade::High => Self::High,
        }
    }
}

/// A pane's folded status, as a card shows it.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneStatus {
    Working,
    /// Waiting on a structured question.
    Asked,
    /// Waiting without a structured question (a permission prompt).
    Blocked,
    /// Finished; a non-zero `exit` is a failure.
    Done {
        exit: i32,
    },
    Idle,
}

impl From<PaneStatus> for attention::PaneStatus {
    fn from(value: PaneStatus) -> Self {
        match value {
            PaneStatus::Working => Self::Working,
            PaneStatus::Asked => Self::Asked,
            PaneStatus::Blocked => Self::Blocked,
            PaneStatus::Done { exit } => Self::Done { exit },
            PaneStatus::Idle => Self::Idle,
        }
    }
}

/// The `state` and effective `attention` of a pane's `phux.agent/v1` record,
/// as an `AgentStateChanged` event carried them. Pass `nil`/`null` for a pane
/// with no record.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeclaredAgent {
    pub state: AgentState,
    pub attention: AgentAttention,
}

impl From<DeclaredAgent> for attention::DeclaredAgent {
    fn from(value: DeclaredAgent) -> Self {
        Self {
            state: match value.state {
                AgentState::Unknown => agent::AgentState::Unknown,
                AgentState::Idle => agent::AgentState::Idle,
                AgentState::Working => agent::AgentState::Working,
                AgentState::Blocked => agent::AgentState::Blocked,
                AgentState::Done => agent::AgentState::Done,
            },
            attention: match value.attention {
                AgentAttention::None => agent::AgentAttention::None,
                AgentAttention::Low => agent::AgentAttention::Low,
                AgentAttention::Normal => agent::AgentAttention::Normal,
                AgentAttention::High => agent::AgentAttention::High,
            },
        }
    }
}

/// One row of a list to triage-order.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TriagePane {
    pub status: PaneStatus,
    pub agent: Option<DeclaredAgent>,
    /// Last activity in Unix milliseconds, rounded down; `nil`/`null` for
    /// none.
    pub last_activity_unix_ms: Option<i64>,
}

impl From<TriagePane> for attention::TriagePane {
    fn from(value: TriagePane) -> Self {
        Self {
            status: value.status.into(),
            agent: value.agent.map(Into::into),
            last_activity_unix_ms: value.last_activity_unix_ms,
        }
    }
}

/// A pane's attention grade. A waiting pane never grades below `Normal`.
#[uniffi::export]
pub fn attention_grade(status: PaneStatus, agent: Option<DeclaredAgent>) -> AttentionGrade {
    attention::grade(status.into(), agent.map(Into::into)).into()
}

/// Whether a record declares more attention than its state derives.
#[uniffi::export]
pub fn agent_flags_urgency(agent: DeclaredAgent) -> bool {
    attention::DeclaredAgent::from(agent).flags_urgency()
}

/// The input indices of `panes` in triage order: waiting, then failures,
/// then the rest; graded inside the waiting band; then by activity minute,
/// most recent first; then by input order.
#[uniffi::export]
pub fn triage_order(panes: Vec<TriagePane>) -> Vec<u32> {
    let panes: Vec<attention::TriagePane> = panes.into_iter().map(Into::into).collect();
    attention::triage_order(&panes)
        .into_iter()
        .map(|index| u32::try_from(index).unwrap_or(u32::MAX))
        .collect()
}

/// The minute triage ranks activity by, or `Int64.min` for no activity.
/// Two times with the same minute never reorder a list.
#[uniffi::export]
pub fn activity_minute(last_activity_unix_ms: Option<i64>) -> i64 {
    attention::activity_minute(last_activity_unix_ms)
}

#[cfg(test)]
mod tests {
    use super::{
        AgentAttention, AgentState, AttentionGrade, DeclaredAgent, PaneStatus, TriagePane,
        agent_flags_urgency, attention_grade, triage_order,
    };

    const URGENT: DeclaredAgent = DeclaredAgent {
        state: AgentState::Working,
        attention: AgentAttention::High,
    };

    #[test]
    fn the_encoder_carries_the_projection() {
        assert_eq!(
            attention_grade(PaneStatus::Asked, None),
            AttentionGrade::Normal
        );
        assert_eq!(
            attention_grade(PaneStatus::Idle, Some(URGENT)),
            AttentionGrade::High
        );
        assert_eq!(
            attention_grade(
                PaneStatus::Done { exit: 0 },
                Some(DeclaredAgent {
                    state: AgentState::Idle,
                    attention: AgentAttention::None,
                })
            ),
            AttentionGrade::None
        );
        assert!(agent_flags_urgency(URGENT));
        let row = |status, agent| TriagePane {
            status,
            agent,
            last_activity_unix_ms: None,
        };
        assert_eq!(
            triage_order(vec![
                row(PaneStatus::Idle, None),
                row(PaneStatus::Done { exit: 2 }, None),
                row(PaneStatus::Asked, None),
                row(PaneStatus::Blocked, Some(URGENT)),
            ]),
            [3, 2, 1, 0]
        );
    }
}
