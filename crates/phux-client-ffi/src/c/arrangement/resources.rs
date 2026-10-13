//! Authoritative resource discovery, separate from terminal replica admission.
use std::collections::{HashMap, HashSet};

use phux_protocol::ResourceId;
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

use super::{Catalog, Command, CommandResult, FrameKind};
use crate::c::{ResourceKind, client::Client, error::BridgeError};

#[derive(Default)]
pub(crate) struct Subscriptions {
    pending: HashMap<u32, ResourceId>,
    confirmed: HashSet<ResourceId>,
    withdrawn: HashSet<ResourceId>,
    closed: HashSet<ResourceId>,
}

impl Subscriptions {
    pub(super) fn clear(&mut self) {
        self.pending.clear();
        self.confirmed.clear();
        self.withdrawn.clear();
        self.closed.clear();
    }

    pub(crate) fn was_withdrawn(&self, id: &ResourceId) -> bool {
        self.withdrawn.contains(id)
    }

    pub(crate) fn was_closed(&self, id: &ResourceId) -> bool {
        self.closed.contains(id)
    }

    pub(crate) fn cancel(&mut self, id: &ResourceId) {
        self.confirmed.remove(id);
        self.pending.retain(|_, pending| pending != id);
    }

    pub(crate) fn mark_closed(&mut self, id: &ResourceId) {
        self.closed.insert(id.clone());
    }

    fn contains(&self, id: &ResourceId) -> bool {
        self.confirmed.contains(id) || self.pending.values().any(|pending| pending == id)
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
    client.control().reconcile_agent_delivery_catalog(snapshot);
    let removed: Vec<_> = client
        .agent_streams
        .keys()
        .filter(|id| !ids.contains(id))
        .cloned()
        .collect();
    for id in removed {
        withdraw(client, &id, true)?;
    }
    client
        .arrangement
        .subscriptions
        .pending
        .retain(|_, id| ids.contains(id));
    client.resources = snapshot
        .resources
        .iter()
        // A federated GET_STATE may have captured its local contribution before
        // an explicit close that we already received. Durable closure wins.
        .filter(|resource| !client.arrangement.subscriptions.closed.contains(&resource.id))
        .map(|resource| crate::c::client::resource_summary(snapshot, resource))
        .collect();
    for resource in &snapshot.resources {
        if client
            .arrangement
            .subscriptions
            .closed
            .contains(&resource.id)
        {
            continue;
        }
        if subscribable(resource, catalog, client.arrangement.selected) {
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
    // A committed publication can be replayed before this connection subscribes.
    // Its generation is display metadata, never evidence of a live subscription.
    if client.arrangement.subscriptions.contains(&resource.id) {
        return Ok(());
    }
    if client.outgoing.len() >= 128 || client.arrangement.subscriptions.pending.len() >= 128 {
        return Ok(());
    }
    let facet = resource.agent.as_ref();
    client
        .control()
        .apply_engine_event(
            phux_client_runtime::engine::EngineEvent::AgentSessionDeclared {
                terminal_id: resource.id.clone(),
                parent: resource.parent.clone(),
                provider: facet.map(|facet| facet.provider.clone()),
                native_id: facet.and_then(|facet| facet.native_id.clone()),
                state: facet.map(|facet| facet.state.clone()),
            },
        )
        .map_err(crate::c::control_error)?;
    client
        .agent_streams
        .entry(resource.id.clone())
        .or_insert(None);
    client
        .arrangement
        .subscriptions
        .withdrawn
        .remove(&resource.id);
    let request_id = client.arrangement.reserve_internal()?;
    let role_policy = client.next_attach_role();
    let agent_after_seq = client.control().agent_after_seq(&resource.id);
    client.queue_frame(&FrameKind::Command {
        request_id,
        command: Command::attach_agent_session(resource.id.clone(), role_policy, agent_after_seq),
    })?;
    client
        .arrangement
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
    let Some(id) = client.arrangement.subscriptions.pending.remove(&request_id) else {
        return Ok(false);
    };
    if matches!(result, CommandResult::Ok) {
        client.arrangement.subscriptions.confirmed.insert(id);
        return Ok(true);
    }
    // Refusal is not authoritative resource closure. Withdraw only admission;
    // keep the inventory entry and permit the next registry read to retry.
    withdraw(client, &id, false)?;
    match result {
        CommandResult::Error { .. } => Ok(true),
        _ => Err(BridgeError::invalid("unexpected agent subscription reply")),
    }
}

fn withdraw(client: &mut Client, id: &ResourceId, notify: bool) -> Result<(), BridgeError> {
    client.arrangement.subscriptions.cancel(id);
    if let Some(stream) = client.agent_streams.remove(id)
        && notify
    {
        let (stream_id, bootstrap_id) = stream.map_or((0, 0), |info| {
            (info.stream_id.get(), info.bootstrap_id.get())
        });
        let mut effect = crate::c::OwnedEffect::simple(
            crate::c::types::EFFECT_AGENT_RECORDS,
            crate::c::types::AGENT_RECORDS_CLOSED,
            id.clone(),
        );
        effect.stream_id = stream_id;
        effect.bootstrap_id = bootstrap_id;
        client.owned_effects.push(effect);
        client.publish_effects();
    }
    // One borrow: two `client.control()` temporaries would both live to
    // the end of this condition, and the control lock is not reentrant.
    let released = {
        let mut control = client.control();
        control.engine().is_none() || control.release_terminal(id)
    };
    if !released {
        return Err(BridgeError::state("cannot release agent subscription"));
    }
    client
        .arrangement
        .subscriptions
        .withdrawn
        .insert(id.clone());
    Ok(())
}

/// Route independently of the arrangement transaction's failure handler, including
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
