//! The session graph, projected from an `ATTACHED` or `GET_STATE` snapshot
//! and, from a server that advertises `ORGANIZATION`, enriched with the
//! organization replica's keys, revisions, lifetimes, members, arrangements,
//! and execution states ([`Topology::with_organization`]).

use std::collections::HashSet;

use phux_client_core::layout::wire::HandleMap;
use phux_protocol::ids::{
    ActivationId, ArrangementKey, AuthorityId, ResourceId, ResourceKey, ResourceKind, Revision,
    SessionId, SessionKey,
};
use phux_protocol::wire::frame::{ExecState, SessionLifetime};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

use super::organization::OrganizationReplica;

/// One session in the server's graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDescriptor {
    /// The session id (live handle).
    pub id: u32,
    /// The session name.
    pub name: String,
    /// Legacy windows the server lists for the session (one per non-empty
    /// session); arrangement groups are the tabs.
    pub window_count: u16,
    /// Clients attached to the session.
    pub attached_client_count: u16,
    /// The session survives its last member (`lifetime == persistent`).
    pub keep_empty: bool,
    /// Creation time, seconds since the Unix epoch.
    pub created_at_unix_secs: i64,
    /// The persistent key, when the server reports it.
    pub key: Option<SessionKey>,
    /// Whether it outlives its last member, when the server reports it.
    pub lifetime: Option<SessionLifetime>,
    /// The record's revision, from the organization replica.
    pub revision: Option<Revision>,
    /// Member resources: the replica's count, else those the snapshot lists.
    pub members: u32,
    /// Its arrangement, from the organization replica.
    pub arrangement: Option<ArrangementKey>,
    /// Whether it is imported work awaiting organization (Intake).
    pub awaiting_organization: bool,
}

/// One Terminal-kind resource, denormalized with its session so a consumer
/// needs no joins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneDescriptor {
    /// The terminal.
    pub terminal_id: ResourceId,
    /// The session it belongs to.
    pub session_id: u32,
    /// That session's name.
    pub session_name: String,
    /// The terminal's grid width.
    pub cols: u16,
    /// The terminal's grid height.
    pub rows: u16,
    /// The terminal's title, if known.
    pub title: Option<String>,
    /// The terminal's working directory, if known.
    pub cwd: Option<String>,
    /// Whether this is the attaching client's initial focus.
    pub is_focused: bool,
    /// The persistent key, from the organization replica.
    pub key: Option<ResourceKey>,
    /// The record's revision, from the organization replica.
    pub revision: Option<Revision>,
    /// The record's display name, from the organization replica.
    pub name: Option<String>,
    /// The current activation, from the organization replica.
    pub activation: Option<ActivationId>,
    /// The current activation's state, from the organization replica.
    pub exec: Option<ExecState>,
}

/// One `AgentSession` resource: a record stream bound to a Terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionDescriptor {
    /// The resource.
    pub id: ResourceId,
    /// The Terminal it is bound to.
    pub parent: Option<ResourceId>,
    /// The provider slug, if declared.
    pub provider: Option<String>,
    /// The provider's own session id, if declared.
    pub native_id: Option<String>,
    /// The server-derived state word, if declared.
    pub state: Option<String>,
}

/// The session/pane graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    /// Every session the server listed.
    pub sessions: Vec<SessionDescriptor>,
    /// Every Terminal-kind resource, in the server's order.
    pub panes: Vec<PaneDescriptor>,
    /// Every `AgentSession` resource.
    pub agent_sessions: Vec<AgentSessionDescriptor>,
    /// The focused session.
    pub focused_session: u32,
    /// The focused terminal.
    pub focused_pane: ResourceId,
    /// The authority the records belong to, from the organization replica.
    pub authority: Option<AuthorityId>,
    /// The designated Intake session; legacy field name retained for consumers.
    pub inbox: Option<SessionKey>,
    /// Resource keys to live handles, from the organization replica.
    pub handles: HandleMap,
}

impl Topology {
    /// The designated Intake session; the legacy public field remains compatible.
    #[must_use]
    pub const fn intake(&self) -> Option<SessionKey> {
        self.inbox
    }

    /// Project a wire snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: &SessionSnapshot) -> Self {
        let sessions = snapshot
            .sessions
            .iter()
            .map(|session| SessionDescriptor {
                id: session.id.get(),
                name: session.name.clone(),
                window_count: session.window_count,
                attached_client_count: session.attached_client_count,
                keep_empty: session.keep_empty,
                created_at_unix_secs: session.created_at_unix_secs,
                key: session.key,
                lifetime: session.lifetime,
                revision: None,
                members: u32::try_from(
                    snapshot
                        .resources
                        .iter()
                        .filter(|info| session_of(snapshot, info) == Some(session.id))
                        .count(),
                )
                .unwrap_or(u32::MAX),
                arrangement: None,
                awaiting_organization: false,
            })
            .collect();
        let panes = terminal_resources(snapshot)
            .map(|pane| {
                let session_id = session_of(snapshot, pane);
                let session = snapshot
                    .sessions
                    .iter()
                    .find(|session| Some(session.id) == session_id);
                PaneDescriptor {
                    terminal_id: pane.id.clone(),
                    session_id: session_id.map_or(0, SessionId::get),
                    session_name: session.map_or_else(String::new, |s| s.name.clone()),
                    cols: pane.cols,
                    rows: pane.rows,
                    title: pane.title.clone(),
                    cwd: pane.cwd.clone(),
                    is_focused: pane.id == snapshot.focused_resource,
                    key: None,
                    revision: None,
                    name: None,
                    activation: None,
                    exec: None,
                }
            })
            .collect();
        let agent_sessions = snapshot
            .resources
            .iter()
            .filter(|resource| resource.kind == ResourceKind::AgentSession)
            .map(|resource| AgentSessionDescriptor {
                id: resource.id.clone(),
                parent: resource.parent.clone(),
                provider: resource.agent.as_ref().map(|facet| facet.provider.clone()),
                native_id: resource
                    .agent
                    .as_ref()
                    .and_then(|facet| facet.native_id.clone()),
                state: resource.agent.as_ref().map(|facet| facet.state.clone()),
            })
            .collect();
        Self {
            sessions,
            panes,
            agent_sessions,
            focused_session: snapshot.focused_session.get(),
            focused_pane: snapshot.focused_resource.clone(),
            authority: None,
            inbox: None,
            handles: HandleMap::new(),
        }
    }

    /// Enrich with the organization replica: keys, revisions, lifetimes,
    /// member counts, arrangements, intake, and execution states. A replica
    /// that is not live leaves the snapshot projection as it is.
    #[must_use]
    pub fn with_organization(mut self, replica: &OrganizationReplica) -> Self {
        if replica.authority().is_none() {
            return self;
        }
        self.authority = replica.authority();
        self.inbox = replica.intake();
        self.handles = replica.handles().clone();
        for session in &mut self.sessions {
            let record = session
                .key
                .and_then(|key| replica.session(&key))
                .or_else(|| replica.session_by_live(SessionId::new(session.id)));
            if let Some(record) = record {
                session.name.clone_from(&record.name);
                session.key = Some(record.key);
                session.lifetime = Some(record.lifetime);
                session.keep_empty = record.lifetime == SessionLifetime::Persistent;
                session.revision = Some(record.revision);
                session.members = record.members;
                session.arrangement = record.arrangement;
                session.awaiting_organization = record.awaiting_organization;
            }
        }
        for pane in &mut self.panes {
            if let Some(record) = replica.resource_by_live(&pane.terminal_id) {
                if let Some(session) = replica.session(&record.session)
                    && let Some(live) = session.live
                {
                    pane.session_id = live.get();
                    pane.session_name.clone_from(&session.name);
                }
                pane.key = Some(record.key);
                pane.revision = Some(record.revision);
                pane.name.clone_from(&record.name);
                pane.activation = record.activation.map(|a| a.id);
                pane.exec = record.activation.map(|a| a.state);
            }
        }
        self
    }

    /// The pane entry for `terminal_id`.
    #[must_use]
    pub fn pane(&self, terminal_id: &ResourceId) -> Option<&PaneDescriptor> {
        self.panes
            .iter()
            .find(|pane| &pane.terminal_id == terminal_id)
    }

    pub(super) fn pane_mut(&mut self, terminal_id: &ResourceId) -> Option<&mut PaneDescriptor> {
        self.panes
            .iter_mut()
            .find(|pane| &pane.terminal_id == terminal_id)
    }

    /// The session entry named `name`.
    #[must_use]
    pub fn session_named(&self, name: &str) -> Option<&SessionDescriptor> {
        self.sessions.iter().find(|session| session.name == name)
    }

    /// The session entry keyed `key`.
    #[must_use]
    pub fn session_keyed(&self, key: &SessionKey) -> Option<&SessionDescriptor> {
        self.sessions
            .iter()
            .find(|session| session.key == Some(*key))
    }

    /// The first live pane of session `session_id`, in the server's order.
    #[must_use]
    pub fn first_pane_of(&self, session_id: u32) -> Option<&ResourceId> {
        self.panes
            .iter()
            .find(|pane| pane.session_id == session_id)
            .map(|pane| &pane.terminal_id)
    }
}

/// The session a snapshot resource belongs to: its `session` facet, or from
/// a server that predates it, the session of its legacy window.
pub(super) fn session_of(snapshot: &SessionSnapshot, info: &ResourceInfo) -> Option<SessionId> {
    info.session.or_else(|| {
        snapshot
            .windows
            .iter()
            .find(|window| window.id == info.window_id)
            .map(|window| window.session_id)
    })
}

/// Which answer a snapshot is. Each is a view filtered by its own grant verb
/// (`ATTACHED` by `OBSERVE`, `GET_STATE` by `INVENTORY`, workload-auth §6),
/// and only a hub's `GET_STATE` lists satellite terminals (L1 §9.1).
#[derive(Debug, Clone, Copy)]
pub(super) enum View {
    Attach,
    Inventory,
}

/// The Terminals each view last listed. A snapshot is a view, not a death
/// certificate: absence proves a close only against the same view's
/// previous listing, and `RESOURCE_CLOSED` is the authority otherwise.
/// Kept across reconnects, so a close missed while disconnected is found.
#[derive(Debug, Default)]
pub(super) struct Listings {
    attach: HashSet<ResourceId>,
    inventory: HashSet<ResourceId>,
}

impl Listings {
    /// Record `snapshot` as `view`'s listing and return the Terminals it
    /// proves closed: listed by that view before and absent now. A satellite
    /// terminal is proven closed only while its hub reports the host
    /// reachable; until then it stays in the listing, unknown.
    pub(super) fn relist(&mut self, view: View, snapshot: &SessionSnapshot) -> Vec<ResourceId> {
        let now: HashSet<ResourceId> = terminal_resources(snapshot)
            .map(|pane| pane.id.clone())
            .collect();
        let previous = std::mem::replace(self.listing(view), now);
        let mut closed = Vec::new();
        for id in previous {
            if self.listing(view).contains(&id) {
                continue;
            }
            if unlisted_host(snapshot, &id) {
                self.listing(view).insert(id);
            } else {
                closed.push(id);
            }
        }
        closed
    }

    /// Whether `snapshot`, as `view`, omits a Terminal the other view lists.
    pub(super) fn hides(&self, view: View, snapshot: &SessionSnapshot) -> bool {
        let other = match view {
            View::Attach => &self.inventory,
            View::Inventory => &self.attach,
        };
        other
            .iter()
            .any(|id| !terminal_resources(snapshot).any(|pane| &pane.id == id))
    }

    /// A close already applied needs no proof from either view.
    pub(super) fn forget(&mut self, id: &ResourceId) {
        self.attach.remove(id);
        self.inventory.remove(id);
    }

    const fn listing(&mut self, view: View) -> &mut HashSet<ResourceId> {
        match view {
            View::Attach => &mut self.attach,
            View::Inventory => &mut self.inventory,
        }
    }
}

/// A satellite terminal whose host the snapshot does not report reachable:
/// its absence says nothing about the terminal (L1 §9.1 degradation).
fn unlisted_host(snapshot: &SessionSnapshot, id: &ResourceId) -> bool {
    let ResourceId::Satellite { host, .. } = id else {
        return false;
    };
    !snapshot
        .hosts()
        .iter()
        .any(|row| &row.host == host && row.is_reachable())
}

/// The Terminal-kind entries of a snapshot's inventory. Skipping every other
/// kind is a client obligation (ADR-0102): an agent session has no window,
/// grid, or PTY, so projecting one as a pane would paint a row no input can
/// reach.
pub(super) fn terminal_resources(
    snapshot: &SessionSnapshot,
) -> impl Iterator<Item = &ResourceInfo> {
    snapshot
        .resources
        .iter()
        .filter(|resource| resource.kind == ResourceKind::Terminal)
}
