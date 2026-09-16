//! Attach roles (ADR-0127, `docs/spec/L1.md` §8.1): declared intent
//! projected onto the input lease.
//!
//! A role is not a second arbitration system. The lease (ADR-0033) stays the
//! arbitration and the connection's scope grant stays the security boundary;
//! this module only remembers which subscriptions were declared observe-only
//! and turns a declared takeover into the lease change it stands for, in the
//! same critical section that subscribes the connection. The broadcasts that
//! follow go through the pane's engine off the lock, exactly as
//! `ACQUIRE_INPUT`'s do.

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
    /// Whether `client` subscribed `terminal` as a `VIEWER`.
    #[must_use]
    pub fn is_viewer(&self, client: ClientId, terminal: &WireResourceId) -> bool {
        self.clients.is_viewer(client, terminal)
    }

    /// Every connection subscribed to `terminal` as a `VIEWER`, ascending.
    #[must_use]
    pub fn terminal_viewers(&self, terminal: &WireResourceId) -> Vec<ClientId> {
        self.clients.viewers_of(terminal)
    }

    /// Remember whether `client` attached its session as a `VIEWER`, so
    /// panes it spawns into that session are observe-only too. A connection
    /// with no attached session has nothing to remember.
    pub fn set_attached_viewer(&mut self, client: ClientId, viewer: bool) {
        if let Some(attached) = self.clients.attached.get_mut(&client) {
            attached.viewer = viewer;
        }
    }

    /// Mark a Terminal `client` just spawned when its session attach
    /// declared `VIEWER`: "watch every pane, type into none" covers its own
    /// spawns. A first mark, not a change, so nothing is journaled.
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

    /// Apply `policy` for `client` on `terminal`, in the critical section
    /// that subscribes it.
    ///
    /// `core` is the local Terminal the lease lives on; `None` for a
    /// satellite Terminal, whose lease the hub's own ledger arbitrates.
    /// `was_subscribed` is whether the connection already held a
    /// subscription before this attach: a first attach as a viewer is not a
    /// change. A widening always is: a `VIEWER` declaration is a tombstone
    /// that outlives detach and a stream's end, so it is the prior role here.
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

    /// Hand back the lease a takeover attach seized when that attach then
    /// failed, so an unsubscribed client never keeps the wheel. The lease
    /// returns to `Open`, not to a holder the takeover displaced. Returns
    /// whether a lease was released.
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

    /// Set `client`'s viewer mark on `terminal` outright: a hub puts back
    /// the mark an attach replaced when the satellite refuses that attach.
    pub fn set_viewer_mark(&mut self, client: ClientId, terminal: &WireResourceId, viewer: bool) {
        self.clients.mark_viewer(client, terminal, viewer);
    }

    /// The lease change `policy` stands for: a viewer never holds the
    /// lease, and a deliberate takeover seizes it. `PRIMARY` with `NEVER`
    /// leaves the lease untouched.
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

    /// `ACQUIRE_INPUT { SEIZE }` with no TTL: an attach carries none, so the
    /// grant supersedes whatever timer the pane's prior lease armed. A
    /// caller that already holds the lease keeps it untouched, so every
    /// lease change this reports is one the attach granted, and
    /// [`Self::undo_attach_takeover`] can only ever hand back that.
    fn seize_for_attach(&mut self, client: ClientId, core: ResourceId) -> Option<ControlAction> {
        let prior = self.input_lease_holder(core);
        if prior == Some(client) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A `VIEWER` declaration outlives `DETACH` (security review): only the
    /// connection closing, which L15's revocation also ends in, or a fresh
    /// `PRIMARY` attach drops it.
    #[test]
    fn detach_keeps_the_viewer_tombstone_and_closing_the_connection_drops_it() {
        let mut state = ServerState::new();
        let client = state.new_client_id();
        let terminal = WireResourceId::local(7);
        let _ = state.apply_attach_role(client, &terminal, None, RolePolicy::VIEWER, false);
        state.detach(client);
        assert!(state.is_viewer(client, &terminal), "detach must not widen");
        state.forget_connection(client);
        assert!(!state.is_viewer(client, &terminal));
        assert!(state.terminal_viewers(&terminal).is_empty());
    }

    /// The tombstone is the prior role, so a fresh `PRIMARY` attach after a
    /// detach is a flip, and the caller journals it.
    #[test]
    fn a_fresh_primary_attach_after_detach_is_a_journaled_widening() {
        let mut state = ServerState::new();
        let client = state.new_client_id();
        let terminal = WireResourceId::local(7);
        let _ = state.apply_attach_role(client, &terminal, None, RolePolicy::VIEWER, false);
        state.detach(client);
        let effects = state.apply_attach_role(client, &terminal, None, RolePolicy::PRIMARY, false);
        assert!(effects.role_changed);
        assert!(!state.is_viewer(client, &terminal));
    }
}
