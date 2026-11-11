//! Every resource this connection streams, and the per-kind policy that
//! says how to subscribe it.
//!
//! One [`Subscription`] per resource id records its kind, the ways it came
//! to stream here ([`Origin`]), the `AgentSession` follow intent kept across
//! reconnects, and whether its stream already ended on this incarnation.
//! What differs by kind (the attach command, a geometry vote, a resume
//! cursor, the ATTACH barrier) is read from [`KindPolicy`], never from a
//! closed set of per-kind maps.

use std::collections::HashMap;

use phux_protocol::ids::{ResourceId, ResourceKind};
use phux_protocol::wire::frame::{Command, RolePolicy};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

use super::agents::AgentSubscription;

/// What differs, by resource kind, in how this connection streams it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct KindPolicy {
    /// The viewport sizes it: an attach and every viewport change send it
    /// `RESIZE_TERMINAL`.
    pub(super) votes_on_geometry: bool,
    /// Its `ATTACH_RESOURCE` carries the committed record cursor.
    pub(super) resumes_from_cursor: bool,
    /// The session `ATTACH` barrier waits for its READY.
    pub(super) joins_attach_barrier: bool,
}

impl KindPolicy {
    pub(super) const fn of(kind: ResourceKind) -> Self {
        match kind {
            ResourceKind::Terminal => Self {
                votes_on_geometry: true,
                resumes_from_cursor: false,
                joins_attach_barrier: true,
            },
            ResourceKind::AgentSession => Self {
                votes_on_geometry: false,
                resumes_from_cursor: true,
                joins_attach_barrier: false,
            },
            // Documents and kinds this build does not know: a stream with
            // no geometry and no cursor.
            _ => Self {
                votes_on_geometry: false,
                resumes_from_cursor: false,
                joins_attach_barrier: false,
            },
        }
    }

    /// The `ATTACH_RESOURCE` that subscribes a resource of this kind.
    pub(super) const fn attach_command(
        self,
        resource_id: ResourceId,
        role_policy: Option<RolePolicy>,
        after_seq: Option<u64>,
    ) -> Command {
        if self.resumes_from_cursor {
            Command::attach_agent_session(resource_id, role_policy, after_seq)
        } else {
            Command::attach_terminal(resource_id, role_policy)
        }
    }
}

/// The resources of `snapshot` whose kind joins the session `ATTACH` barrier.
pub(super) fn barrier_resources(snapshot: &SessionSnapshot) -> impl Iterator<Item = &ResourceInfo> {
    snapshot
        .resources
        .iter()
        .filter(|resource| KindPolicy::of(resource.kind).joins_attach_barrier)
}

/// One way a resource came to stream on this connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    /// Listed by the session `ATTACH`: it rides the session pumps.
    SessionAttach,
    /// This client's own `ATTACH_RESOURCE`.
    AttachResource,
    /// This client's spawn: the server pumps it to the spawner.
    OwnSpawn,
    /// An `AgentSession` declared to the kernel.
    AgentDeclared,
}

impl Origin {
    const fn bit(self) -> u8 {
        match self {
            Self::SessionAttach => 1,
            Self::AttachResource => 1 << 1,
            Self::OwnSpawn => 1 << 2,
            Self::AgentDeclared => 1 << 3,
        }
    }
}

/// The origins whose stream pump delivers a resource's frames here.
pub(super) const STREAMED: &[Origin] = &[
    Origin::SessionAttach,
    Origin::AttachResource,
    Origin::OwnSpawn,
];

/// The origins whose pump is the session's or a spawn's: a per-resource
/// attach beside one would double-pump output.
pub(super) const RIDES_SHARED_PUMP: &[Origin] = &[Origin::SessionAttach, Origin::OwnSpawn];

/// Every origin: the kernel holds lane state for the resource.
pub(super) const KERNEL_KNOWN: &[Origin] = &[
    Origin::SessionAttach,
    Origin::AttachResource,
    Origin::OwnSpawn,
    Origin::AgentDeclared,
];

/// One resource's subscription state on this control plane.
#[derive(Debug)]
pub(super) struct Subscription {
    kind: ResourceKind,
    origins: u8,
    /// `subscribe_agent_sessions` follows it; kept across reconnects until
    /// the server proves it closed.
    follow: Option<AgentSubscription>,
    /// Its stream ended on this server incarnation: a later close the
    /// server sends for it is not a pane closing.
    ended: bool,
}

impl Subscription {
    const fn new(kind: ResourceKind) -> Self {
        Self {
            kind,
            origins: 0,
            follow: None,
            ended: false,
        }
    }

    const fn is_empty(&self) -> bool {
        self.origins == 0 && self.follow.is_none() && !self.ended
    }

    const fn has(&self, origin: Origin) -> bool {
        self.origins & origin.bit() != 0
    }
}

/// Every resource's [`Subscription`], keyed by resource id.
#[derive(Debug, Default)]
pub(super) struct Subscriptions {
    entries: HashMap<ResourceId, Subscription>,
}

impl Subscriptions {
    fn entry(&mut self, resource_id: &ResourceId, kind: ResourceKind) -> &mut Subscription {
        let entry = self
            .entries
            .entry(resource_id.clone())
            .or_insert_with(|| Subscription::new(kind));
        entry.kind = kind;
        entry
    }

    fn prune(&mut self, resource_id: &ResourceId) {
        if self
            .entries
            .get(resource_id)
            .is_some_and(Subscription::is_empty)
        {
            self.entries.remove(resource_id);
        }
    }

    /// The kind recorded for a subscribed resource.
    pub(super) fn kind(&self, resource_id: &ResourceId) -> Option<ResourceKind> {
        self.entries.get(resource_id).map(|entry| entry.kind)
    }

    pub(super) fn has(&self, resource_id: &ResourceId, origin: Origin) -> bool {
        self.entries
            .get(resource_id)
            .is_some_and(|entry| entry.has(origin))
    }

    pub(super) fn has_any(&self, resource_id: &ResourceId, origins: &[Origin]) -> bool {
        self.entries
            .get(resource_id)
            .is_some_and(|entry| origins.iter().any(|origin| entry.has(*origin)))
    }

    /// Record `origin` for `resource_id`; `false` when it was already set.
    pub(super) fn insert(
        &mut self,
        resource_id: &ResourceId,
        kind: ResourceKind,
        origin: Origin,
    ) -> bool {
        let entry = self.entry(resource_id, kind);
        let added = !entry.has(origin);
        entry.origins |= origin.bit();
        added
    }

    /// Withdraw `origin`; `true` when it was set.
    pub(super) fn remove(&mut self, resource_id: &ResourceId, origin: Origin) -> bool {
        let Some(entry) = self.entries.get_mut(resource_id) else {
            return false;
        };
        let removed = entry.has(origin);
        entry.origins &= !origin.bit();
        self.prune(resource_id);
        removed
    }

    /// Withdraw every origin in `origins`; `true` when any was set.
    pub(super) fn remove_all(&mut self, resource_id: &ResourceId, origins: &[Origin]) -> bool {
        origins
            .iter()
            .fold(false, |any, origin| self.remove(resource_id, *origin) | any)
    }

    /// Withdraw `origin` from every resource.
    pub(super) fn clear(&mut self, origin: Origin) {
        for entry in self.entries.values_mut() {
            entry.origins &= !origin.bit();
        }
        self.entries.retain(|_, entry| !entry.is_empty());
    }

    /// Exactly `resource_ids` carry `origin` now.
    pub(super) fn replace(
        &mut self,
        origin: Origin,
        kind: ResourceKind,
        resource_ids: impl IntoIterator<Item = ResourceId>,
    ) {
        self.clear(origin);
        for resource_id in resource_ids {
            self.insert(&resource_id, kind, origin);
        }
    }

    /// The resources carrying `origin`, in no particular order.
    pub(super) fn ids(&self, origin: Origin) -> impl Iterator<Item = &ResourceId> + '_ {
        self.entries
            .iter()
            .filter(move |(_, entry)| entry.has(origin))
            .map(|(resource_id, _)| resource_id)
    }

    pub(super) fn follow(&self, resource_id: &ResourceId) -> Option<&AgentSubscription> {
        self.entries.get(resource_id)?.follow.as_ref()
    }

    pub(super) fn follow_mut(
        &mut self,
        resource_id: &ResourceId,
    ) -> Option<&mut AgentSubscription> {
        self.entries.get_mut(resource_id)?.follow.as_mut()
    }

    /// Follow `resource_id`, an `AgentSession`, across reconnects.
    pub(super) fn set_follow(&mut self, resource_id: &ResourceId, follow: AgentSubscription) {
        self.entry(resource_id, ResourceKind::AgentSession).follow = Some(follow);
    }

    pub(super) fn take_follow(&mut self, resource_id: &ResourceId) -> Option<AgentSubscription> {
        let follow = self.entries.get_mut(resource_id)?.follow.take();
        self.prune(resource_id);
        follow
    }

    /// Every followed `AgentSession`, in no particular order.
    pub(super) fn followed(&self) -> impl Iterator<Item = (&ResourceId, &AgentSubscription)> + '_ {
        self.entries
            .iter()
            .filter_map(|(resource_id, entry)| Some((resource_id, entry.follow.as_ref()?)))
    }

    /// Whether `resource_id`'s stream ended on this incarnation.
    pub(super) fn ended(&self, resource_id: &ResourceId) -> bool {
        self.entries
            .get(resource_id)
            .is_some_and(|entry| entry.ended)
    }

    pub(super) fn set_ended(&mut self, resource_id: &ResourceId, ended: bool) {
        if ended {
            self.entry(resource_id, ResourceKind::AgentSession).ended = true;
        } else if let Some(entry) = self.entries.get_mut(resource_id) {
            entry.ended = false;
            self.prune(resource_id);
        }
    }

    /// A new incarnation: no stream has ended on it yet.
    pub(super) fn clear_ended(&mut self) {
        for entry in self.entries.values_mut() {
            entry.ended = false;
        }
        self.entries.retain(|_, entry| !entry.is_empty());
    }
}

impl super::ControlPlane {
    /// The kind of `resource_id` as this connection knows it: the topology's
    /// listing first, then what it was subscribed as. An unlisted resource
    /// is taken for a Terminal, the kind every subscription began as.
    pub(super) fn resource_kind_of(&self, resource_id: &ResourceId) -> ResourceKind {
        self.topology
            .as_ref()
            .and_then(|topology| {
                topology
                    .resources
                    .iter()
                    .find(|resource| &resource.id == resource_id)
                    .map(|resource| resource.kind)
            })
            .or_else(|| self.subscriptions.kind(resource_id))
            .unwrap_or(ResourceKind::Terminal)
    }

    /// How `resource_id`'s kind streams on this connection.
    pub(super) fn kind_policy(&self, resource_id: &ResourceId) -> KindPolicy {
        KindPolicy::of(self.resource_kind_of(resource_id))
    }
}

#[cfg(test)]
mod tests;
