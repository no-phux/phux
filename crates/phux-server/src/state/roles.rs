//! Attach roles (ADR-0127, `docs/protocol.md#attach-and-output`): declared intent
//! projected onto the input lease.
//!
//! The lease (ADR-0033) stays the arbitration; this module remembers which
//! subscriptions are observe-only and turns a declared takeover into a
//! lease change, in the critical section that subscribes the connection.

use phux_core::ids::ResourceId;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{ControlAction, RolePolicy};

use super::{ClientId, ServerState};

/// What applying one attach's declared role changed, for the caller to
/// broadcast through the pane's engine once the lock is released.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoleEffects {
    /// The connection was already subscribed to the Terminal and its role
    /// flipped: journal `terminal_control { ROLE_CHANGED }`.
    pub role_changed: bool,
    /// The lease transition the role caused: `Seized` or `Acquired` for a
    /// deliberate takeover, `Released` when the holder narrowed to a viewer.
    pub lease: Option<ControlAction>,
}

impl RoleEffects {
    /// Whether anything needs broadcasting.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.role_changed && self.lease.is_none()
    }
}

impl ServerState {
    /// Whether the connection attached its session as observe-only.
    #[must_use]
    pub fn is_attached_viewer(&self, client: ClientId) -> bool {
        self.clients
            .attached
            .get(&client)
            .is_some_and(|attached| attached.viewer)
    }

    /// Remember whether `client` attached its session as `VIEWER`.
    pub fn set_attached_viewer(&mut self, client: ClientId, viewer: bool) {
        if let Some(attached) = self.clients.attached.get_mut(&client) {
            attached.viewer = viewer;
        }
    }

    /// Mark a Terminal a `VIEWER`-session client spawned as observe-only
    /// (a first mark, not journaled).
    pub fn mark_if_viewer_session(&mut self, client: ClientId, terminal: &WireResourceId) {
        if self
            .clients
            .attached
            .get(&client)
            .is_some_and(|attached| attached.viewer)
        {
            self.clients.mark_viewer(client, terminal, true);
        }
    }

    /// Apply `policy` for `client` on `terminal` while subscribing it.
    /// `core` is `None` for a satellite Terminal. A first `VIEWER` attach is
    /// not a change; a widening always is.
    pub fn apply_attach_role(
        &mut self,
        client: ClientId,
        terminal: &WireResourceId,
        core: Option<ResourceId>,
        policy: RolePolicy,
        was_subscribed: bool,
    ) -> RoleEffects {
        let flipped = self
            .clients
            .mark_viewer(client, terminal, policy.is_viewer());
        let lease = core.and_then(|core| self.lease_for_role(client, core, policy));
        RoleEffects {
            role_changed: flipped && (was_subscribed || !policy.is_viewer()),
            lease,
        }
    }

    /// Release the lease a failed takeover attach seized (back to `Open`).
    pub fn undo_attach_takeover(
        &mut self,
        client: ClientId,
        core: ResourceId,
        effects: RoleEffects,
    ) -> bool {
        let took = matches!(
            effects.lease,
            Some(ControlAction::Seized | ControlAction::Acquired)
        );
        if !took || !self.release_input_lease(core, client) {
            return false;
        }
        self.refresh_input_lease_expiry(core, false);
        true
    }

    /// The lease change `policy` implies: viewers never hold it, a takeover
    /// seizes it.
    fn lease_for_role(
        &mut self,
        client: ClientId,
        core: ResourceId,
        policy: RolePolicy,
    ) -> Option<ControlAction> {
        if policy.is_viewer() {
            return self.release_for_viewer(client, core);
        }
        if policy.takes_over() {
            return self.seize_for_attach(client, core);
        }
        None
    }

    fn release_for_viewer(&mut self, client: ClientId, core: ResourceId) -> Option<ControlAction> {
        if !self.release_input_lease(core, client) {
            return None;
        }
        self.refresh_input_lease_expiry(core, false);
        Some(ControlAction::Released)
    }

    /// `ACQUIRE_INPUT { SEIZE }` with no TTL; a caller already holding the
    /// lease keeps it untouched, and a non-human caller leaves a human's
    /// lease alone (ADR-0163 §6) and attaches without it.
    fn seize_for_attach(&mut self, client: ClientId, core: ResourceId) -> Option<ControlAction> {
        let prior = self.leases().holder(core);
        if prior == Some(client) || self.lease_protected_from(core, client) {
            return None;
        }
        self.set_input_lease(core, client);
        self.refresh_input_lease_expiry(core, false);
        Some(if prior.is_some() {
            ControlAction::Seized
        } else {
            ControlAction::Acquired
        })
    }
}
