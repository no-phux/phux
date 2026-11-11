//! Per-resource agent ledgers, shared by the apps that host agents: the
//! pending-question detector (ADR-0046 §D), keyed by core id, and the
//! `phux.agent/v1` record arbiter (§E), keyed by wire id. They live in an
//! app slot of [`ServerState`], which drops both when a resource is reaped.

use std::sync::LazyLock;

use phux_core::ids::ResourceId;
use phux_protocol::ids::ResourceId as WireResourceId;

use phux_agent_session::asked::{AskedDetector, AskedPayload, AskedSource, AskedTransition};
use phux_agent_session::state::AgentRecordArbiter;

use crate::state::{AppSlot, ServerState};

/// Both per-resource agent ledgers.
#[derive(Debug, Default)]
struct AgentState {
    /// Resources with an agent waiting on a human (`phux.agent.asked/v1`).
    asked: AskedDetector,
    /// Ownership of each resource's `phux.agent/v1` record.
    records: AgentRecordArbiter,
}

impl AppSlot for AgentState {
    fn forget_resource(&mut self, resource: ResourceId, wire: Option<&WireResourceId>) {
        self.asked.clear_terminal(resource);
        // A recycled wire id must not inherit a stale declaration.
        if let Some(wire) = wire {
            self.records.forget(wire);
        }
    }
}

/// The arbiter before any record had an owner.
static NO_RECORDS: LazyLock<AgentRecordArbiter> = LazyLock::new(AgentRecordArbiter::default);

impl ServerState {
    pub(crate) fn report_agent_asked(
        &mut self,
        terminal: ResourceId,
        source: AskedSource,
        payload: AskedPayload,
    ) -> AskedTransition {
        self.app_slot_mut::<AgentState>()
            .asked
            .report(terminal, source, payload)
    }

    /// Report a question from an `AgentSession` child's stream (ADR-0103 §5),
    /// named so callers cannot pass the wrong source.
    #[allow(
        dead_code,
        reason = "called by the AgentSession engine's record-to-ask mapping, which lands with that engine"
    )]
    pub(crate) fn report_stream_ask(
        &mut self,
        terminal: ResourceId,
        payload: AskedPayload,
    ) -> AskedTransition {
        self.report_agent_asked(terminal, AskedSource::Stream, payload)
    }

    /// Retract `source`'s question for `terminal` (only its own).
    pub(crate) fn retract_agent_asked(
        &mut self,
        terminal: ResourceId,
        source: AskedSource,
    ) -> Option<AskedPayload> {
        self.app_slot_mut::<AgentState>()
            .asked
            .retract(terminal, source)
    }

    /// The question `terminal`'s agent is currently waiting on, if any.
    #[cfg(test)]
    pub(crate) fn current_agent_asked(&self, terminal: ResourceId) -> Option<&AskedPayload> {
        self.app_slot::<AgentState>()?.asked.current(terminal)
    }

    /// Whether any ask source still holds on `terminal` (ADR-0136).
    pub(crate) fn agent_is_asked(&self, terminal: ResourceId) -> bool {
        self.app_slot::<AgentState>()
            .is_some_and(|agent| agent.asked.is_pending(terminal))
    }

    /// Read the `phux.agent/v1` record arbiter (ADR-0046 §E).
    pub(crate) fn agent_records(&self) -> &AgentRecordArbiter {
        self.app_slot::<AgentState>()
            .map_or(&*NO_RECORDS, |agent| &agent.records)
    }

    /// Mutate the `phux.agent/v1` record arbiter (ADR-0046 §E).
    pub(crate) fn agent_records_mut(&mut self) -> &mut AgentRecordArbiter {
        &mut self.app_slot_mut::<AgentState>().records
    }
}
