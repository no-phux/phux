use phux_core::ids::TerminalId;

use super::ServerState;
use crate::agent_asked::{AskedPayload, AskedSource, AskedTransition};

impl ServerState {
    pub(crate) fn report_agent_asked(
        &mut self,
        terminal: TerminalId,
        source: AskedSource,
        payload: AskedPayload,
    ) -> AskedTransition {
        self.agent.report_asked(terminal, source, payload)
    }

    #[cfg(test)]
    pub(crate) fn current_agent_asked(&self, terminal: TerminalId) -> Option<&AskedPayload> {
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
