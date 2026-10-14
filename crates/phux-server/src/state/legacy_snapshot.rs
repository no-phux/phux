//! The legacy `ATTACHED` / `GET_STATE` snapshot (R06): windows synthesized
//! from the windowless model for clients that still read them.
//!
//! Each session with at least one live Terminal reports exactly one window,
//! named `window-0`, whose wire id is interned from the session handle (so
//! it is stable across calls and carried through an upgrade) and whose panes
//! are the session's live Terminal members in membership order. An empty
//! session reports no window. `keep_empty` equals `lifetime == Persistent`.
//! A Terminal is listed while its engine is live or its activation is
//! current; a record whose execution ended with no engine left (ended by a
//! restart, awaiting `restore`) is not, since a legacy client could do
//! nothing with it. The designated Inbox is listed only while it holds such
//! a Terminal: older clients know nothing of it.
//!
//! Isolated here so it can be deleted with the translation layer.

use std::collections::HashMap;

use phux_core::ids::{ResourceId, SessionId};
use phux_core::resource::ResourceKind;
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

use super::ServerState;

impl ServerState {
    /// A session's Terminal members a legacy client can use, in membership
    /// order.
    pub(crate) fn legacy_panes_of(&self, session: SessionId) -> Vec<ResourceId> {
        self.sessions
            .terminal_members(session)
            .into_iter()
            .filter(|id| self.legacy_visible(*id))
            .collect()
    }

    /// The synthesized window's active pane: the session's `recent`
    /// member (R14), else its first listed one.
    fn legacy_active_pane(&self, session: SessionId, members: &[ResourceId]) -> Option<ResourceId> {
        self.sessions
            .registry
            .session(session)
            .and_then(|s| s.recent)
            .and_then(|key| self.sessions.registry.resource_id(&key))
            .filter(|p| members.contains(p))
            .or_else(|| members.first().copied())
    }

    /// Whether a legacy client sees `resource`: its engine is live, or its
    /// activation is current.
    fn legacy_visible(&self, resource: ResourceId) -> bool {
        self.resource_handle(resource).is_some()
            || self
                .sessions
                .registry
                .record(resource)
                .and_then(|r| r.activation.as_ref())
                .is_some_and(|a| a.state.is_current())
    }

    /// Build the `ATTACHED` [`SessionSnapshot`] (SPEC §13), interning wire
    /// ids as needed. Focus falls back to the session's default pane; an
    /// empty session carries `0` sentinels. `None` if `focus_session` is
    /// unknown.
    #[allow(clippy::too_many_lines)]
    pub fn build_session_snapshot(&mut self, focus_session: SessionId) -> Option<SessionSnapshot> {
        let mut attached_counts: HashMap<SessionId, u16> = HashMap::new();
        for c in self.clients.attached.values() {
            let count = attached_counts.entry(c.session).or_insert(0);
            *count = count.saturating_add(1);
        }

        let mut session_ids: Vec<(SessionId, phux_core::Stamp)> = self
            .sessions
            .registry
            .sessions()
            .map(|(id, s)| (id, s.created))
            .collect();
        // Creation order, so the listing does not shuffle with slotmap reuse.
        session_ids.sort_by_key(|(id, created)| (*created, self.idspace.session_wire(*id)));

        let mut sessions = Vec::with_capacity(session_ids.len());
        let mut windows = Vec::new();
        let mut panes = Vec::new();

        for (sid, _) in session_ids {
            let Some(record) = self.sessions.registry.session(sid).cloned() else {
                continue;
            };
            let members = self.legacy_panes_of(sid);
            if members.is_empty()
                && record.key == self.org().authority().inbox_session
                && sid != focus_session
            {
                continue;
            }
            let session_wire = self.idspace.intern_session(sid);
            let window_wire = (!members.is_empty()).then(|| self.intern_window_wire(sid));
            let created_at_unix_secs =
                i64::try_from(record.created.as_millis() / 1000).unwrap_or(i64::MAX);
            sessions.push(
                SessionInfo::new(session_wire, record.name.clone())
                    .with_active_window(window_wire)
                    .with_created_at_unix_secs(created_at_unix_secs)
                    .with_window_count(u16::from(window_wire.is_some()))
                    .with_attached_client_count(attached_counts.get(&sid).copied().unwrap_or(0))
                    .with_lifetime(Some(crate::org_wire::lifetime(record.lifetime)))
                    .with_key(phux_protocol::ids::SessionKey::new(*record.key.as_bytes())),
            );
            let Some(window_wire) = window_wire else {
                continue;
            };
            let active = self
                .legacy_active_pane(sid, &members)
                .map(|p| self.intern_terminal_wire(p));
            windows.push(
                WindowInfo::new(window_wire, session_wire, "window-0".to_owned())
                    .with_index(0)
                    .with_active_resource(active),
            );
            for pid in members {
                let Some(terminal) = self.sessions.registry.terminal(pid).cloned() else {
                    continue;
                };
                let terminal_wire = self.intern_terminal_wire(pid);
                let cwd =
                    Some(terminal.cwd.to_string_lossy().into_owned()).filter(|s| !s.is_empty());
                // ADR-0124: a retained pane reports how its process ended.
                let exit = self.retained_exit(pid);
                let lifecycle = if exit.is_some() {
                    phux_protocol::wire::frame::ResourceLifecycle::Exited
                } else {
                    phux_protocol::wire::frame::ResourceLifecycle::Running
                };
                // ADR-0033: the current lease holder, if any.
                let input_holder = self.input_lease_holder(pid).map(|holder| {
                    phux_protocol::ids::ClientId::new(u32::try_from(holder.0).unwrap_or(u32::MAX))
                });
                // ADR-0127: who watches without input.
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
                    ResourceInfo::new(terminal_wire, window_wire, terminal.dims.0, terminal.dims.1)
                        .with_title(terminal.title.clone())
                        .with_cwd(cwd)
                        .with_lifecycle(lifecycle)
                        .with_exit(exit)
                        .with_input_holder(input_holder)
                        .with_viewers(viewers)
                        .with_session(Some(session_wire)),
                );
            }
        }

        // Non-Terminal resources have no window (ADR-0102): listed after the
        // walk with `0 x 0`, `WindowId(0)`, their parent, and facet.
        let others: Vec<(ResourceId, phux_core::ResourceDescriptor)> = self
            .sessions
            .registry
            .resources()
            .filter(|(_, r)| r.kind != ResourceKind::Terminal)
            .map(|(id, r)| (id, r.clone()))
            .collect();
        for (id, descriptor) in others {
            if !self.legacy_visible(id) {
                continue;
            }
            let wire = self.intern_terminal_wire(id);
            let parent = descriptor.parent.map(|p| self.intern_terminal_wire(p));
            let session = self
                .sessions
                .registry
                .session_of(id)
                .map(|s| self.idspace.intern_session(s));
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
                    .with_agent(agent)
                    .with_session(session),
            );
        }

        self.sessions.registry.session(focus_session)?;
        let focused_session_wire = self.idspace.intern_session(focus_session);
        let members = self.legacy_panes_of(focus_session);
        let focus_pane = self.legacy_active_pane(focus_session, &members);
        // An empty session carries the `0` sentinels.
        let (focused_window_wire, focused_pane_wire) = focus_pane.map_or_else(
            || {
                (
                    phux_protocol::ids::WindowId::new(0),
                    phux_protocol::ids::ResourceId::local(0),
                )
            },
            |pane| {
                (
                    self.intern_window_wire(focus_session),
                    self.intern_terminal_wire(pane),
                )
            },
        );

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
}
