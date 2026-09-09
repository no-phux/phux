//! Authoritative resource discovery, separate from terminal replica admission.
use std::collections::{HashMap, HashSet};

use phux_protocol::ResourceId;
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

use super::{Catalog, Command, CommandResult, FrameKind};
use crate::{ResourceKind, client::Client, error::BridgeError};

#[derive(Default)]
pub(crate) struct Subscriptions {
    pending: HashMap<u32, ResourceId>,
    withdrawn: HashSet<ResourceId>,
    closed: HashSet<ResourceId>,
}

impl Subscriptions {
    pub(super) fn clear(&mut self) {
        self.pending.clear();
        self.withdrawn.clear();
        self.closed.clear();
    }

    pub(crate) fn was_withdrawn(&self, id: &ResourceId) -> bool {
        self.withdrawn.contains(id)
    }

    pub(crate) fn cancel(&mut self, id: &ResourceId) {
        self.pending.retain(|_, pending| pending != id);
    }

    pub(crate) fn mark_closed(&mut self, id: &ResourceId) {
        self.closed.insert(id.clone());
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
        withdraw(client, &id)?;
    }
    client
        .workspace
        .subscriptions
        .pending
        .retain(|_, id| ids.contains(id));
    client.resources = snapshot
        .resources
        .iter()
        // A federated GET_STATE may have captured its local contribution before
        // an explicit close that we already received. Durable closure wins.
        .filter(|resource| !client.workspace.subscriptions.closed.contains(&resource.id))
        .map(crate::resource_summary)
        .collect();
    for resource in &snapshot.resources {
        if client.workspace.subscriptions.closed.contains(&resource.id) {
            continue;
        }
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
    client
        .workspace
        .subscriptions
        .withdrawn
        .remove(&resource.id);
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
fn receive_reply(
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
    if client
        .agent_streams
        .get(&id)
        .is_some_and(|stream| stream.generation.is_none())
    {
        client.agent_streams.remove(&id);
    }
    withdraw(client, &id)?;
    match result {
        CommandResult::Error { .. } => Ok(true),
        _ => Err(BridgeError::invalid("unexpected agent subscription reply")),
    }
}

/// Membership withdrawal is reversible; only an explicit wire close is durable.
fn withdraw(client: &mut Client, id: &ResourceId) -> Result<(), BridgeError> {
    crate::retire_agent_stream(client, id);
    if !client.session.release_terminal(id) {
        return Err(BridgeError::state("cannot release agent subscription"));
    }
    client.workspace.subscriptions.withdrawn.insert(id.clone());
    Ok(())
}

/// Route independently of the workspace transaction's failure handler, including
/// malformed subscription replies while a newer host mutation is in flight.
pub(crate) fn dispatch(client: &mut Client, frame: &FrameKind) -> Result<bool, BridgeError> {
    match frame {
        FrameKind::CommandResult { request_id, result } => {
            receive_reply(client, *request_id, result)
        }
        FrameKind::Error {
            request_id: Some(request_id),
            code,
            message,
        } => receive_reply(
            client,
            *request_id,
            &CommandResult::Error {
                code: *code,
                message: message.clone(),
            },
        ),
        _ => Ok(false),
    }
}
