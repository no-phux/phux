//! `AgentSession` resources as the chrome sees them: one row per session,
//! grouped under the pane it is bound to, which is always its click target.

use std::collections::HashMap;

use phux_protocol::ids::ResourceId;

use super::pane_state::AttachKernel;
use phux_client::agent_meta::AgentMetaState;

/// One `AgentSession` resource, keyed for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentSessionRow {
    /// The `AgentSession` resource id.
    pub id: ResourceId,
    /// Provider slug (`claude`, `codex`, ...) when the stream or facet named it.
    pub provider: Option<String>,
    /// Opaque provider session id, when known.
    pub native_id: Option<String>,
    /// Lifecycle state in the chrome's vocabulary.
    pub state: AgentMetaState,
}

impl AgentSessionRow {
    /// The display name: the provider, or a placeholder when the stream has
    /// not named one yet.
    pub(super) fn name(&self) -> &str {
        self.provider.as_deref().unwrap_or("agent")
    }
}

/// `AgentSession` rows grouped by their parent pane.
pub(super) type AgentSessionRows = HashMap<ResourceId, Vec<AgentSessionRow>>;

/// Project every live `AgentSession` onto its parent pane. Ended or
/// parentless sessions produce no row; rows per parent sort by id so repeated
/// projections compare equal.
pub(super) fn agent_session_rows(kernel: &AttachKernel) -> AgentSessionRows {
    let mut rows: AgentSessionRows = HashMap::new();
    for view in kernel.agent_sessions() {
        let Some(parent) = view.parent else { continue };
        let Some(state) = view.state.status.as_agent_state() else {
            continue;
        };
        rows.entry(parent.clone())
            .or_default()
            .push(AgentSessionRow {
                id: view.terminal_id.clone(),
                provider: view.state.provider.clone(),
                native_id: view.state.native_id.clone(),
                state,
            });
    }
    for siblings in rows.values_mut() {
        siblings.sort_by(|a, b| a.id.cmp(&b.id));
    }
    rows
}

#[cfg(test)]
mod tests {
    use phux_client_core::session::agent_stream::AgentSessionStatus;

    use super::*;

    #[test]
    fn ended_sessions_are_retracted() {
        assert_eq!(AgentSessionStatus::Ended.as_agent_state(), None);
        assert_eq!(
            AgentSessionStatus::Blocked.as_agent_state(),
            Some(AgentMetaState::Blocked)
        );
    }

    #[test]
    fn a_nameless_row_reads_as_agent() {
        let row = AgentSessionRow {
            id: ResourceId::local(7),
            provider: None,
            native_id: None,
            state: AgentMetaState::Working,
        };
        assert_eq!(row.name(), "agent");
    }
}
