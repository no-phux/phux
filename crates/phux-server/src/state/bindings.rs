//! Parent bindings and the close ledger (ADR-0104).
//!
//! A resource may name one parent at spawn. The binding is immutable and it
//! is *lifecycle*, not metadata: when a parent leaves for any reason its
//! children leave with it, and the cascade runs in the same acquisition of
//! the state lock that removes the parent, so no client can observe a child
//! whose parent is gone. That is the guarantee `KILL_TERMINALS` already
//! gives a batch, extended down the one edge this program creates.
//!
//! The graph itself is the registry's — [`Registry::children`] reads the
//! `parent` each descriptor already carries, so there is no second map to
//! keep in step with it. What lives here is the part the registry cannot
//! know: *why* each resource is closing, and which closer owns emitting its
//! `TERMINAL_CLOSED`.
//!
//! # The close ledger
//!
//! Every resource closes through one path: something cancels its engine
//! token, the engine's run loop ends, its exit notification fires, and the
//! per-resource exit watcher broadcasts `TERMINAL_CLOSED` and reaps. That
//! path knows the resource died; it does not know whether a kill, a parent,
//! or the shell's own `exit` did it. So a closer records the reason before
//! it cancels, and the watcher claims it.
//!
//! The claim is what makes a cascade safe. A cascading closer reaps its
//! children itself, inside the one lock — but each child's own watcher is
//! still armed and will wake later. [`ServerState::begin_resource_close`]
//! answers `None` for a resource the registry no longer holds, so exactly
//! one closer ever emits a given resource's frame, and a late watcher for
//! an already-reaped child does nothing rather than interning a fresh wire
//! id for a corpse.
//!
//! [`Registry::children`]: phux_core::registry::Registry::children

use phux_core::ids::ResourceId;
use phux_core::resource::ResourceKind;
use phux_protocol::wire::frame::CloseReason;

use super::ServerState;

impl ServerState {
    /// The resources bound to `parent`. Empty for a leaf, and by the
    /// one-level rule (ADR-0104 §5) a child's own list is always empty.
    #[must_use]
    pub fn resource_children(&self, parent: ResourceId) -> Vec<ResourceId> {
        self.sessions.registry.children(parent)
    }

    /// `true` when `parent` has at least one live `AgentSession` child.
    ///
    /// The query ADR-0103 §5 gives the detector: while a session is
    /// producing records, its stream is the ranking evidence about the
    /// pane's agent and screen derivation is left to the questions no hook
    /// can answer. This exposes the fact; what the detector does with it is
    /// the detector's.
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

    /// The live `AgentSession` children of `parent`, with their handles.
    ///
    /// The producer path's route from a Terminal to the sessions running
    /// inside it — `REPORT_AGENT_STATE`'s fallback and the TUI's per-pane
    /// session line both start here.
    #[must_use]
    pub fn agent_session_children(
        &self,
        parent: ResourceId,
    ) -> Vec<(ResourceId, crate::resource::ResourceHandle)> {
        self.sessions
            .registry
            .children(parent)
            .into_iter()
            .filter(|child| {
                self.sessions
                    .registry
                    .resource(*child)
                    .is_some_and(|r| r.kind == ResourceKind::AgentSession)
            })
            .filter_map(|child| self.resource_handle(child).cloned().map(|h| (child, h)))
            .collect()
    }

    /// Record why `resource` is closing, for the watcher that will emit its
    /// `TERMINAL_CLOSED`.
    ///
    /// First writer wins: a deliberate `Killed`, `ParentClosed`, or
    /// `ServerShutdown` set before the engine stops is not overwritten by a
    /// later, less specific closer, and a resource nobody marked closes as
    /// `Exited` — the shell typed `exit`.
    pub fn mark_resource_closing(&mut self, resource: ResourceId, reason: CloseReason) {
        self.close_reasons.entry(resource).or_insert(reason);
    }

    /// Claim the right to close `resource`, taking the reason recorded for
    /// it.
    ///
    /// `None` when the registry no longer holds `resource`: another closer
    /// already reaped it (a parent cascading, or a racing kill), and this
    /// caller must emit nothing. `Some(CloseReason::Exited)` when it is
    /// live and nobody recorded a reason.
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
    }

    /// Close `targets` and every resource bound to one of them, in a single
    /// borrow of the state (ADR-0104 §2).
    ///
    /// Targets close with `reason`; cascaded children close with
    /// `ParentClosed`, unless the caller also named the child as a target,
    /// in which case its own reason stands and it is cancelled exactly
    /// once. Returns how many distinct resources were closed.
    ///
    /// Only cancellation happens here. Each closed resource's own exit
    /// watcher performs the reap and the `TERMINAL_CLOSED` fanout with the
    /// reason this recorded, which keeps one teardown path for kills,
    /// cascades, and a shell's own exit.
    pub fn close_resources(&mut self, targets: &[ResourceId], reason: CloseReason) -> u32 {
        let mut closing: Vec<(ResourceId, CloseReason)> = Vec::new();
        for target in targets {
            if self.sessions.registry.resource(*target).is_none() {
                continue;
            }
            if !closing.iter().any(|(id, _)| id == target) {
                closing.push((*target, reason));
            }
        }
        for index in 0..closing.len() {
            let (parent, _) = closing[index];
            for child in self.sessions.registry.children(parent) {
                if !closing.iter().any(|(id, _)| *id == child) {
                    closing.push((child, CloseReason::ParentClosed));
                }
            }
        }
        let closed = u32::try_from(closing.len()).unwrap_or(u32::MAX);
        for (resource, reason) in closing {
            self.mark_resource_closing(resource, reason);
            self.detach_resource_actor(resource);
        }
        closed
    }
}
