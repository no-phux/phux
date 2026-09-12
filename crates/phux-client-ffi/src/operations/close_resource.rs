//! Intentional pane termination, distinct from abandoned-spawn cleanup.

use phux_client_core::session::InputEligibility;
use phux_protocol::wire::frame::KillConditions;

use super::{
    BridgeError, Client, Command, FrameKind, KillPrecondition, MAX_DYNAMIC_TERMINALS, Operations,
    Pending, PhuxClient, PhuxClientResult, PhuxResourceId, ResourceId, ensure_queue_capacity, ptr,
    terminal_id_in, valid_terminal, with_client_mut,
};

/// Queue intentional termination of a terminal owned by this attached client.
///
/// Success/refusal is reported as operation kind 5. Neither enqueue nor success
/// withdraws admission: authoritative `RESOURCE_CLOSED` retires the replica.
///
/// The client is the connection fence: it cannot reconnect, disconnect discards
/// queued bytes, and calls on it then fail. A host must retain this exact client
/// for a captured action, never replay it on a replacement connection. Bound
/// spawn evidence adds an instance-only server precondition when available.
/// Satellite resources require that evidence: a hub connection alone cannot
/// fence a restarted satellite's recycled numeric resource IDs.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread. The ID and its
/// host span are readable for the call. Outgoing bytes belong to this connection
/// only and must not be replayed after its loss.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_close_resource(
    client: *mut PhuxClient,
    request_id: u32,
    terminal_id: *const PhuxResourceId,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.ensure_attached()?;
        // SAFETY: caller supplies the readable ID and host span.
        let id = unsafe { terminal_id_in(terminal_id) }?;
        valid_terminal(&id)?;
        ensure_queue_capacity(client, request_id)?;
        ensure_close_owner(client, &id)?;
        client.queue_frame(&FrameKind::Command {
            request_id,
            command: close_command(client, &id)?,
        })?;
        client.operations.insert(request_id, Pending::Close(id));
        Ok(())
    })
}

/// Queue one all-or-nothing local `KILL_RESOURCES` after validating every owner.
///
/// The batch result (kind 6) has no single terminal. No instance-conditional
/// batch exists on the wire; the captured connection fence is mandatory.
/// Satellite IDs are refused even when bound: the batch relay does not provide
/// correlated atomic teardown, so no local prefix may be submitted either.
///
/// # Safety
/// As for `phux_client_queue_close_resource`. `terminal_ids` contains `count`
/// readable IDs with readable host spans. The array is copied before return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_close_resources(
    client: *mut PhuxClient,
    request_id: u32,
    terminal_ids: *const PhuxResourceId,
    count: usize,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.ensure_attached()?;
        ensure_queue_capacity(client, request_id)?;
        // SAFETY: forwards the readable array and spans contract.
        let ids = unsafe { close_ids_in(client, terminal_ids, count) }?;
        client.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::KillResources { ids: ids.clone() },
        })?;
        client
            .operations
            .insert(request_id, Pending::CloseMany(ids));
        Ok(())
    })
}

unsafe fn close_ids_in(
    client: &Client,
    terminal_ids: *const PhuxResourceId,
    count: usize,
) -> Result<Vec<ResourceId>, BridgeError> {
    if terminal_ids.is_null() || count == 0 || count > MAX_DYNAMIC_TERMINALS {
        return Err(BridgeError::invalid(
            "close batch needs 1..256 readable terminal IDs",
        ));
    }
    // SAFETY: caller supplies count readable records, checked non-null and bounded.
    let records = unsafe { std::slice::from_raw_parts(terminal_ids, count) };
    let mut ids = Vec::with_capacity(count);
    for record in records {
        // SAFETY: each record and its host span obey the array contract.
        let id = unsafe { terminal_id_in(ptr::from_ref(record)) }?;
        valid_terminal(&id)?;
        ensure_local_batch_target(&id)?;
        ensure_close_owner(client, &id)?;
        if ids.contains(&id) {
            return Err(BridgeError::invalid("close batch repeats a terminal ID"));
        }
        ids.push(id);
    }
    Ok(ids)
}

fn close_pending(operations: &Operations, id: &ResourceId) -> bool {
    operations.pending.values().any(|op| match op {
        Pending::Close(target) => target == id,
        Pending::CloseMany(targets) => targets.contains(id),
        _ => false,
    })
}

fn ensure_local_batch_target(id: &ResourceId) -> Result<(), BridgeError> {
    if matches!(id, ResourceId::Satellite { .. }) {
        return Err(BridgeError::state(
            "atomic close is unavailable for satellite resources; batch was not queued",
        ));
    }
    Ok(())
}

fn ensure_close_owner(client: &Client, id: &ResourceId) -> Result<(), BridgeError> {
    if !client.operations.admitted(id) && !client.session.active_attach_contains(id) {
        return Err(BridgeError::state(
            "close requires a terminal owned by this attachment",
        ));
    }
    if !matches!(
        client.session.input_eligibility(id),
        InputEligibility::Eligible { .. }
    ) {
        return Err(BridgeError::state(
            "close requires a current live terminal stream",
        ));
    }
    if client.operations.subscription_pending(id) {
        return Err(BridgeError::state(
            "terminal has a pending subscription operation",
        ));
    }
    if close_pending(&client.operations, id) {
        return Err(BridgeError::state("terminal close is already pending"));
    }
    Ok(())
}

fn close_command(client: &Client, id: &ResourceId) -> Result<Command, BridgeError> {
    match (id, client.operations.instances.get(id)) {
        (_, Some(instance)) => Ok(Command::KillResourceIf {
            terminal_id: id.clone(),
            precondition: KillPrecondition {
                instance: Some(*instance),
                conditions: KillConditions::NONE,
            },
        }),
        (ResourceId::Local { .. }, None) => Ok(Command::KillResource {
            terminal_id: id.clone(),
        }),
        (ResourceId::Satellite { .. }, None) => Err(BridgeError::state(
            "satellite close requires an instance-bound resource; no safe incarnation fence",
        )),
    }
}
