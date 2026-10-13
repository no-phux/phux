//! Intentional pane termination, distinct from abandoned-spawn cleanup.

use phux_protocol::wire::frame::{KillConditions, TerminateMode};

use super::{
    BridgeError, Client, Command, FrameKind, KillPrecondition, MAX_DYNAMIC_TERMINALS, Operations,
    Pending, PendingClose, PhuxClient, PhuxClientResult, PhuxResourceId, ResourceId,
    ensure_queue_capacity, ptr, terminal_id_in, valid_terminal, with_client_mut,
};

/// Queue intentional termination of a terminal owned by this attached client.
///
/// Success/refusal is reported as operation kind 5. Success is retained until
/// this Client receives both command Ok and authoritative `RESOURCE_CLOSED`,
/// in either order. Enqueue and Ok never fabricate replica retirement.
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
        let command = match terminate_command(client, &id) {
            Some(command) => command,
            None => close_command(client, &id)?,
        };
        client.queue_frame(&FrameKind::Command {
            request_id,
            command,
        })?;
        client
            .operations
            .insert(request_id, Pending::Close(PendingClose::new(vec![id])));
        Ok(())
    })
}

/// Queue one all-or-nothing local `KILL_RESOURCES` after validating every owner.
///
/// The batch result (kind 6) has no single terminal. No instance-conditional
/// batch exists on the wire; the captured connection fence is mandatory.
/// Success requires command Ok and `RESOURCE_CLOSED` for every captured ID;
/// disconnect before all evidence arrives reports an unknown outcome.
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
    // SAFETY: same readable-array contract as documented on this symbol.
    unsafe {
        queue_close_batch(client, request_id, terminal_ids, count, |ids| {
            Command::KillResources {
                ids,
                operation_id: None,
            }
        })
    }
}

/// Queue one all-or-nothing local `CLOSE_TAB_RESOURCES` after the same owner
/// checks as [`phux_client_queue_close_resources`].
///
/// The batch result is still kind 6. Keep-empty on a fully covered session
/// is preserved (L1 §5.2.2). Satellite IDs are refused for the same reason
/// as `queue_close_resources`. Success still requires command Ok and
/// `RESOURCE_CLOSED` for every captured ID.
///
/// # Safety
/// As for `phux_client_queue_close_resources`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_close_tab_resources(
    client: *mut PhuxClient,
    request_id: u32,
    terminal_ids: *const PhuxResourceId,
    count: usize,
) -> PhuxClientResult {
    // SAFETY: same readable-array contract as documented on this symbol.
    unsafe { queue_close_batch_terminating(client, request_id, terminal_ids, count) }
}

/// Whether `HELLO_OK` advertised `CLOSE_TAB_RESOURCES`.
///
/// # Safety
/// Client is live and unmodified for the call; `out_supported` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_close_tab_resources_supported(
    client: *const PhuxClient,
    out_supported: *mut bool,
) -> PhuxClientResult {
    super::with_client_ref(client, |client| {
        // SAFETY: caller supplies a writable output when non-null.
        let out = unsafe { out_supported.as_mut() }
            .ok_or_else(|| BridgeError::invalid("supported output is null"))?;
        *out = client.close_tab_resources;
        Ok(())
    })
}

unsafe fn queue_close_batch(
    client: *mut PhuxClient,
    request_id: u32,
    terminal_ids: *const PhuxResourceId,
    count: usize,
    command: impl FnOnce(Vec<ResourceId>) -> Command,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.ensure_attached()?;
        ensure_queue_capacity(client, request_id)?;
        // SAFETY: forwards the readable array and spans contract.
        let ids = unsafe { close_ids_in(client, terminal_ids, count) }?;
        client.queue_frame(&FrameKind::Command {
            request_id,
            command: command(ids.clone()),
        })?;
        client
            .operations
            .insert(request_id, Pending::CloseMany(PendingClose::new(ids)));
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
        Pending::Close(close) | Pending::CloseMany(close) => close.contains(id),
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
    if !client.operations.admitted(id) && !client.active_attach_contains(id) {
        return Err(BridgeError::state(
            "close requires a terminal owned by this attachment",
        ));
    }
    if !client.input_ready(id) {
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
            operation_id: None,
        }),
        (ResourceId::Local { .. }, None) => Ok(Command::KillResource {
            terminal_id: id.clone(),
            operation_id: None,
        }),
        (ResourceId::Satellite { .. }, None) => Err(BridgeError::state(
            "satellite close requires an instance-bound resource; no safe incarnation fence",
        )),
    }
}

/// `RESOURCE_TERMINATE` of `id`'s current activation, against a server that
/// advertises `ORGANIZATION` whose replica holds `id`'s record. `None` means
/// the legacy kill: no `ORGANIZATION`, no record yet, or a satellite, whose
/// teardown keeps the instance-fenced `KILL_RESOURCE_IF`.
fn terminate_command(client: &Client, id: &ResourceId) -> Option<Command> {
    if matches!(id, ResourceId::Satellite { .. }) || !crate::c::organization::live(client) {
        return None;
    }
    let target = {
        let control = client.control();
        control
            .organization()
            .resource_by_live(id)
            .and_then(phux_client_core::org_ops::live_target)?
    };
    Some(Command::ResourceTerminate {
        operation: phux_client_runtime::control::mint_operation(),
        target,
        mode: TerminateMode::Graceful,
    })
}

/// `phux_client_queue_close_tab_resources`: against a server that
/// advertises `ORGANIZATION`, terminate each terminal on its own
/// (`RESOURCE_TERMINATE`, the first on `request_id` and the rest on
/// bridge-internal ids); the server leaves membership and arrangement to the
/// resources' exit policies. Otherwise one atomic `CLOSE_TAB_RESOURCES`.
unsafe fn queue_close_batch_terminating(
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
        let commands: Option<Vec<Command>> =
            ids.iter().map(|id| terminate_command(client, id)).collect();
        let Some(commands) = commands else {
            client.queue_frame(&FrameKind::Command {
                request_id,
                command: Command::CloseTabResources { ids: ids.clone() },
            })?;
            client
                .operations
                .insert(request_id, Pending::CloseMany(PendingClose::new(ids)));
            return Ok(());
        };
        if client.outgoing.len() + commands.len() > MAX_DYNAMIC_TERMINALS {
            return Err(BridgeError::state(
                "outgoing queue is full; drain outgoing frames before closing",
            ));
        }
        let mut frame_ids = Vec::with_capacity(commands.len());
        frame_ids.push(request_id);
        for _ in 1..commands.len() {
            frame_ids.push(client.arrangement.reserve_internal()?);
        }
        let before = client.outgoing.len();
        for (frame_id, command) in frame_ids.iter().zip(commands) {
            if let Err(error) = client.queue_frame(&FrameKind::Command {
                request_id: *frame_id,
                command,
            }) {
                client.outgoing.truncate(before);
                return Err(error);
            }
        }
        let acks = frame_ids.len();
        for part in &frame_ids[1..] {
            client.operations.parts.insert(*part, request_id);
        }
        client.operations.insert(
            request_id,
            Pending::CloseMany(PendingClose::with_acks(ids, acks)),
        );
        Ok(())
    })
}
