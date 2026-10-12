//! Attention grading and the triage order (ADR-0166).
//!
//! How urgent a pane is, and in what order a list of panes is shown, are
//! client policy rather than wire facts. They live here once so every mobile
//! client reads them the same way. This is a port of phux-mobile's
//! `AttentionGrade.swift` and `TerminalStore+Triage.swift`, and the shared
//! fixture `fixtures/attention.json` pins it.
//!
//! The grade follows three rules:
//!
//! 1. **Monotonicity.** Grading may raise urgency above what a client showed
//!    before grading existed, but never lower it. A waiting pane (`Asked` or
//!    `Blocked`) is floored at [`AttentionGrade::Normal`], whatever its record
//!    says. An ask hook fires as soon as a question appears, while the
//!    detector's record arrives about 300 ms later (ADR-0046). In that window
//!    a stale `idle` or `done` record derives `none` or `low`, and without the
//!    floor it would mute a live demand.
//! 2. **Attention sits beside status, never inside it.** The grade reads the
//!    pane status. A status fold must never read a grade.
//! 3. **Total.** Every input has a grade. A pane with no record grades
//!    `Normal`.
//!
//! The top grade, `High`, is for a record that *flags* itself: one that
//! declares a level above what its own state derives (L3 §3.7). Every
//! `blocked` record derives `high`, so a plain `high` describes an ordinary
//! permission prompt and is capped at `Normal`.

use std::cmp::Reverse;

use super::agent::{AgentAttention, AgentState, derived_attention};

/// A pane's folded status, as a client shows it on a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneStatus {
    /// An agent or process is running on its own.
    Working,
    /// An agent is waiting on an answer to a structured question.
    Asked,
    /// An agent is stuck without a structured question, for example on a
    /// permission prompt. It needs the user as much as `Asked` does.
    Blocked,
    /// Finished with this exit code.
    Done {
        /// The exit code; non-zero is a failure.
        exit: i32,
    },
    /// A dormant shell, or no known agent status.
    Idle,
}

impl PaneStatus {
    /// Whether a human is being waited on: `Asked` or `Blocked`.
    #[must_use]
    pub const fn is_waiting(self) -> bool {
        matches!(self, Self::Asked | Self::Blocked)
    }

    /// Whether the pane finished with a non-zero exit.
    #[must_use]
    pub const fn is_failure(self) -> bool {
        matches!(self, Self::Done { exit } if exit != 0)
    }
}

/// The two fields of a pane's `phux.agent/v1` badge that grading reads, as
/// [`super::agent::badge`] resolved them. `None` in place of this means the
/// pane has no record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeclaredAgent {
    /// The declared lifecycle word.
    pub state: AgentState,
    /// The effective attention: declared, or derived from `state`.
    pub attention: AgentAttention,
}

impl DeclaredAgent {
    /// Whether the record asks for more attention than its own state
    /// derives, meaning the agent went out of its way.
    ///
    /// A record that declares `blocked` with `high` reads the same as one
    /// that declared no level, so it is not flagged. Telling them apart would
    /// mean reading `AttentionReading`, which this port does not do.
    #[must_use]
    pub fn flags_urgency(self) -> bool {
        AttentionGrade::from(self.attention) > AttentionGrade::from(derived_attention(self.state))
    }
}

/// A pane's urgency, ordered from `None` to `High`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AttentionGrade {
    /// An explicit down-claim: the agent says it does not need a human.
    None,
    /// Low urgency.
    Low,
    /// The grade of a pane with no record, and the floor for every waiting
    /// pane.
    Normal,
    /// A record that flags itself ([`DeclaredAgent::flags_urgency`]).
    High,
}

impl From<AgentAttention> for AttentionGrade {
    fn from(value: AgentAttention) -> Self {
        match value {
            AgentAttention::None => Self::None,
            AgentAttention::Low => Self::Low,
            AgentAttention::Normal => Self::Normal,
            AgentAttention::High => Self::High,
        }
    }
}

/// Grade a pane. The steps run in this order:
///
/// 1. Start from the declared level (`Normal` with no record).
/// 2. Cap it at `Normal` unless the record flags itself.
/// 3. Floor it at `Normal` while the pane is waiting.
///
/// The floor runs last, so it always wins. No combination of status and
/// record grades a waiting pane below `Normal`.
#[must_use]
pub fn grade(status: PaneStatus, agent: Option<DeclaredAgent>) -> AttentionGrade {
    let declared = agent.map_or(AttentionGrade::Normal, |agent| agent.attention.into());
    let ceiling = if agent.is_some_and(DeclaredAgent::flags_urgency) {
        AttentionGrade::High
    } else {
        AttentionGrade::Normal
    };
    let floor = if status.is_waiting() {
        AttentionGrade::Normal
    } else {
        AttentionGrade::None
    };
    declared.min(ceiling).max(floor)
}

/// One row of a list to be triage-ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriagePane {
    /// The folded status.
    pub status: PaneStatus,
    /// The pane's record, or `None` when it declares no agent.
    pub agent: Option<DeclaredAgent>,
    /// When the pane last showed activity, in Unix milliseconds rounded down,
    /// or `None` when it never has.
    pub last_activity_unix_ms: Option<i64>,
}

/// Apple's reference date (2001-01-01T00:00:00Z) in Unix milliseconds.
///
/// Activity minutes are counted from it, as the Swift original counted from
/// `timeIntervalSinceReferenceDate`. It is a whole number of minutes after
/// the Unix epoch, so for any time after 2001 the minute boundaries match
/// plain Unix minutes.
pub const REFERENCE_DATE_UNIX_MS: i64 = 978_307_200_000;

/// The minute that triage ranks activity by.
///
/// Panes active in the same minute
/// keep their input order, so a list does not reorder on every line of
/// output. Returns `i64::MIN` for a pane with no activity. Division
/// truncates toward zero, as Swift's `Int64(_:)` does.
#[must_use]
pub const fn activity_minute(last_activity_unix_ms: Option<i64>) -> i64 {
    match last_activity_unix_ms {
        Some(unix_ms) => unix_ms.saturating_sub(REFERENCE_DATE_UNIX_MS) / 60_000,
        None => i64::MIN,
    }
}

/// The input indices of `panes` in triage order. The sort keys, in order:
///
/// 1. Band: waiting panes, then failures, then everything else.
/// 2. Inside the waiting band only, the grade, highest first. A grade never
///    moves a pane across bands.
/// 3. Activity minute, most recent first.
/// 4. Input index, so equal panes never reorder.
///
/// In practice step 2 is binary: a flagged pane, then every other waiting
/// pane by recency.
#[must_use]
pub fn triage_order(panes: &[TriagePane]) -> Vec<usize> {
    let mut keyed: Vec<_> = panes
        .iter()
        .enumerate()
        .map(|(index, pane)| {
            let (band, sub) = rank(pane);
            let minute = activity_minute(pane.last_activity_unix_ms);
            ((band, sub, Reverse(minute), index), index)
        })
        .collect();
    keyed.sort_unstable_by_key(|(key, _)| *key);
    keyed.into_iter().map(|(_, index)| index).collect()
}

/// `(band, sub-rank)`, both ascending. Panes that are not waiting take
/// sub-rank 0, so grading leaves the failure and resting bands untouched.
fn rank(pane: &TriagePane) -> (u8, u8) {
    if pane.status.is_waiting() {
        let grade = grade(pane.status, pane.agent);
        return (0, AttentionGrade::High as u8 - grade as u8);
    }
    if pane.status.is_failure() {
        return (1, 0);
    }
    (2, 0)
}

#[cfg(test)]
#[path = "attention/tests.rs"]
mod tests;
