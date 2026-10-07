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

use phux_core::ids::ResourceId;
use phux_core::resource::ResourceKind;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::CloseReason;

use super::ServerState;

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

    /// Whether `parent` has a live `AgentSession` child (the detector's
    /// ADR-0103 §5 query).
    #[must_use]
    pub fn has_live_agent_session_child(&self, parent: ResourceId) -> bool {
        self.sessions
            .registry
            .children(parent)
            .into_iter()
            .any(|child| {
                self.sessions
                    .registry
                    .resource(child)
                    .is_some_and(|r| r.kind == ResourceKind::AgentSession)
            })
    }

    /// Record why `resource` is closing. First writer wins; an unmarked
    /// resource closes as `Exited`.
    pub fn mark_resource_closing(&mut self, resource: ResourceId, reason: CloseReason) {
        self.close_reasons.entry(resource).or_insert(reason);
    }

    /// Claim the close of `resource` and its recorded reason. `None` when
    /// already reaped (emit nothing); `Exited` when nobody recorded one.
    pub fn begin_resource_close(&mut self, resource: ResourceId) -> Option<CloseReason> {
        self.sessions.registry.resource(resource)?;
        Some(
            self.close_reasons
                .remove(&resource)
                .unwrap_or(CloseReason::Exited),
        )
    }

    /// Forget a reaped resource's recorded close reason.
    pub(super) fn forget_close_reason(&mut self, resource: ResourceId) {
        self.close_reasons.remove(&resource);
        self.close_attributions.remove(&resource);
    }

    /// Take the attribution a kill recorded for `resource`, for its
    /// `pane_closed`; the default (nobody, no key) when none did.
    pub fn take_close_attribution(&mut self, resource: ResourceId) -> super::CloseAttribution {
        self.close_attributions
            .remove(&resource)
            .unwrap_or_default()
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
        let closing = self.mark_and_cancel(targets, reason, attribution);
        u32::try_from(closing.len()).unwrap_or(u32::MAX)
    }

    /// Record why `targets` and their descendants close and cancel their
    /// actors, without reaping. Returns every closing resource, each after
    /// its parent, so a reverse walk closes children first.
    pub(crate) fn mark_and_cancel(
        &mut self,
        targets: &[ResourceId],
        reason: CloseReason,
        attribution: super::CloseAttribution,
    ) -> Vec<ResourceId> {
        let mut closing: Vec<(ResourceId, CloseReason)> = Vec::new();
        for target in targets {
            if self.sessions.registry.resource(*target).is_none() {
                continue;
            }
            if !closing.iter().any(|(id, _)| id == target) {
                closing.push((*target, reason));
            }
        }
        let roots: Vec<ResourceId> = closing.iter().map(|(id, _)| *id).collect();
        for parent in roots {
            for child in self.resource_descendants(parent) {
                if !closing.iter().any(|(id, _)| *id == child) {
                    closing.push((child, CloseReason::ParentClosed));
                }
            }
        }
        let mut closed = Vec::with_capacity(closing.len());
        for (resource, reason) in closing {
            self.mark_resource_closing(resource, reason);
            if attribution != super::CloseAttribution::default() {
                self.close_attributions
                    .entry(resource)
                    .or_insert(attribution);
            }
            self.detach_resource_actor(resource);
            closed.push(resource);
        }
        closed
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

    /// End `resource`'s exit watch: its process is gone.
    pub(crate) fn end_exit_watch(&mut self, resource: ResourceId) {
        self.exit_watches.remove(&resource);
    }

    /// Whether last-session self-exit may run now: no session is left and
    /// no killed resource's process is still in its hangup grace.
    #[must_use]
    pub(crate) fn self_exit_due(&self) -> bool {
        self.sessions.registry.session_count() == 0
            && self.exit_watches.is_empty()
            && self.has_served_client()
    }
}

#[cfg(test)]
mod tests {
    use phux_core::ids::ResourceId;
    use phux_core::resource::AgentFacet;
    use phux_protocol::wire::frame::CloseReason;
    use tokio_util::sync::CancellationToken;

    use crate::state::ServerState;

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
        let (_session, _window, grandparent) = state.seed_session("main");
        let child = state
            .registry_mut()
            .new_agent_session(grandparent, agent("child"))
            .expect("child");
        let grandchild = state
            .registry_mut()
            .new_agent_session(grandparent, agent("grandchild"))
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

    /// phux-twft: reaping a parent directly (no close cascade first, as a
    /// failed publication does) removes its descendants from the registry.
    /// Their handles and engines must go with them; before, each one's watcher
    /// found it already gone, reaped nothing, and its handle stayed in the
    /// resource table for every later upgrade to trip on.
    #[test]
    fn a_direct_reap_forgets_descendant_handles_and_cancels_their_engines() {
        let mut state = ServerState::new();
        let (grandparent, child, grandchild) = three_level_tree(&mut state);
        let mut tokens = Vec::new();
        for resource in [grandparent, child, grandchild] {
            let token = CancellationToken::new();
            let _ = state.register_resource_handle(
                resource,
                crate::state::tests::mk_handle(),
                token.clone(),
            );
            tokens.push(token);
        }

        let _ = state.reap_terminal(grandparent);

        for resource in [grandparent, child, grandchild] {
            assert!(
                state.resource_handle(resource).is_none(),
                "{resource:?} must leave the resource table with its registry entry"
            );
        }
        assert!(state.resource_ids().is_empty());
        assert!(
            tokens.iter().all(CancellationToken::is_cancelled),
            "no descendant engine may keep running unreachable"
        );
    }
}
