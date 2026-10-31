//! Assembling the blob: panes, agent sessions, wire ids.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

impl ServerState {
    /// Assemble the blob from the live tree plus pre-fetched handoffs,
    /// under the lock. Agent sessions come only from their cuts, so a blob
    /// assembled with no handoffs is the tree's identity alone: a busy
    /// agent's appends never make it look changed.
    #[allow(clippy::too_many_lines, reason = "one pass per carried entity kind")]
    pub(crate) fn assemble_upgrade_blob(
        &self,
        listener_fd: RawFd,
        handoffs: &UpgradeHandoffs,
    ) -> StateBlob {
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
                .filter_map(|tid| Some((tid, self.terminal_wire(tid)?)))
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
                .and_then(|t| self.terminal_wire(t))
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
                let retained = self.retained_exit(tid).map(exit_blob);
                let mut pane = pane_blob(
                    pane_wire,
                    window_wire,
                    desc,
                    &self.config.term,
                    handoffs.panes.get(&tid).filter(|_| retained.is_none()),
                    retained,
                    self.retain_request(tid),
                );
                pane.key = self.sessions.registry.resource_key(tid);
                panes.push(pane);
            }
        }

        let live_panes: HashSet<u32> = panes
            .iter()
            .filter(|pane| pane.retained_exit.is_none())
            .map(|pane| pane.wire_id)
            .collect();
        let agent_sessions = self.agent_session_blobs(&handoffs.agent_sessions, &live_panes);
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
            agent_sessions,
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
            native_agents: Vec::new(),
        }
    }

    /// One [`AgentSessionBlob`] per cut whose session is still registered
    /// under a pane that crosses live, in wire-id order. A session that
    /// closed, or whose parent stopped crossing, since its cut is dropped.
    pub(in crate::state) fn agent_session_blobs(
        &self,
        cuts: &HashMap<ResourceId, AgentSessionBootstrap>,
        live_panes: &HashSet<u32>,
    ) -> Vec<AgentSessionBlob> {
        let mut blobs: Vec<AgentSessionBlob> = cuts
            .iter()
            .filter_map(|(&id, cut)| {
                let desc = self.sessions.registry.resource(id)?;
                let facet = desc.agent.as_ref()?;
                let parent_wire_id = self.terminal_wire(desc.parent?)?;
                if !live_panes.contains(&parent_wire_id) {
                    return None;
                }
                Some(AgentSessionBlob {
                    wire_id: self.terminal_wire(id)?,
                    parent_wire_id,
                    provider: facet.provider.clone(),
                    native_id: facet.native_id.clone(),
                    state: facet.state.clone(),
                    base_seq: cut.base_seq,
                    dropped: cut.dropped,
                    ended: cut.ended,
                    // Stamped records are canonical JSON, so always UTF-8.
                    records: cut
                        .records
                        .iter()
                        .map(|record| String::from_utf8_lossy(record).into_owned())
                        .collect(),
                    key: Some(desc.key),
                    native_publications: cut.native_publications.clone(),
                    native_publication_high_water: cut.native_publication_high_water,
                })
            })
            .collect();
        blobs.sort_by_key(|blob| blob.wire_id);
        blobs
    }

    /// Ask one pane's actor for its upgrade handoff. `None` when the pane has
    /// no registered handle or the actor has gone away.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: the server runs on a LocalSet; pane actors are reached through !Send local futures"
    )]
    pub(in crate::state) async fn request_pane_handoff(
        &self,
        tid: ResourceId,
    ) -> Option<PaneUpgradeHandle> {
        let terminal = self.resource_handle(tid)?.terminal().ok()?.clone();
        let (reply, rx) = oneshot::channel();
        terminal
            .upgrade
            .send(UpgradeHandleRequest { reply })
            .await
            .ok()?;
        rx.await.ok()
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

    pub(in crate::state) fn terminal_wire(&self, tid: ResourceId) -> Option<u32> {
        self.idspace
            .terminal_wire(tid)
            .and_then(phux_protocol::ResourceId::local_id)
    }
}
