//! Parent bindings and the close ledger (ADR-0104).
//!
//! A resource may name one immutable parent at spawn; when the parent
//! leaves, its children leave in the same lock acquisition, so no client
//! sees an orphan. The graph is the registry's ([`Registry::children`]);
//! this module records why each resource is closing. A closer records the
//! reason before cancelling, and the resource's exit watcher claims it:
//! [`ServerState::begin_resource_close`] returns `None` for an already-reaped
//! resource, so exactly one closer emits each `RESOURCE_CLOSED`. A kill
//! reaps in its own lock (L1 §5.2) while the process is still in its hangup
//! grace; the exit-watch ledger lets that process's watcher finish the rest.
//!
//! [`Registry::children`]: phux_core::registry::Registry::children

use std::collections::HashSet;

use phux_core::ids::ResourceId;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::CloseReason;

use super::{CloseAttribution, ServerState};

#[derive(Debug)]
pub(super) struct PendingClose {
    reason: CloseReason,
    attribution: CloseAttribution,
}

/// Exact set a close will cancel and reap, with descendants before parents.
pub(crate) struct ClosePlan(Vec<(ResourceId, CloseReason)>);

impl ClosePlan {
    pub(crate) fn resources(&self) -> impl ExactSizeIterator<Item = ResourceId> + '_ {
        self.0.iter().map(|(id, _)| *id)
    }

    pub(crate) fn locked_tab(&self, state: &ServerState) -> Option<String> {
        let keys: Vec<_> = self
            .resources()
            .filter_map(|id| state.registry().resource_key(id))
            .collect();
        phux_core::arrangement::locked_tab_closed_by(state.org(), &keys)
    }

    pub(crate) fn into_resources(self) -> impl Iterator<Item = ResourceId> {
        self.0.into_iter().map(|(id, _)| id)
    }
}

impl ServerState {
    /// `parent`'s descendants, breadth-first.
    #[must_use]
    pub fn resource_descendants(&self, parent: ResourceId) -> Vec<ResourceId> {
        let mut descendants = self.sessions.registry.children(parent);
        let mut index = 0;
        while index < descendants.len() {
            let current = descendants[index];
            for child in self.sessions.registry.children(current) {
                if !descendants.contains(&child) {
                    descendants.push(child);
                }
            }
            index += 1;
        }
        descendants
    }

    /// The parent `child` named at spawn; the close path must resolve it
    /// before the reap retires the binding.
    #[must_use]
    pub fn resource_parent(&self, child: ResourceId) -> Option<ResourceId> {
        self.sessions
            .registry
            .resource(child)
            .and_then(|resource| resource.parent)
    }

    /// Whether the resource `wire` names has a live `AgentSession` child;
    /// `false` for unknown or satellite ids.
    #[must_use]
    pub(crate) fn resource_has_live_session(&self, wire: &phux_protocol::ids::ResourceId) -> bool {
        self.idspace
            .resource_from_wire(wire)
            .is_some_and(|core| self.has_live_agent_session_child(core))
    }

    /// Whether `parent` has a live record-stream child, an `AgentSession`
    /// (the detector's ADR-0103 §5 query).
    #[must_use]
    pub fn has_live_agent_session_child(&self, parent: ResourceId) -> bool {
        self.sessions
            .registry
            .children(parent)
            .into_iter()
            .any(|child| {
                self.resources()
                    .handle(child)
                    .and_then(|handle| {
                        handle.companion::<crate::resource::companions::LiveStream>()
                    })
                    .is_some_and(crate::resource::companions::LiveStream::is_live)
            })
    }

    /// Preserve the first close reason and the first attribution supplied.
    pub(crate) fn mark_resource_closing(
        &mut self,
        resource: ResourceId,
        reason: CloseReason,
        attribution: CloseAttribution,
    ) {
        let close = self.pending_closes.entry(resource).or_insert(PendingClose {
            reason,
            attribution,
        });
        if close.attribution == CloseAttribution::default() {
            close.attribution = attribution;
        }
    }

    /// Take the facts for `RESOURCE_CLOSED` and `pane_closed` before reaping.
    /// Return `None` if already reaped. An unmarked resource closes as `Exited`.
    pub(crate) fn begin_resource_close(
        &mut self,
        resource: ResourceId,
    ) -> Option<(CloseReason, CloseAttribution)> {
        self.sessions.registry.resource(resource)?;
        Some(self.pending_closes.remove(&resource).map_or_else(
            || (CloseReason::Exited, CloseAttribution::default()),
            |close| (close.reason, close.attribution),
        ))
    }

    /// Why `resource` is leaving when a closer has marked it.
    #[must_use]
    pub fn pending_close_reason(&self, resource: ResourceId) -> Option<CloseReason> {
        self.pending_closes.get(&resource).map(|close| close.reason)
    }

    /// Close `targets` and every descendant in one borrow (ADR-0104 §2).
    /// Targets get `reason`, descendants `ParentClosed` unless also targeted.
    /// Only cancels; each exit watcher reaps and emits. Returns how many
    /// closed.
    pub fn close_resources(&mut self, targets: &[ResourceId], reason: CloseReason) -> u32 {
        self.close_resources_attributed(targets, reason, super::CloseAttribution::default())
    }

    /// [`Self::close_resources`], stamping every closed resource's
    /// `pane_closed` with `attribution`: the kill's connection and key.
    pub fn close_resources_attributed(
        &mut self,
        targets: &[ResourceId],
        reason: CloseReason,
        attribution: super::CloseAttribution,
    ) -> u32 {
        let count = self.mark_and_cancel(targets, reason, attribution);
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// Resolve the cascade before mutating state. An explicitly targeted
    /// descendant keeps the requested reason regardless of request order.
    pub(crate) fn plan_close(&self, targets: &[ResourceId], reason: CloseReason) -> ClosePlan {
        let explicit: HashSet<_> = targets.iter().copied().collect();
        let mut seen = HashSet::new();
        let mut closing = Vec::new();
        for &target in targets.iter().rev() {
            if seen.contains(&target) || self.registry().resource(target).is_none() {
                continue;
            }
            // Reversing the breadth-first descendants visits every descendant
            // before its parent. Previously visited subtrees are already
            // planned in that order, so overlapping requests need only one row per id.
            let subtree = self
                .resource_descendants(target)
                .into_iter()
                .rev()
                .chain(std::iter::once(target));
            for id in subtree.filter(|id| seen.insert(*id)) {
                let reason = if explicit.contains(&id) {
                    reason
                } else {
                    CloseReason::ParentClosed
                };
                closing.push((id, reason));
            }
        }
        ClosePlan(closing)
    }

    /// Record why each planned resource closes and cancel its actor. Reaping
    /// is the caller's responsibility or, for an asynchronous close, the
    /// exit watcher's.
    pub(crate) fn cancel_close_plan(
        &mut self,
        plan: &ClosePlan,
        attribution: super::CloseAttribution,
    ) {
        for &(resource, reason) in plan.0.iter().rev() {
            self.mark_resource_closing(resource, reason, attribution);
            self.resources_mut().detach_actor(resource);
        }
    }

    pub(crate) fn mark_and_cancel(
        &mut self,
        targets: &[ResourceId],
        reason: CloseReason,
        attribution: super::CloseAttribution,
    ) -> usize {
        let plan = self.plan_close(targets, reason);
        self.cancel_close_plan(&plan, attribution);
        plan.resources().len()
    }

    /// Note that `resource` has an exit watcher, which runs until its
    /// process is gone ([`Self::end_exit_watch`]).
    pub(crate) fn begin_exit_watch(&mut self, resource: ResourceId) {
        self.exit_watches.insert(resource, None);
    }

    /// Remember the wire id of a watched resource a kill closed before its
    /// process exited, so its watcher can still fire `pane-exit` for it.
    pub(crate) fn note_closed_before_exit(&mut self, resource: ResourceId, wire: WireResourceId) {
        if let Some(slot) = self.exit_watches.get_mut(&resource) {
            *slot = Some(wire);
        }
    }

    /// The wire id of a watched resource a kill closed before its process
    /// exited; `None` for one that is live or another closer reaped.
    #[must_use]
    pub(crate) fn closed_before_exit(&self, resource: ResourceId) -> Option<WireResourceId> {
        self.exit_watches.get(&resource).cloned().flatten()
    }

    /// End the watch once the process is gone and check whether the server
    /// may now exit.
    pub(crate) fn end_exit_watch(&mut self, resource: ResourceId) -> bool {
        self.exit_watches.remove(&resource);
        self.self_exit_due()
    }

    /// Whether self-exit may run now (R13): the server has served a client,
    /// no activation is live, no killed resource's process is still in its
    /// hangup grace, and no connection is open. Records, including empty
    /// persistent sessions, survive in the store and do not hold it up.
    #[must_use]
    pub(crate) fn self_exit_due(&self) -> bool {
        self.lifecycle().has_served_client()
            && self.exit_watches.is_empty()
            && self.lifecycle().idle_since().is_some()
            && !self.has_live_activation()
    }
}

#[cfg(test)]
mod tests {
    use phux_core::ids::ResourceId;
    use phux_core::resource::AgentFacet;
    use phux_protocol::wire::frame::CloseReason;
    use tokio_util::sync::CancellationToken;

    use crate::state::{ClientId, CloseAttribution, ServerState};

    fn agent(provider: &str) -> AgentFacet {
        AgentFacet {
            provider: provider.to_owned(),
            native_id: None,
            state: None,
        }
    }

    /// Terminal → child → grandchild (re-parented after insert, since spawn
    /// refuses a second level).
    fn three_level_tree(state: &mut ServerState) -> (ResourceId, ResourceId, ResourceId) {
        let (_session, grandparent) = state.seed_session("main");
        let child = state
            .new_agent_session(grandparent, &agent("child"))
            .expect("child");
        let grandchild = state
            .new_agent_session(grandparent, &agent("grandchild"))
            .expect("grandchild seed");
        state
            .registry_mut()
            .resource_mut(grandchild)
            .expect("live")
            .parent = Some(child);
        (grandparent, child, grandchild)
    }

    #[test]
    fn kill_cascades_through_grandchildren() {
        let mut state = ServerState::new();
        let (grandparent, child, grandchild) = three_level_tree(&mut state);
        let grandchild_token = CancellationToken::new();
        state
            .resources
            .register_token_for_test(grandchild, grandchild_token.clone());

        let closed = state.close_resources(&[grandparent], CloseReason::Killed);

        assert_eq!(closed, 3, "grandparent, child, and grandchild");
        assert_eq!(
            state.pending_close_reason(grandparent),
            Some(CloseReason::Killed)
        );
        assert_eq!(
            state.pending_close_reason(child),
            Some(CloseReason::ParentClosed)
        );
        assert_eq!(
            state.pending_close_reason(grandchild),
            Some(CloseReason::ParentClosed),
            "a grandchild must close with its ancestor (ADR-0104 §2); a \
             frozen 0..len() range stops at the child and leaves this None"
        );
        assert!(
            grandchild_token.is_cancelled(),
            "the grandchild's actor must be cancelled, not left running"
        );
        assert_eq!(
            state.resource_descendants(grandparent),
            vec![child, grandchild]
        );
    }

    #[test]
    fn late_attribution_keeps_the_original_close_reason() {
        let mut state = ServerState::new();
        let (_, pane) = state.seed_session("main");
        let first = CloseAttribution {
            actor: Some(ClientId(1)),
            operation_id: None,
        };
        let later = CloseAttribution {
            actor: Some(ClientId(2)),
            operation_id: None,
        };
        state.mark_resource_closing(
            pane,
            CloseReason::ServerShutdown,
            CloseAttribution::default(),
        );
        state.close_resources_attributed(&[pane], CloseReason::Killed, first);
        state.close_resources_attributed(&[pane], CloseReason::Killed, later);

        assert_eq!(
            state.begin_resource_close(pane),
            Some((CloseReason::ServerShutdown, first))
        );
        state.reap_resource(pane);
        assert_eq!(state.begin_resource_close(pane), None);
    }

    /// phux-twft: reaping a parent directly (no close cascade first, as a
    /// failed publication does) removes its descendants from the registry,
    /// and their handles and engines go with them, so no handle outlives its
    /// record in the resource table.
    #[test]
    fn a_direct_reap_forgets_descendant_handles_and_cancels_their_engines() {
        let mut state = ServerState::new();
        let (grandparent, child, grandchild) = three_level_tree(&mut state);
        let mut tokens = Vec::new();
        for resource in [grandparent, child, grandchild] {
            let token = CancellationToken::new();
            let handle = if resource == grandparent {
                crate::state::tests::mk_handle()
            } else {
                crate::state::tests::mk_agent_handle(grandparent, token.clone())
            };
            state
                .register_resource_handle(resource, handle, token.clone())
                .unwrap();
            tokens.push(token);
        }

        let _ = state.reap_resource(grandparent);

        for resource in [grandparent, child, grandchild] {
            assert!(
                state.resources().handle(resource).is_none(),
                "{resource:?} must leave the resource table with its registry entry"
            );
            assert!(state.registry().record(resource).is_none());
        }
        assert!(state.resources().resource_ids().is_empty());
        assert!(
            tokens.iter().all(CancellationToken::is_cancelled),
            "no descendant engine may keep running unreachable"
        );
    }
}
