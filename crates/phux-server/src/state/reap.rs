use phux_core::ids::{ResourceId, SessionId};
use phux_core::process::ExitOutcome;
use phux_protocol::ids::SessionId as WireSessionId;

use super::ServerState;

/// The layout-family key suffix `.layout/v1/<wire session id>` (docs/organization.md,
/// §3.5), used to find every layout key naming a reaped session.
fn layout_key_suffix(session: WireSessionId) -> String {
    format!(".layout/v1/{}", session.get())
}

/// The wire session id a `<prefix>.layout/v1/<id>` key names, or `None` for
/// a non-layout key or a non-canonical id (which the reap would never
/// match anyway).
pub(crate) fn layout_key_session_id(key: &str) -> Option<u32> {
    let (prefix, suffix) = key.rsplit_once(".layout/v1/")?;
    if prefix.is_empty() || prefix.contains(".layout/v1/") {
        return None;
    }
    let id = suffix.parse::<u32>().ok()?;
    (suffix == id.to_string()).then_some(id)
}

impl ServerState {
    /// Whether this is a default-Group layout key naming a dead session
    /// (ADR-0129): a TUI republish can race the reap and would resurrect
    /// the key. `None` when not a layout key in that Group.
    #[must_use]
    pub fn layout_key_names_a_dead_session(
        &self,
        scope: &phux_protocol::wire::frame::Scope,
        key: &str,
    ) -> Option<bool> {
        if !matches!(scope, phux_protocol::wire::frame::Scope::Group(gid) if *gid == super::DEFAULT_GROUP_ID)
        {
            return None;
        }
        let wire_id = layout_key_session_id(key)?;
        let live = self
            .idspace
            .resolve_session(WireSessionId::new(wire_id))
            .is_some();
        Some(!live)
    }
}

impl ServerState {
    /// Reap a pane whose actor exited: end its activation, remove its
    /// record (an ephemeral session it empties goes in the same commit), and
    /// free its server-side bookkeeping, descendants included. Returns
    /// whether the server now executes nothing (R13). Idempotent.
    pub fn reap_resource(&mut self, pane: ResourceId) -> bool {
        let (idle, token) = self.reap_terminal_inner(pane, ExitOutcome::UNKNOWN, false);
        if let Some(token) = token {
            token.cancel();
        }
        idle
    }

    /// Reap `pane` but leave its actor running until the caller cancels the
    /// returned token (so a fenced pump can publish the last screen).
    /// `outcome` is how its process ended, as its exit watcher saw it.
    pub(crate) fn reap_terminal_deferring_actor_cancel(
        &mut self,
        pane: ResourceId,
        outcome: ExitOutcome,
        preserve_record: bool,
    ) -> (bool, Option<tokio_util::sync::CancellationToken>) {
        self.reap_terminal_inner(pane, outcome, preserve_record)
    }

    fn reap_terminal_inner(
        &mut self,
        pane: ResourceId,
        outcome: ExitOutcome,
        preserve_record: bool,
    ) -> (bool, Option<tokio_util::sync::CancellationToken>) {
        if self.sessions.registry.resource(pane).is_none() {
            return (!self.has_live_activation(), None);
        }
        let parent = self.resource_parent(pane);
        // A bound descendant still here (no close cascade ran first, e.g. a
        // failed publication) must lose its record, handle, and engine too:
        // its own watcher will find it gone and reap nothing.
        let descendants = self.resource_descendants(pane);
        for child in descendants.into_iter().rev() {
            if let Some(child_token) = self.forget_resource_bookkeeping(child) {
                child_token.cancel();
            }
            self.retire_record(child, ExitOutcome::UNKNOWN);
        }
        if preserve_record {
            self.activation_ended(pane, outcome, false);
        }
        let token = self.forget_resource_bookkeeping(pane);
        if !preserve_record {
            self.retire_record(pane, outcome);
        }
        // A removed producer hands its parent back to the remaining sources.
        if let Some(handle) = parent.and_then(|parent| self.resource_handle(parent)) {
            let _ = handle
                .control
                .try_send(crate::resource::ControlRequest::AgentRecordInvalidated);
        }
        (!self.has_live_activation(), token)
    }

    /// Delete every default-Group `<prefix>.layout/v1/<id>` key for the
    /// session (no-op if it never had a wire id).
    pub(super) fn forget_layout_keys_of_session(&mut self, wire: Option<WireSessionId>) {
        let Some(wire) = wire else {
            return;
        };
        let suffix = layout_key_suffix(wire);
        let scope = phux_protocol::wire::frame::Scope::Group(super::DEFAULT_GROUP_ID);
        let keys: Vec<String> = self
            .metadata()
            .list(&scope)
            .into_iter()
            .filter(|key| key.ends_with(&suffix))
            .collect();
        for key in keys {
            let _ = self.metadata_delete(&scope, &key);
        }
    }

    /// Drain the sessions a commit deleted whose attached clients still need
    /// `DETACHED { SESSION_KILLED }`.
    pub fn take_killed_sessions(&mut self) -> Vec<SessionId> {
        self.sessions.take_killed()
    }

    /// Drop every map entry keyed on a removed pane and retire its wire id
    /// (never reused).
    pub(super) fn forget_resource_bookkeeping(
        &mut self,
        pane: ResourceId,
    ) -> Option<tokio_util::sync::CancellationToken> {
        // Subscriptions go now; draining pumps remain owned until close.
        // The actor stays available for their final checkpoint captures.
        let token = self.resources.forget_resource(pane);
        // An unclaimed close reason must not outlive its id.
        self.forget_close_reason(pane);
        self.retained.forget(pane);
        // Asked state is keyed by core id; the arbiter by wire id (below).
        self.agent.clear_asked(pane);
        // No lease change can follow a removed pane.
        self.leases.forget_resource(pane);
        // The retired wire id keys the metadata scope and the arbiter.
        if let Some(wire) = self.idspace.retire_terminal(pane) {
            if self.native_agents.bindings.remove(&wire).is_some()
                && let Some(wake) = self.native_agents.wakes.remove(&wire)
            {
                wake.notify_waiters();
            }
            self.metadata.forget_resource(&wire);
            // A viewer mark (ADR-0127) must not outlive the Terminal it names.
            self.clients.forget_viewed_terminal(&wire);
            // A recycled wire id must not inherit a stale declaration.
            self.agent.forget_record(&wire);
        }
        token
    }

    /// Forget a removed session's wire id and last-touch ordering entry,
    /// and revoke every delegated credential scoped to it (ADR-0163 §5).
    pub(super) fn forget_session_bookkeeping(&mut self, session: SessionId) {
        self.revoke_session_delegations(session);
        self.idspace.forget_session(session);
        self.sessions.forget_session(session);
    }
}
