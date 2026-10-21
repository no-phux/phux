//! What a prompt asked to wait for, how it ended, and why it failed.

use std::time::Duration;

use super::{Delivery, Refusal};
use crate::agent_meta::{AgentMetaState, AgentRecord};
use crate::agent_wait::{AgentWaitResult, DepartureReason};
use crate::attach::AttachError;

/// What `--wait` was asked for.
#[derive(Debug, Clone)]
pub struct PromptWait {
    /// The states a post-submit transition must land in.
    pub targets: Vec<AgentMetaState>,
    /// Give up after this long. `None` waits forever.
    pub timeout: Option<Duration>,
    /// `GET_METADATA` poll-floor cadence, recovering an edge a dropped
    /// notification never delivered.
    pub poll_interval: Duration,
}

/// Everything one acknowledged submit produced.
#[derive(Debug, Clone)]
pub struct PromptOutcome {
    /// What the receipt attests.
    pub delivery: Delivery,
    /// Lowercase hex of the operation id, for correlation (ADR-0076 point 2).
    pub operation_id: String,
    /// The record the ownership check passed on, immediately before the
    /// submit.
    pub agent: AgentRecord,
    /// That record's state.
    pub pre_submit_state: AgentMetaState,
    /// How many submits it took (`>1` only after `RESOURCE_EXHAUSTED`).
    pub attempts: u32,
    /// Wall time spent on the submit leg.
    pub submit_ms: u64,
    /// The `--wait` result, when one was asked for.
    pub wait: Option<AgentWaitResult>,
    /// Whether the push half of the wait ended and the poll floor carried it.
    pub degraded_to_polling: bool,
}

impl PromptOutcome {
    /// Whether a post-submit transition into a target state was observed.
    /// Always `false` without `--wait`.
    #[must_use]
    pub fn transition_observed(&self) -> bool {
        self.wait.as_ref().is_some_and(AgentWaitResult::satisfied)
    }
}

/// Why an acknowledged submit did not produce a [`PromptOutcome`].
#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    /// Nothing was written and the identical batch cannot succeed. Exit 2.
    #[error("{0}")]
    Refused(Refusal),
    /// The lane never freed; nothing written, safe to re-run. Exit 1.
    #[error(
        "the pane's acknowledged input stayed busy across {attempts} attempts \
         ({budget_ms}ms): {message}"
    )]
    LaneBusy {
        /// How many submits were made, all under the same operation id.
        attempts: u32,
        /// The total backoff budget spent.
        budget_ms: u64,
        /// The server's last diagnostic.
        message: String,
        /// The operation id, in hex.
        operation_id: String,
    },
    /// Nothing was written for a non-lane reason; safe to resubmit. Exit 1.
    #[error("nothing was written (operation {operation_id}): {message}")]
    NotWritten {
        /// The operation id, in hex.
        operation_id: String,
        /// The server's diagnostic.
        message: String,
    },
    /// The Terminal is gone. Exit 1.
    #[error("terminal not found: {0}")]
    NotFound(String),
    /// Terminal: some, all, or none of the bytes reached the tty; any retry
    /// replays or duplicates. Read the pane instead. Exit 1.
    #[error("delivery unknown (operation {operation_id}): {message}")]
    DeliveryUnknown {
        /// The operation id, in hex — the only handle on what happened.
        operation_id: String,
        /// The server's diagnostic.
        message: String,
    },
    /// The pane's occupant changed while the batch was in flight, so the
    /// bytes went to an unknown occupant. Exit 1.
    #[error("the pane's occupant changed while the batch was in flight: {detail}")]
    OccupantChanged {
        /// What changed.
        detail: String,
        /// The operation id, in hex.
        operation_id: String,
        /// Whether the submit was acknowledged before the change was seen.
        delivery: Delivery,
    },
    /// The agent went away during `--wait`. Not success, not a timeout.
    #[error("the agent departed from '{}' ({})", .from.as_str(), .reason.as_str())]
    Departed {
        /// The state held before the departure.
        from: AgentMetaState,
        /// How it departed.
        reason: DepartureReason,
        /// The submit's own outcome, which succeeded.
        delivery: Delivery,
        /// The operation id, in hex.
        operation_id: String,
    },
    /// Transport or protocol failure.
    #[error(transparent)]
    Transport(#[from] AttachError),
}
