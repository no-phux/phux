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

    /// Whether any ask source still holds on `terminal` (ADR-0136).
    pub(crate) fn agent_is_asked(&self, terminal: ResourceId) -> bool {
        self.agent.is_asked(terminal)
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
