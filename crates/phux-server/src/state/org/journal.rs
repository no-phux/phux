//! Journaling what an applied organization change did.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "shares the parent's namespace"
)]
use super::*;

impl ServerState {
    /// Journal a committed transition's events (ADR-0123), stamped with the
    /// operation and the actor: its principal, and the connection when one
    /// sent the command.
    pub(super) fn journal_org_events(
        &mut self,
        applied: &Applied,
        actor: &Actor,
        client: Option<ClientId>,
    ) {
        let operation = crate::org_wire::operation(applied.operation);
        let mut stamp = client.map_or_else(
            || crate::org_wire::actor_ref(actor, None),
            |client| self.clients.actor_ref(client),
        );
        stamp = stamp
            .with_principal(crate::org_wire::principal_id(&actor.principal))
            .with_on_behalf_of(
                actor
                    .on_behalf_of
                    .as_ref()
                    .and_then(crate::org_wire::principal_id),
            );
        for event in &applied.transition.events {
            let Some(wire) = crate::org_wire::event(event, self.sessions.registry.org()) else {
                continue;
            };
            let record = crate::state::EventRecord::new(None, wire)
                .with_actor(client)
                .with_actor_ref(Some(stamp.clone()))
                .with_operation_id(operation);
            let _ = self.record_and_fanout(record);
        }
    }
}
