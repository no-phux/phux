//! `AgentSession` resources as the chrome sees them: one row per session,
//! grouped under the Terminal-kind pane it is bound to.
//!
//! The session kernel owns the record stream and the state folded from it;
//! this module is the plain-data projection the sidebar's attention queue
//! and the fleet dashboard consume. An `AgentSession` never has a pane slot or
//! a layout leaf, so its click target is always its parent pane.

use std::collections::HashMap;

use phux_client_core::session::agent_stream::AgentSessionStatus;
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

/// Project every live `AgentSession` the kernel holds onto its parent pane.
///
/// A session whose stream ended (`session_end`) is retracted and produces no
/// row; a session with no declared parent cannot be placed under a pane and
/// is skipped as well. Rows under one parent hold a stable order by resource
/// id so repeated projections compare equal.
pub(super) fn agent_session_rows(kernel: &AttachKernel) -> AgentSessionRows {
    let mut rows: AgentSessionRows = HashMap::new();
    for view in kernel.agent_sessions() {
        let Some(parent) = view.parent else { continue };
        let Some(state) = meta_state(view.state.status) else {
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

/// Map a stream-derived status onto the chrome's state vocabulary. `None`
/// means the session is retracted and must not be shown.
pub(super) const fn meta_state(status: AgentSessionStatus) -> Option<AgentMetaState> {
    Some(match status {
        AgentSessionStatus::Unknown => AgentMetaState::Unknown,
        AgentSessionStatus::Idle => AgentMetaState::Idle,
        AgentSessionStatus::Working => AgentMetaState::Working,
        AgentSessionStatus::Blocked => AgentMetaState::Blocked,
        AgentSessionStatus::Done => AgentMetaState::Done,
        AgentSessionStatus::Ended => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ended_sessions_are_retracted() {
        assert_eq!(meta_state(AgentSessionStatus::Ended), None);
        assert_eq!(
            meta_state(AgentSessionStatus::Blocked),
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
