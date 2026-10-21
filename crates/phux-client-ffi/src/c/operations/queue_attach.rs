//! Queueing terminal attach and detach.

use super::{
    BridgeError, Client, Command, FrameKind, MAX_OPERATIONS, Pending, PhuxAttachResourceOptions,
    PhuxClient, PhuxClientResult, PhuxDetachResourceOptions, ResourceId, check_struct, mem, ptr,
    terminal_id_in, valid_terminal, with_client_mut,
};

/// Queue a subscription and admit bootstrap frames before `COMMAND_RESULT` arrives.
///
/// # Safety
/// Client must be live and exclusively accessed on its owning thread. Options and
/// any nonempty terminal host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_attach_resource(
    client: *mut PhuxClient,
    options: *const PhuxAttachResourceOptions,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.ensure_attached()?;
        // SAFETY: caller supplies readable options when non-null.
        let options =
            unsafe { options.as_ref() }.ok_or_else(|| BridgeError::invalid("options is null"))?;
        check_struct(
            options.size,
            mem::size_of::<PhuxAttachResourceOptions>(),
            options.version,
        )?;
        // SAFETY: options includes the readable ID and its host span.
        let id = unsafe { terminal_id_in(ptr::from_ref(&options.terminal_id)) }?;
        valid_terminal(&id)?;
        queue_terminal_attach(client, options.request_id, id)
    })
}

/// Declare the role every later `ATTACH` and `ATTACH_RESOURCE` carries.
///
/// ADR-0127: bit 0 `VIEWER`, bit 1 `DELIBERATE` takeover, the byte of
/// `docs/spec/L1.md` §8.1. `0` restores the default and sends nothing.
/// Additive; the ABI version is unchanged.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_attach_role(
    client: *mut PhuxClient,
    role_policy: u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.attach_role = declared_role(client, role_policy)?;
        Ok(())
    })
}

/// The stored form of a `phux_client_attach_role` byte: refused when a bit is
/// reserved, when it asks a viewer to take over, or when the server has not
/// advertised `ATTACH_ROLES`, which would ignore the byte and grant an
/// ordinary attach.
pub(super) fn declared_role(
    client: &Client,
    role_policy: u8,
) -> Result<Option<phux_protocol::wire::frame::RolePolicy>, BridgeError> {
    use phux_protocol::wire::frame::RolePolicy;
    let policy = RolePolicy::from_u8(role_policy);
    if policy.to_u8() != role_policy || !policy.is_valid() {
        return Err(BridgeError::invalid(
            "role_policy must be 0, 1 (VIEWER), or 2 (PRIMARY with DELIBERATE takeover)",
        ));
    }
    if policy == RolePolicy::PRIMARY {
        return Ok(None);
    }
    if !client.protocol_ready || !client.attach_roles {
        return Err(BridgeError::state(
            "the server did not advertise ATTACH_ROLES; an attach role cannot be declared",
        ));
    }
    Ok(Some(policy))
}

pub(super) fn queue_terminal_attach(
    client: &mut Client,
    request_id: u32,
    id: ResourceId,
) -> Result<(), BridgeError> {
    ensure_queue_capacity(client, request_id)?;
    client.operations.check_admission_capacity()?;
    if client.control().terminal_is_admitted(&id) || client.operations.admitted(&id) {
        return Err(BridgeError::state("terminal is already admitted"));
    }
    if client.operations.subscription_pending(&id) {
        return Err(BridgeError::state(
            "terminal already has a pending subscription operation",
        ));
    }
    if client.terminal_closed(&id) {
        return Err(BridgeError::state(
            "terminal in the initial ATTACH inventory has closed",
        ));
    }
    let role_policy = client.next_attach_role();
    client.queue_frame(&FrameKind::Command {
        request_id,
        command: Command::attach_terminal(id.clone(), role_policy),
    })?;
    if !client.control().admit_external_attach(&id, role_policy) {
        return Err(BridgeError::state(
            "terminal admission changed while queueing",
        ));
    }
    client.operations.dynamic.insert(id.clone());
    client.operations.insert(request_id, Pending::Attach(id));
    Ok(())
}

/// Queue per-terminal detach without stopping the remote process. Admission and
/// replica state remain until correlated success; refusals preserve both.
///
/// # Safety
/// Client is live and exclusively accessed. Options and terminal host are readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_detach_resource(
    client: *mut PhuxClient,
    options: *const PhuxDetachResourceOptions,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.ensure_attached()?;
        // SAFETY: caller supplies readable options when non-null.
        let options =
            unsafe { options.as_ref() }.ok_or_else(|| BridgeError::invalid("options is null"))?;
        check_struct(
            options.size,
            mem::size_of::<PhuxDetachResourceOptions>(),
            options.version,
        )?;
        // SAFETY: options includes the readable ID and host span.
        let id = unsafe { terminal_id_in(ptr::from_ref(&options.terminal_id)) }?;
        valid_terminal(&id)?;
        queue_terminal_detach(client, options.request_id, id)
    })
}

pub(super) fn queue_terminal_detach(
    client: &mut Client,
    request_id: u32,
    id: ResourceId,
) -> Result<(), BridgeError> {
    ensure_queue_capacity(client, request_id)?;
    if client.operations.subscription_pending(&id) {
        return Err(BridgeError::state(
            "terminal has a pending subscription operation",
        ));
    }
    if !client.control().terminal_is_admitted(&id) && !client.operations.admitted(&id) {
        return Err(BridgeError::state("terminal is not admitted"));
    }
    client.queue_frame(&FrameKind::Command {
        request_id,
        command: Command::DetachResource {
            terminal_id: id.clone(),
        },
    })?;
    client.operations.insert(request_id, Pending::Detach(id));
    Ok(())
}

pub(super) fn ensure_queue_capacity(client: &Client, request_id: u32) -> Result<(), BridgeError> {
    client.operations.check_capacity(request_id)?;
    if client.outgoing.len() >= MAX_OPERATIONS {
        return Err(BridgeError::state(
            "outgoing queue is full; drain outgoing frames before adding operations",
        ));
    }
    Ok(())
}
