use phux_core::ids::SessionId;

use super::{AttachSnapshotPane, ServerState};

impl ServerState {
    /// Panes in `session` with live handles, with their wire ids.
    #[must_use]
    pub fn attach_snapshot_panes(&mut self, session: SessionId) -> Vec<AttachSnapshotPane> {
        let mut panes = Vec::new();
        for pid in self.legacy_panes_of(session) {
            if let Some(handle) = self.resource_handle(pid).cloned() {
                panes.push(AttachSnapshotPane {
                    terminal_id: pid,
                    handle,
                    wire_terminal_id: self.intern_terminal_wire(pid),
                });
            }
        }
        panes
    }

    /// A hash of everything `phux workspace save` restores structurally:
    /// session ids, names, and lifetimes; membership and arrangement; each
    /// pane's cwd; each live agent session's identity; and the archived
    /// metadata keys (layout envelopes, agent-session records).
    /// The autosave probe (ADR-0150)
    /// compares it once a second; titles and sizes are left to its floor.
    /// Only equality within one process is meaningful.
    #[must_use]
    pub fn workspace_revision(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};

        let registry = &self.sessions.registry;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for (id, session) in registry.sessions() {
            id.hash(&mut hasher);
            session.name.hash(&mut hasher);
            session.lifetime.hash(&mut hasher);
            session.members.hash(&mut hasher);
            session.arrangement.hash(&mut hasher);
            for member in session
                .members
                .iter()
                .filter_map(|k| registry.resource_id(k))
            {
                registry.terminal(member).map(|t| &t.cwd).hash(&mut hasher);
            }
        }
        // A live `AgentSession` child is archived as a resume record
        // (ADR-0151): its identity counts, its busy/idle state does not.
        for (id, resource) in registry.resources() {
            if let Some(agent) = &resource.agent {
                id.hash(&mut hasher);
                resource.parent.hash(&mut hasher);
                agent.provider.hash(&mut hasher);
                agent.native_id.hash(&mut hasher);
            }
        }
        self.metadata.archived_revision().hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use phux_core::ids::ResourceId;
    use phux_core::resource::AgentFacet;
    use phux_protocol::wire::frame::{RESOURCE_AGENT_SESSION_KEY, Scope};

    use super::ServerState;

    #[test]
    fn workspace_revision_moves_with_topology_and_archived_metadata_only() {
        let mut state = ServerState::new();
        let empty = state.workspace_revision();
        assert_eq!(empty, state.workspace_revision(), "stable when idle");

        let _ = state.seed_empty_session("work").expect("work");
        let seeded = state.workspace_revision();
        assert_ne!(seeded, empty, "a new session");

        let _ = state.rename_session("work", "play");
        let renamed = state.workspace_revision();
        assert_ne!(renamed, seeded, "a rename");

        // Hot, unarchived keys (agent state) do not move it.
        let _ = state.metadata_set(&Scope::Global, "phux.agent/v1", b"busy".to_vec());
        assert_eq!(state.workspace_revision(), renamed);

        let _ = state.metadata_set(&Scope::Global, "phux.tui.layout/v1/1", b"{}".to_vec());
        let layout = state.workspace_revision();
        assert_ne!(layout, renamed, "a layout envelope write");

        let _ = state.metadata_set(&Scope::Global, RESOURCE_AGENT_SESSION_KEY, b"r".to_vec());
        assert_ne!(
            state.workspace_revision(),
            layout,
            "an agent-session record"
        );
    }

    /// Set a field of `agent`'s facet in place.
    fn edit_agent(state: &mut ServerState, agent: ResourceId, edit: impl FnOnce(&mut AgentFacet)) {
        let facet = state
            .registry_mut()
            .resource_mut(agent)
            .and_then(|resource| resource.agent.as_mut())
            .expect("agent facet");
        edit(facet);
    }

    #[test]
    fn workspace_revision_tracks_live_agent_identity_not_state() {
        let mut state = ServerState::new();
        let (_session, pane) = state.seed_session("main");
        let before = state.workspace_revision();
        let agent = state
            .new_agent_session(
                pane,
                &AgentFacet {
                    provider: "claude".to_owned(),
                    native_id: None,
                    state: None,
                },
            )
            .expect("agent session");
        let spawned = state.workspace_revision();
        assert_ne!(spawned, before, "a live agent session");

        edit_agent(&mut state, agent, |facet| {
            facet.state = Some("busy".to_owned());
        });
        assert_eq!(
            state.workspace_revision(),
            spawned,
            "busy/idle is not archived"
        );

        edit_agent(&mut state, agent, |facet| {
            facet.native_id = Some("conversation-1".to_owned());
        });
        assert_ne!(state.workspace_revision(), spawned, "a native id to resume");
    }
}
