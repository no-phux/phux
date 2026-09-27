//! Per-pane agent ledgers: the pending-question detector (ADR-0046 §D) and
//! the `phux.agent/v1` record arbiter (§E). Both are dropped in the reap
//! cascade: the detector (keyed by core id) before the wire id retires, the
//! arbiter (keyed by wire id) after. Everything is `pub(super)` and sync.

use phux_core::ids::ResourceId;
use phux_protocol::ids::ResourceId as WireResourceId;

use crate::agent_asked::{AskedDetector, AskedPayload, AskedSource, AskedTransition};
use crate::agent_state::AgentRecordArbiter;

/// Both per-pane agent ledgers.
#[derive(Debug)]
pub(super) struct AgentState {
    /// Panes with an agent waiting on a human (`phux.agent.asked/v1`).
    asked: AskedDetector,
    /// Ownership of each Terminal's `phux.agent/v1` record.
    records: AgentRecordArbiter,
}

impl Default for AgentState {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentState {
    /// Build empty ledgers — no pane has a pending question and no record
    /// has an owner.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            asked: AskedDetector::default(),
            records: AgentRecordArbiter::default(),
        }
    }

    /// Record that `terminal`'s agent is asking, returning how that moved
    /// the pane's pending-question state.
    pub(super) fn report_asked(
        &mut self,
        terminal: ResourceId,
        source: AskedSource,
        payload: AskedPayload,
    ) -> AskedTransition {
        self.asked.report(terminal, source, payload)
    }

    /// Retract `source`'s question for `terminal` (only its own).
    pub(super) fn retract_asked(
        &mut self,
        terminal: ResourceId,
        source: AskedSource,
    ) -> Option<AskedPayload> {
        self.asked.retract(terminal, source)
    }

    /// The question `terminal`'s agent is currently waiting on, if any.
    #[cfg(test)]
    pub(super) fn current_asked(&self, terminal: ResourceId) -> Option<&AskedPayload> {
        self.asked.current(terminal)
    }

    /// Whether any ask source still holds on `terminal` (ADR-0136).
    pub(super) fn is_asked(&self, terminal: ResourceId) -> bool {
        self.asked.is_pending(terminal)
    }

    /// Drop a reaped pane's question (before its wire id retires).
    pub(super) fn clear_asked(&mut self, terminal: ResourceId) {
        self.asked.clear_terminal(terminal);
    }

    /// Read the `phux.agent/v1` record arbiter (ADR-0046 §E).
    pub(super) const fn records(&self) -> &AgentRecordArbiter {
        &self.records
    }

    /// Mutate the `phux.agent/v1` record arbiter (ADR-0046 §E).
    pub(super) const fn records_mut(&mut self) -> &mut AgentRecordArbiter {
        &mut self.records
    }

    /// Drop arbiter state for a retired wire id.
    pub(super) fn forget_record(&mut self, terminal: &WireResourceId) {
        self.records.forget(terminal);
    }
}
