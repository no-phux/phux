//! Authoritative resource discovery, separate from terminal replica admission.
use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

use super::{Catalog, Command, CommandResult, FrameKind};
use crate::{ResourceKind, client::Client, error::BridgeError};

#[derive(Default)]
pub(super) struct Subscriptions {
    pending: HashMap<u32, ResourceId>,
}

impl Subscriptions {
    pub(super) fn clear(&mut self) {
        self.pending.clear();
    }

    fn contains(&self, id: &ResourceId) -> bool {
        self.pending.values().any(|pending| pending == id)
    }
}

/// The registry is the membership authority even when layout metadata is invalid.
/// Unknown kinds stay in the catalog, but never acquire a subscription or replica.
pub(super) fn reconcile(
    client: &mut Client,
    snapshot: &SessionSnapshot,
    catalog: &Catalog,
) -> Result<(), BridgeError> {
    let mut ids = std::collections::HashSet::new();
    for resource in &snapshot.resources {
        if !ids.insert(&resource.id) {
            return Err(BridgeError::invalid("duplicate catalog resource"));
        }
    }
    let removed: Vec<_> = client
        .agent_streams
        .keys()
        .filter(|id| !ids.contains(id))
        .cloned()
        .collect();
    for id in removed {
        crate::apply_terminal_closed(client, &id)?;
    }
    client
        .workspace
        .subscriptions
        .pending
        .retain(|_, id| ids.contains(id));
    client.resources = snapshot
        .resources
        .iter()
        .map(crate::resource_summary)
        .collect();
    for resource in &snapshot.resources {
        if subscribable(resource, catalog, client.workspace.selected) {
            subscribe(client, resource)?;
        }
    }
    Ok(())
}

fn subscribable(resource: &ResourceInfo, catalog: &Catalog, selected: u32) -> bool {
    resource.kind == ResourceKind::AgentSession
        && resource
            .parent
            .as_ref()
            .is_some_and(|parent| catalog.allows(parent, selected))
}

fn subscribe(client: &mut Client, resource: &ResourceInfo) -> Result<(), BridgeError> {
    if client.workspace.subscriptions.contains(&resource.id)
        || client
            .agent_streams
            .get(&resource.id)
            .is_some_and(|stream| stream.generation.is_some())
    {
        return Ok(());
    }
    // The bridge's bounded outgoing queue can be busy with host operations.
    // Leave this resource catalogued; a later refresh retries its subscription.
    if client.outgoing.len() >= 128 || client.workspace.subscriptions.pending.len() >= 128 {
        return Ok(());
    }
    crate::declare_agent_session(client, resource)?;
    let request_id = client.workspace.reserve_internal()?;
    client.queue_frame(&FrameKind::Command {
        request_id,
        command: Command::AttachResource {
            terminal_id: resource.id.clone(),
        },
    })?;
    client
        .workspace
        .subscriptions
        .pending
        .insert(request_id, resource.id.clone());
    Ok(())
}

/// Bootstrap precedes the command acknowledgement. Correlate only requests this
/// discovery path issued; these IDs never enter the host operation-result queue.
pub(super) fn receive_reply(
    client: &mut Client,
    request_id: u32,
    result: &CommandResult,
) -> Result<bool, BridgeError> {
    let Some(id) = client.workspace.subscriptions.pending.remove(&request_id) else {
        return Ok(false);
    };
    if matches!(result, CommandResult::Ok) {
        return Ok(true);
    }
    // Refusal is not authoritative resource closure. Withdraw only admission;
    // keep the inventory entry and permit the next registry read to retry.
    if client.agent_streams.remove(&id).is_some() && !client.session.release_terminal(&id) {
        return Err(BridgeError::state("cannot release agent subscription"));
    }
    match result {
        CommandResult::Error { message, .. } => Err(BridgeError::state(message)),
        _ => Err(BridgeError::invalid("unexpected agent subscription reply")),
    }
}
