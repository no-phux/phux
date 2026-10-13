//! The [`Registry`]: the in-memory index over one authority's [`Org`].
//!
//! It owns the organization records and maps each record key to a live
//! slotmap handle ([`SessionId`], [`ResourceId`]) that the server and its
//! wire bridge use. Records change only through [`Registry::commit`] of a
//! [`Transition`] produced by [`crate::apply::apply`]; live facets (Terminal
//! geometry, agent activity) are edited in place through
//! [`Registry::resource_mut`].
//!
//! Membership lives on the session record. A resource's session is
//! [`Registry::session_of`]; there is no window layer and no removal
//! cascade outside what `apply` puts in a transition.

use std::collections::HashMap;
use std::path::PathBuf;

use slotmap::SlotMap;

use crate::activation::Stamp;
use crate::actor::Actor;
use crate::apply::apply;
use crate::command::{Applied, Command, Refusal};
use crate::ids::{ResourceId, SessionId};
use crate::keys::{FreshKeys, InstanceId, ResourceKey, SessionKey};
use crate::org::{Delta, Org, Transition};
use crate::resource::{AgentFacet, LaunchSpec, Resource, ResourceDescriptor, ResourceKind};
use crate::session::Session;
use crate::terminal::TerminalFacet;

/// Grid size a new Terminal descriptor starts at, `(cols, rows)`.
pub const DEFAULT_DIMS: (u16, u16) = (80, 24);

/// Handles a [`Registry::commit`] allocated or released.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Committed {
    /// Sessions that gained a handle.
    pub sessions_added: Vec<(SessionKey, SessionId)>,
    /// Sessions whose handle was released.
    pub sessions_removed: Vec<(SessionKey, SessionId)>,
    /// Resources that gained a handle.
    pub resources_added: Vec<(ResourceKey, ResourceId)>,
    /// Resources whose handle was released.
    pub resources_removed: Vec<(ResourceKey, ResourceId)>,
}

/// Owns one authority's records and their live handles.
///
/// Single-threaded and synchronous; the server wraps it behind its
/// event-loop boundary.
#[derive(Debug)]
pub struct Registry {
    org: Org,
    sessions: SlotMap<SessionId, SessionKey>,
    resources: SlotMap<ResourceId, ResourceDescriptor>,
    session_ids: HashMap<SessionKey, SessionId>,
    resource_ids: HashMap<ResourceKey, ResourceId>,
}

impl Registry {
    /// Index `org`, issuing a handle for every session and resource.
    #[must_use]
    pub fn new(org: Org) -> Self {
        let mut reg = Self {
            org,
            sessions: SlotMap::with_key(),
            resources: SlotMap::with_key(),
            session_ids: HashMap::new(),
            resource_ids: HashMap::new(),
        };
        let sessions: Vec<SessionKey> = reg.org.sessions().map(|s| s.key).collect();
        for key in sessions {
            reg.index_session(key);
        }
        let resources: Vec<Resource> = reg.org.resources().cloned().collect();
        for r in &resources {
            reg.index_resource(r);
        }
        for r in &resources {
            reg.link_parent(r);
        }
        reg
    }

    /// The organization records.
    #[must_use]
    pub const fn org(&self) -> &Org {
        &self.org
    }

    /// Set the current server run, at startup.
    pub const fn set_instance(&mut self, instance: InstanceId) {
        self.org.set_instance(instance);
    }

    /// Mirror a committed transition and keep handles in step: new records
    /// gain handles, deleted ones release them.
    pub fn commit(&mut self, transition: &Transition) -> Committed {
        self.org.commit(transition);
        let mut out = Committed::default();
        for delta in &transition.deltas {
            match delta {
                Delta::PutSession(s) if !self.session_ids.contains_key(&s.key) => {
                    out.sessions_added.push((s.key, self.index_session(s.key)));
                }
                Delta::DeleteSession(k) => {
                    if let Some(id) = self.session_ids.remove(k) {
                        self.sessions.remove(id);
                        out.sessions_removed.push((*k, id));
                    }
                }
                Delta::PutResource(r) if !self.resource_ids.contains_key(&r.key) => {
                    let id = self.index_resource(r);
                    self.link_parent(r);
                    out.resources_added.push((r.key, id));
                }
                Delta::RemoveResource(k) => {
                    if let Some(id) = self.resource_ids.remove(k) {
                        self.resources.remove(id);
                        for (_, child) in &mut self.resources {
                            if child.parent == Some(id) {
                                child.parent = None;
                            }
                        }
                        out.resources_removed.push((*k, id));
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// [`apply()`] then [`Self::commit`], for callers with no durable store
    /// between the two (tests, in-memory tools).
    ///
    /// # Errors
    ///
    /// The [`Refusal`] from [`apply()`]; nothing changes.
    pub fn execute(
        &mut self,
        cmd: &Command,
        actor: &Actor,
        now: Stamp,
        fresh: &mut dyn FreshKeys,
    ) -> Result<(Applied, Committed), Refusal> {
        let applied = apply(&self.org, cmd, actor, now, fresh)?;
        let committed = self.commit(&applied.transition);
        Ok((applied, committed))
    }

    fn index_session(&mut self, key: SessionKey) -> SessionId {
        let id = self.sessions.insert(key);
        self.session_ids.insert(key, id);
        id
    }

    fn index_resource(&mut self, r: &Resource) -> ResourceId {
        let cwd_default = self
            .org
            .session(&r.session)
            .and_then(|s| s.attrs.directory.clone())
            .unwrap_or_default();
        let (terminal, agent) = facets(r, cwd_default);
        let (key, kind) = (r.key, r.kind);
        let id = self.resources.insert_with_key(|id| ResourceDescriptor {
            id,
            key,
            kind,
            parent: None,
            terminal,
            agent,
        });
        self.resource_ids.insert(key, id);
        id
    }

    fn link_parent(&mut self, r: &Resource) {
        let parent = r
            .provenance
            .parent
            .and_then(|p| self.resource_ids.get(&p).copied());
        if let Some(d) = self
            .resource_ids
            .get(&r.key)
            .and_then(|id| self.resources.get_mut(*id))
        {
            d.parent = parent;
        }
    }

    // ---- handles -----------------------------------------------------------

    /// The live handle of a session record.
    #[must_use]
    pub fn session_id(&self, key: &SessionKey) -> Option<SessionId> {
        self.session_ids.get(key).copied()
    }

    /// The record key behind a session handle.
    #[must_use]
    pub fn session_key(&self, id: SessionId) -> Option<SessionKey> {
        self.sessions.get(id).copied()
    }

    /// The live handle of a resource record.
    #[must_use]
    pub fn resource_id(&self, key: &ResourceKey) -> Option<ResourceId> {
        self.resource_ids.get(key).copied()
    }

    /// The record key behind a resource handle.
    #[must_use]
    pub fn resource_key(&self, id: ResourceId) -> Option<ResourceKey> {
        self.resources.get(id).map(|d| d.key)
    }

    // ---- lookups -----------------------------------------------------------

    /// A session record by handle.
    #[must_use]
    pub fn session(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(id).and_then(|k| self.org.session(k))
    }

    /// A session handle by name.
    #[must_use]
    pub fn session_by_name(&self, name: &str) -> Option<SessionId> {
        self.org
            .session_by_name(name)
            .and_then(|s| self.session_id(&s.key))
    }

    /// Every session with its handle, in slotmap order (not creation order
    /// across remove and reinsert; sort on a field if it matters).
    pub fn sessions(&self) -> impl Iterator<Item = (SessionId, &Session)> + '_ {
        self.sessions
            .iter()
            .filter_map(|(id, k)| self.org.session(k).map(|s| (id, s)))
    }

    /// A live resource descriptor by handle.
    #[must_use]
    pub fn resource(&self, id: ResourceId) -> Option<&ResourceDescriptor> {
        self.resources.get(id)
    }

    /// Mutably borrow a live descriptor, for facet edits. Records are not
    /// reachable this way.
    #[must_use]
    pub fn resource_mut(&mut self, id: ResourceId) -> Option<&mut ResourceDescriptor> {
        self.resources.get_mut(id)
    }

    /// A resource record by handle.
    #[must_use]
    pub fn record(&self, id: ResourceId) -> Option<&Resource> {
        self.resources
            .get(id)
            .and_then(|d| self.org.resource(&d.key))
    }

    /// Every live descriptor with its handle, in slotmap order.
    pub fn resources(&self) -> impl Iterator<Item = (ResourceId, &ResourceDescriptor)> + '_ {
        self.resources.iter()
    }

    /// The Terminal facet of `id`; `None` if unknown or not a Terminal.
    #[must_use]
    pub fn terminal(&self, id: ResourceId) -> Option<&TerminalFacet> {
        self.resources
            .get(id)
            .and_then(ResourceDescriptor::terminal)
    }

    /// Mutably borrow the Terminal facet of `id`.
    #[must_use]
    pub fn terminal_mut(&mut self, id: ResourceId) -> Option<&mut TerminalFacet> {
        self.resources
            .get_mut(id)
            .and_then(ResourceDescriptor::terminal_mut)
    }

    /// The session `resource` is a member of.
    #[must_use]
    pub fn session_of(&self, resource: ResourceId) -> Option<SessionId> {
        self.record(resource)
            .and_then(|r| self.session_id(&r.session))
    }

    /// A session's members as handles, in membership order.
    #[must_use]
    pub fn members(&self, session: SessionId) -> Vec<ResourceId> {
        self.session(session)
            .map(|s| {
                s.members
                    .iter()
                    .filter_map(|k| self.resource_id(k))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The resources bound to `parent`, in slotmap order.
    #[must_use]
    pub fn children(&self, parent: ResourceId) -> Vec<ResourceId> {
        self.resources
            .iter()
            .filter(|(_, r)| r.parent == Some(parent))
            .map(|(id, _)| id)
            .collect()
    }

    /// Set a session's advisory most-recent resource (R14). Unversioned.
    /// Returns `false` when either handle is unknown or `resource` is not a
    /// member.
    pub fn set_recent(&mut self, session: SessionId, resource: Option<ResourceId>) -> bool {
        let Some(key) = self.session_key(session) else {
            return false;
        };
        let resource = match resource {
            Some(id) => match self.resource_key(id) {
                Some(k) => Some(k),
                None => return false,
            },
            None => None,
        };
        self.org.set_recent(&key, resource)
    }

    /// Drop receipts older than `before`, then the oldest beyond
    /// `keep_at_most` (see [`Org::prune_receipts`]). Returns the dropped
    /// operation ids.
    pub fn prune_receipts(
        &mut self,
        before: Stamp,
        keep_at_most: usize,
    ) -> Vec<crate::keys::OperationId> {
        self.org.prune_receipts(before, keep_at_most)
    }

    /// Number of sessions.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Number of Terminal-kind resources.
    #[must_use]
    pub fn terminal_count(&self) -> usize {
        self.resources
            .values()
            .filter(|r| r.kind == ResourceKind::Terminal)
            .count()
    }
}

/// The live facets a new descriptor starts with, from its record.
fn facets(r: &Resource, cwd_default: PathBuf) -> (Option<TerminalFacet>, Option<AgentFacet>) {
    match (r.kind, &r.launch) {
        (ResourceKind::Terminal, launch) => {
            let cwd = match launch {
                Some(LaunchSpec::Terminal(t)) => t.cwd.clone().unwrap_or(cwd_default),
                _ => cwd_default,
            };
            (
                Some(TerminalFacet {
                    dims: DEFAULT_DIMS,
                    cwd,
                    title: None,
                }),
                None,
            )
        }
        (ResourceKind::AgentSession, launch) => {
            let (provider, native_id) = match launch {
                Some(LaunchSpec::Agent(a)) => (a.provider.clone(), a.native_id.clone()),
                _ => (String::new(), None),
            };
            (
                None,
                Some(AgentFacet {
                    provider,
                    native_id,
                    state: None,
                }),
            )
        }
    }
}
