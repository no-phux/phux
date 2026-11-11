//! Assembling the blob's tree: sessions, windows, panes, wire ids.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

impl ServerState {
    /// Assemble the blob's tree from the registry, under the lock: the
    /// tree's identity alone. Each app adds what its engines carry at the
    /// cut (`runtime::apps::UpgradeCut::carry`), so a busy engine never
    /// makes the tree look changed.
    #[allow(clippy::too_many_lines, reason = "one pass per tree level")]
    pub(crate) fn assemble_upgrade_blob(&self, listener_fd: RawFd) -> StateBlob {
        let mut sessions = Vec::new();
        let mut windows = Vec::new();
        let mut panes = Vec::new();

        let mut ordered: Vec<SessionId> = self
            .sessions
            .registry
            .sessions()
            .map(|(id, _)| id)
            .collect();
        ordered.sort_by_key(|sid| self.session_wire(*sid));
        for sid in ordered {
            let (Some(session), Some(session_wire)) =
                (self.sessions.registry.session(sid), self.session_wire(sid))
            else {
                continue;
            };
            let members: Vec<(ResourceId, u32)> = self
                .legacy_panes_of(sid)
                .into_iter()
                .filter_map(|tid| Some((tid, self.resource_wire(tid)?)))
                .collect();
            let window_wire = (!members.is_empty())
                .then(|| self.window_wire(sid))
                .flatten();
            sessions.push(SessionBlob {
                wire_id: session_wire,
                name: session.name.clone(),
                window_wire_ids: window_wire.into_iter().collect(),
                active_window: window_wire,
                created_at_unix_nanos: u128::from(session.created.as_millis()) * 1_000_000,
                last_touched: self.sessions.last_touched_at(sid),
                root: self.sessions.root(sid).cloned(),
                keep_empty: session.lifetime == SessionLifetime::Persistent,
                key: Some(session.key),
            });
            let Some(window_wire) = window_wire else {
                continue;
            };
            let active = self
                .sessions
                .default_pane_of(sid)
                .and_then(|t| self.resource_wire(t))
                .filter(|w| members.iter().any(|(_, m)| m == w))
                .or_else(|| members.first().map(|(_, w)| *w));
            windows.push(WindowBlob {
                wire_id: window_wire,
                session_wire_id: session_wire,
                pane_wire_ids: members.iter().map(|(_, w)| *w).collect(),
                active_resource: active,
                layout: None,
                last_cwd: self.sessions.last_cwd(sid).cloned(),
            });
            for (tid, pane_wire) in members {
                let Some(desc) = self.sessions.registry.terminal(tid) else {
                    continue;
                };
                // ADR-0124 §6: a retained pane's PTY does not cross; a live
                // pane's retention request does.
                let mut pane = pane_blob(
                    pane_wire,
                    window_wire,
                    desc,
                    &self.config.term,
                    self.retained_exit(tid).map(exit_blob),
                    self.retain_request(tid),
                );
                pane.key = self.sessions.registry.resource_key(tid);
                panes.push(pane);
            }
        }

        let org = self.org();

        StateBlob {
            version: BLOB_VERSION,
            listener_fd,
            counters: Counters {
                next_session_wire_id: self.idspace.next_session_wire(),
                next_terminal_wire_id: self.idspace.next_terminal_wire(),
                next_window_wire_id: self.idspace.next_window_wire(),
                next_touch_timestamp: self.sessions.next_touch_timestamp(),
                server_instance: Some(*self.idspace.instance().as_bytes()),
            },
            sessions,
            windows,
            panes,
            // What engines carry, each app adds at the cut.
            agent_sessions: Vec::new(),
            metadata: Vec::new(),
            org: Some(OrgBlob {
                authority: *org.authority(),
                // `recent` is advisory and moves with every keystroke; it does
                // not cross, so typing cannot make the tree look changed.
                sessions: org
                    .sessions()
                    .map(|session| phux_core::Session {
                        recent: None,
                        ..session.clone()
                    })
                    .collect(),
                resources: org.resources().cloned().collect(),
                arrangements: org.arrangements().cloned().collect(),
            }),
            // Carried by the upgrade itself, with descriptors it owns
            // (`runtime::delegation::carry_delegations`); not part of the
            // tree's identity.
            delegations: Vec::new(),
            // Each app's, like `agent_sessions`.
            native_agents: Vec::new(),
            documents: Vec::new(),
        }
    }

    pub(in crate::state) fn session_wire(&self, sid: SessionId) -> Option<u32> {
        self.idspace
            .session_wire(sid)
            .map(phux_protocol::SessionId::get)
    }

    pub(in crate::state) fn window_wire(&self, sid: SessionId) -> Option<u32> {
        self.idspace
            .window_wire(sid)
            .map(phux_protocol::WindowId::get)
    }

    pub(crate) fn resource_wire(&self, tid: ResourceId) -> Option<u32> {
        self.idspace
            .resource_wire(tid)
            .and_then(phux_protocol::ResourceId::local_id)
    }
}
