use std::collections::HashMap;

use phux_core::ids::SessionId;
use phux_core::session::Session;

use super::{AttachSnapshotPane, ServerState};

impl ServerState {
    /// Build the `ATTACHED` [`phux_protocol::wire::info::SessionSnapshot`]
    /// (SPEC §13), interning wire ids as needed. Focus falls back to the
    /// session's and window's active entries; a windowless keep-empty
    /// session carries `0` sentinels. `None` if `focus_session` is unknown.
    #[allow(clippy::too_many_lines)]
    pub fn build_session_snapshot(
        &mut self,
        focus_session: SessionId,
    ) -> Option<phux_protocol::wire::info::SessionSnapshot> {
        use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

        let attached_counts: HashMap<SessionId, u16> = {
            let mut counts: HashMap<SessionId, u16> = HashMap::new();
            for c in self.clients.attached.values() {
                *counts.entry(c.session).or_insert(0) = counts
                    .get(&c.session)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1);
            }
            counts
        };

        let session_pairs: Vec<(SessionId, Session)> = self
            .sessions
            .registry
            .sessions()
            .map(|(id, s)| (id, s.clone()))
            .collect();

        let mut sessions = Vec::with_capacity(session_pairs.len());
        let mut windows = Vec::new();
        let mut panes = Vec::new();

        for (sid, session) in &session_pairs {
            let session_wire = self.idspace.intern_session(*sid);
            // Pre-intern the active window so `active_window` round-trips.
            let active_window_wire = session.active.map(|w| self.intern_window_wire(w));

            let created_at_unix_secs = session
                .created_at
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            sessions.push(
                SessionInfo::new(session_wire, session.name.clone())
                    .with_active_window(active_window_wire)
                    .with_created_at_unix_secs(created_at_unix_secs)
                    .with_window_count(u16::try_from(session.windows.len()).unwrap_or(u16::MAX))
                    .with_attached_client_count(attached_counts.get(sid).copied().unwrap_or(0))
                    .with_keep_empty(session.keep_empty),
            );

            for (index, wid) in session.windows.iter().enumerate() {
                let Some(window) = self.sessions.registry.window(*wid).cloned() else {
                    continue;
                };
                let window_wire = self.intern_window_wire(*wid);
                let active_pane_wire = window.active.map(|p| self.intern_terminal_wire(p));

                // Layout is not mirrored on the wire.
                windows.push(
                    WindowInfo::new(window_wire, session_wire, format!("window-{index}"))
                        .with_index(u16::try_from(index).unwrap_or(u16::MAX))
                        .with_active_resource(active_pane_wire),
                );

                for pid in &window.slots {
                    let Some(terminal) = self.sessions.registry.terminal(*pid).cloned() else {
                        continue;
                    };
                    let terminal_wire = self.intern_terminal_wire(*pid);
                    let cwd =
                        Some(terminal.cwd.to_string_lossy().into_owned()).filter(|s| !s.is_empty());
                    // ADR-0124: a retained pane reports how its process
                    // ended; a live one adds nothing to the extension block.
                    let exit = self.retained_exit(*pid);
                    let lifecycle = if exit.is_some() {
                        phux_protocol::wire::frame::ResourceLifecycle::Exited
                    } else {
                        phux_protocol::wire::frame::ResourceLifecycle::Running
                    };
                    // ADR-0033: the current lease holder, if any.
                    let input_holder = self.input_lease_holder(*pid).map(|holder| {
                        phux_protocol::ids::ClientId::new(
                            u32::try_from(holder.0).unwrap_or(u32::MAX),
                        )
                    });
                    // ADR-0127: who watches without input, beside who holds
                    // the wheel.
                    let viewers = self
                        .terminal_viewers(&terminal_wire)
                        .into_iter()
                        .map(|viewer| {
                            phux_protocol::ids::ClientId::new(
                                u32::try_from(viewer.0).unwrap_or(u32::MAX),
                            )
                        })
                        .collect();
                    panes.push(
                        ResourceInfo::new(
                            terminal_wire,
                            window_wire,
                            terminal.dims.0,
                            terminal.dims.1,
                        )
                        .with_title(terminal.title.clone())
                        .with_cwd(cwd)
                        .with_lifecycle(lifecycle)
                        .with_exit(exit)
                        .with_input_holder(input_holder)
                        .with_viewers(viewers),
                    );
                }
            }
        }

        // Non-Terminal resources have no window (ADR-0102): listed after the
        // walk with `0 x 0`, `WindowId(0)`, their parent, and facet.
        for (id, descriptor) in self
            .sessions
            .registry
            .resources()
            .filter(|(_, r)| r.kind != phux_core::resource::ResourceKind::Terminal)
            .map(|(id, r)| (id, r.clone()))
            .collect::<Vec<_>>()
        {
            let wire = self.intern_terminal_wire(id);
            let parent = descriptor.parent.map(|p| self.intern_terminal_wire(p));
            let agent = descriptor.agent.as_ref().map(|facet| {
                phux_protocol::wire::info::AgentFacet::new(
                    facet.provider.clone(),
                    facet.state.clone().unwrap_or_else(|| "unknown".to_owned()),
                )
                .with_native_id(facet.native_id.clone())
            });
            panes.push(
                ResourceInfo::resource(wire, crate::resource::wire_kind(descriptor.kind))
                    .with_parent(parent)
                    .with_agent(agent),
            );
        }

        let session = self.sessions.registry.session(focus_session)?;
        let focus_pair = session.active.and_then(|window| {
            let pane = self.sessions.registry.window(window)?.active?;
            Some((window, pane))
        });

        let focused_session_wire = self.idspace.intern_session(focus_session);
        // Windowless keep-empty sessions carry the `0` sentinels.
        let (focused_window_wire, focused_pane_wire) = match focus_pair {
            Some((window, pane)) => (
                self.intern_window_wire(window),
                self.intern_terminal_wire(pane),
            ),
            None => (
                phux_protocol::ids::WindowId::new(0),
                phux_protocol::ids::ResourceId::local(0),
            ),
        };

        let mut snapshot =
            SessionSnapshot::new(focused_session_wire, focused_window_wire, focused_pane_wire)
                .with_sessions(sessions)
                .with_windows(windows)
                .with_resources(panes);
        if self.has_remote_listener_report() {
            snapshot = snapshot.with_listeners(self.remote_listeners().clone());
        }
        Some(snapshot)
    }

    /// Panes in `session` with live handles, with their wire ids.
    #[must_use]
    pub fn attach_snapshot_panes(&mut self, session: SessionId) -> Vec<AttachSnapshotPane> {
        let window_ids = self
            .sessions
            .registry
            .session(session)
            .map(|s| s.windows.clone())
            .unwrap_or_default();
        let mut panes = Vec::new();
        for wid in window_ids {
            let window_panes = self
                .sessions
                .registry
                .window(wid)
                .map(|w| w.slots.clone())
                .unwrap_or_default();
            for pid in window_panes {
                if let Some(handle) = self.resource_handle(pid).cloned() {
                    panes.push(AttachSnapshotPane {
                        terminal_id: pid,
                        handle,
                        wire_terminal_id: self.intern_terminal_wire(pid),
                    });
                }
            }
        }
        panes
    }

    /// A hash of everything `phux workspace save` restores structurally:
    /// session ids, names, and keep-empty marks; window order and focus;
    /// each pane's place and cwd; each live agent session's identity; and
    /// the archived metadata keys (layout envelopes, agent-session records).
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
            session.keep_empty.hash(&mut hasher);
            session.active.hash(&mut hasher);
            session.windows.hash(&mut hasher);
            for window in session.windows.iter().filter_map(|id| registry.window(*id)) {
                window.slots.hash(&mut hasher);
                window.active.hash(&mut hasher);
                for slot in &window.slots {
                    registry.terminal(*slot).map(|t| &t.cwd).hash(&mut hasher);
                }
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

        let _ = state.seed_empty_session("work");
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
        let (_session, _window, pane) = state.seed_session("main");
        let before = state.workspace_revision();
        let agent = state
            .registry_mut()
            .new_agent_session(
                pane,
                AgentFacet {
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
