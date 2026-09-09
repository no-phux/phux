use phux_core::ids::ResourceId;

use super::ServerState;
use crate::agent_asked::{AskedPayload, AskedSource, AskedTransition};

impl ServerState {
    pub(crate) fn report_agent_asked(
        &mut self,
        terminal: ResourceId,
        source: AskedSource,
        payload: AskedPayload,
    ) -> AskedTransition {
        self.agent.report_asked(terminal, source, payload)
    }

    /// Report a question an `AgentSession` child's stream carried
    /// (ADR-0103 decision 5).
    ///
    /// A thin, NAMED entry rather than one more `report_agent_asked(...,
    /// AskedSource::Stream, ...)` call site: the rung is the whole point, and
    /// a caller that has to name the source is a caller that can pass the
    /// wrong one. Retraction stays on the general
    /// [`Self::retract_agent_asked`] with [`AskedSource::Stream`], because
    /// per-source retraction is exactly the shipped rule and needs no variant
    /// of its own.
    #[allow(
        dead_code,
        reason = "called by the AgentSession engine's record-to-ask mapping, which lands with that engine"
    )]
    pub(crate) fn report_stream_ask(
        &mut self,
        terminal: ResourceId,
        payload: AskedPayload,
    ) -> AskedTransition {
        self.agent
            .report_asked(terminal, AskedSource::Stream, payload)
    }

    pub(crate) fn retract_agent_asked(
        &mut self,
        terminal: ResourceId,
        source: AskedSource,
    ) -> Option<AskedPayload> {
        self.agent.retract_asked(terminal, source)
    }

    #[cfg(test)]
    pub(crate) fn current_agent_asked(&self, terminal: ResourceId) -> Option<&AskedPayload> {
        self.agent.current_asked(terminal)
    }

    /// Read the `phux.agent/v1` record arbiter (ADR-0046 §E).
    pub(crate) const fn agent_records(&self) -> &crate::agent_state::AgentRecordArbiter {
        self.agent.records()
    }

    /// Mutate the `phux.agent/v1` record arbiter (ADR-0046 §E).
    pub(crate) const fn agent_records_mut(
        &mut self,
    ) -> &mut crate::agent_state::AgentRecordArbiter {
        self.agent.records_mut()
    }
}
