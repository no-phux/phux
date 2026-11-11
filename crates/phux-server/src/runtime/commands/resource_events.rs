//! Resource-scoped event subscriptions.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;

/// Handle `SUBSCRIBE_RESOURCE_EVENTS`: a Terminal-scoped `SUBSCRIBE_EVENTS`
/// with a type filter in the same registry (ADR-0123); a repeat replaces the
/// filter. Registered before `Ok`, so no later event is missed.
pub(crate) fn handle_subscribe_terminal_events(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    event_types: Vec<phux_protocol::wire::frame::ResourceEventType>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> CommandResult {
    let filter = crate::state::EventFilter::of(event_types);
    let registered = state.with_mut(|s| {
        s.resource_from_wire(terminal_id)?;
        s.subscribe_resource_events(client_id, terminal_id.clone(), filter, out_tx.clone());
        Some(())
    });
    if registered.is_none() {
        return terminal_not_found(terminal_id);
    }
    crate::runtime::connections::ensure_event_pump(state, client_id);
    debug!(
        ?client_id,
        ?terminal_id,
        "SUBSCRIBE_RESOURCE_EVENTS: subscriber registered"
    );
    CommandResult::Ok
}
