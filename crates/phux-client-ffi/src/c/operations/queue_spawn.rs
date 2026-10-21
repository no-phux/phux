//! Queueing spawns, event subscriptions, conditional kills, and input leases.

use super::{
    BridgeError, Client, Command, FrameKind, GroupId, IdempotencyKey, InputMode, KillPrecondition,
    MAX_OPERATIONS, MAX_SPAWN_ARGS, MAX_SPAWN_BYTES, Pending, PhuxBytes, PhuxClient,
    PhuxClientResult, PhuxResourceId, PhuxSpawnOptions, ResourceId, SatelliteHost, ServerInstance,
    SpawnResource, bytes_in, ensure_queue_capacity, ptr, spawn_options_in, terminal_id_in,
    valid_terminal, with_client_mut, with_client_ref,
};

pub(super) unsafe fn text_in(value: PhuxBytes, budget: &mut usize) -> Result<String, BridgeError> {
    *budget = budget
        .checked_sub(value.len)
        .ok_or_else(|| BridgeError::invalid("spawn text exceeds 64 KiB aggregate bound"))?;
    // SAFETY: caller provides a readable span; its size was bounded above.
    let bytes = unsafe { bytes_in(value.data, value.len) }?;
    let text =
        std::str::from_utf8(bytes).map_err(|_| BridgeError::invalid("spawn text is not UTF-8"))?;
    if text.contains('\0') {
        return Err(BridgeError::invalid("spawn text contains NUL"));
    }
    Ok(text.to_owned())
}

pub(super) unsafe fn command_in(
    options: &PhuxSpawnOptions,
    budget: &mut usize,
) -> Result<Option<Vec<String>>, BridgeError> {
    if options.argc == 0 {
        return Ok(None);
    }
    if options.argc > MAX_SPAWN_ARGS || options.argv.is_null() {
        return Err(BridgeError::invalid(
            "argv is null or exceeds 256 arguments",
        ));
    }
    // SAFETY: caller promises argc readable PhuxBytes records; count is bounded.
    let args = unsafe { std::slice::from_raw_parts(options.argv, options.argc) };
    let command: Vec<_> = args
        .iter()
        .map(|value| {
            // SAFETY: argument spans obey the options contract.
            unsafe { text_in(*value, budget) }
        })
        .collect::<Result<_, _>>()?;
    if command[0].is_empty() {
        return Err(BridgeError::invalid("spawn executable is empty"));
    }
    Ok(Some(command))
}

pub(super) unsafe fn spawn_frame(
    options: &PhuxSpawnOptions,
    bind_instance: bool,
) -> Result<FrameKind, BridgeError> {
    if options.cols == 0 || options.rows == 0 {
        return Err(BridgeError::invalid("spawn geometry must be nonzero"));
    }
    let mut budget = MAX_SPAWN_BYTES;
    // SAFETY: caller's options contract covers all spans.
    let satellite = unsafe { text_in(options.satellite, &mut budget) }?;
    // SAFETY: caller's options contract covers all spans.
    let cwd = unsafe { text_in(options.cwd, &mut budget) }?;
    // SAFETY: caller's options contract covers the argv array and spans.
    let command = unsafe { command_in(options, &mut budget) }?;
    // SAFETY: caller's options contract covers the optional owner pointer.
    let owner_terminal = unsafe { owner_in(options.owner_terminal, &satellite, &mut budget) }?;
    let retain_secs = options.has_retain_secs.then_some(options.retain_secs);
    let idempotency_key = if options.idempotency_key.iter().all(|&byte| byte == 0) {
        None
    } else {
        Some(
            IdempotencyKey::new(options.idempotency_key)
                .ok_or_else(|| BridgeError::invalid("idempotency_key must be 16 nonzero bytes"))?,
        )
    };
    let resource = SpawnResource::default()
        .with_bind_instance(bind_instance)
        .with_retain_secs(retain_secs)
        .with_idempotency_key(idempotency_key);
    Ok(FrameKind::SpawnResource {
        request_id: options.request_id,
        group: GroupId::new(1),
        command,
        cwd: (!cwd.is_empty()).then_some(cwd),
        env: None,
        term: None,
        satellite: (!satellite.is_empty()).then(|| SatelliteHost::new(satellite)),
        owner_terminal,
        agent_session: None,
        initial_size: Some((options.cols, options.rows)),
        // Field 15–17 only when asked: a plain spawn stays byte-identical.
        resource: (!resource.is_default()).then(|| Box::new(resource)),
    })
}

pub(super) unsafe fn owner_in(
    owner: *const PhuxResourceId,
    satellite: &str,
    budget: &mut usize,
) -> Result<Option<ResourceId>, BridgeError> {
    if owner.is_null() {
        return Ok(None);
    }
    // SAFETY: caller supplies a readable owner ID.
    let owner = unsafe { terminal_id_in(owner) }?;
    valid_terminal(&owner)?;
    let expected_route = match &owner {
        ResourceId::Local { .. } => "",
        ResourceId::Satellite { host, .. } => {
            *budget = budget
                .checked_sub(host.as_str().len())
                .ok_or_else(|| BridgeError::invalid("spawn text exceeds 64 KiB aggregate bound"))?;
            host.as_str()
        }
    };
    if satellite != expected_route {
        return Err(BridgeError::invalid(
            "spawn owner must be local without a route, or satellite with an exactly matching route",
        ));
    }
    Ok(Some(owner))
}

/// Queue one durable spawn; consumes no request ID on local validation failure.
///
/// # Safety
/// Client must be live, exclusively accessed on its owning thread. Options and
/// its non-null owner, argc argv records, and nonempty spans must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_spawn(
    client: *mut PhuxClient,
    options: *const PhuxSpawnOptions,
) -> PhuxClientResult {
    // SAFETY: forwards the options contract.
    with_client_mut(client, |client| unsafe {
        queue_spawn(client, options, false)
    })
}

/// `phux_client_queue_spawn` with `SPAWN_RESOURCE.bind_instance` set
/// (ADR-0109). Refused, with nothing queued, without `CONDITIONAL_KILL`.
///
/// # Safety
/// As for `phux_client_queue_spawn`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_spawn_bound(
    client: *mut PhuxClient,
    options: *const PhuxSpawnOptions,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if !client.conditional_kill {
            return Err(BridgeError::state(
                "the server did not advertise CONDITIONAL_KILL; a spawn cannot be bound",
            ));
        }
        // SAFETY: forwards the options contract.
        unsafe { queue_spawn(client, options, true) }
    })
}

/// Arm or send `SUBSCRIBE_EVENTS` with an optional journal cursor (ADR-0123).
///
/// `terminal` null is the connection-wide scope the automatic post-`ATTACH_READY`
/// subscribe uses. `after_seq` null is live-only; a non-null pointer is the
/// cursor and is stored for the automatic subscribe as well. A cursor requires
/// `EVENT_JOURNAL`. Before attach, the call only stores the cursor; after
/// attach it queues the frame immediately.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread. `terminal`,
/// when non-null, is a readable ID; `after_seq`, when non-null, is a readable
/// `u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_subscribe_events(
    client: *mut PhuxClient,
    terminal: *const PhuxResourceId,
    after_seq: *const u64,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if client.detached {
            return Err(BridgeError::state(
                "SUBSCRIBE_EVENTS is not valid on a detached client",
            ));
        }
        if !client.protocol_ready {
            return Err(BridgeError::state(
                "SUBSCRIBE_EVENTS needs a negotiated connection",
            ));
        }
        let after_seq = if after_seq.is_null() {
            None
        } else {
            // SAFETY: caller supplies a readable u64.
            Some(unsafe { ptr::read(after_seq) })
        };
        if after_seq.is_some() && !client.event_journal {
            return Err(BridgeError::state(
                "the server did not advertise EVENT_JOURNAL; a journal cursor cannot be sent",
            ));
        }
        let terminal = if !client.attached || terminal.is_null() {
            None
        } else {
            // SAFETY: caller supplies a readable ID and its host span.
            let id = unsafe { terminal_id_in(terminal) }?;
            valid_terminal(&id)?;
            Some(id)
        };
        client.event_after_seq = after_seq;
        client.control().set_event_after_seq(after_seq);
        if !client.attached {
            return Ok(());
        }
        if client.outgoing.len() >= MAX_OPERATIONS {
            return Err(BridgeError::state(
                "outgoing queue is full; drain outgoing frames before adding operations",
            ));
        }
        client.queue_frame(&FrameKind::SubscribeEvents {
            terminal,
            after_seq,
        })
    })
}

pub(super) unsafe fn queue_spawn(
    client: &mut Client,
    options: *const PhuxSpawnOptions,
    bind_instance: bool,
) -> Result<(), BridgeError> {
    client.ensure_attached()?;
    // SAFETY: caller supplies readable options when non-null.
    let options = unsafe { spawn_options_in(options) }?;
    let retain_secs = options.has_retain_secs.then_some(options.retain_secs);
    let has_idempotency_key = options.idempotency_key.iter().any(|&byte| byte != 0);
    if retain_secs.is_some() && !client.retain_on_exit {
        return Err(BridgeError::state(
            "the server did not advertise RETAIN_ON_EXIT; retain_secs cannot be sent",
        ));
    }
    if has_idempotency_key && !client.spawn_idempotency {
        return Err(BridgeError::state(
            "the server did not advertise SPAWN_IDEMPOTENCY; a spawn cannot carry a key",
        ));
    }
    // SAFETY: forwards the options contract.
    let frame = unsafe { spawn_frame(&options, bind_instance) }?;
    ensure_queue_capacity(client, options.request_id)?;
    client.operations.check_admission_capacity()?;
    let FrameKind::SpawnResource { satellite, .. } = &frame else {
        unreachable!()
    };
    let pending = Pending::Spawn {
        satellite: satellite.clone(),
    };
    client.queue_frame(&frame)?;
    client.operations.insert(options.request_id, pending);
    Ok(())
}

/// Queue `KILL_RESOURCE_IF` for a terminal this client spawned and bound.
///
/// The server kills it only if its instance token still equals `instance`
/// and no connection but the spawning one has attached or used it. There is
/// no unconditional variant here. Needs only a negotiated client.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread; the ID and
/// its host span are readable, and `instance` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_kill_if(
    client: *mut PhuxClient,
    request_id: u32,
    terminal_id: *const PhuxResourceId,
    instance: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if !client.protocol_ready || client.detached {
            return Err(BridgeError::state(
                "a conditional kill needs a negotiated connection",
            ));
        }
        if !client.conditional_kill {
            return Err(BridgeError::state(
                "the server did not advertise CONDITIONAL_KILL; nothing is killed",
            ));
        }
        if instance.is_null() {
            return Err(BridgeError::invalid("instance is null"));
        }
        // SAFETY: caller supplies 16 readable instance bytes.
        let token: [u8; 16] = unsafe { bytes_in(instance, 16) }?
            .try_into()
            .map_err(|_| BridgeError::invalid("instance must be 16 bytes"))?;
        // SAFETY: caller supplies a readable ID and its host span.
        let id = unsafe { terminal_id_in(terminal_id) }?;
        valid_terminal(&id)?;
        ensure_queue_capacity(client, request_id)?;
        client.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::KillResourceIf {
                terminal_id: id.clone(),
                precondition: KillPrecondition::spawned_and_unattached(ServerInstance::new(token)),
                operation_id: None,
            },
        })?;
        client.operations.insert(request_id, Pending::Kill(id));
        Ok(())
    })
}

/// Queue `ACQUIRE_INPUT` for `terminal_id` (ADR-0033).
///
/// Cooperative when `seize` is false, which the server refuses
/// `INPUT_LEASE_HELD` while another client drives; preempting when true.
/// Correlated like every operation (kind `PHUX_OPERATION_ACQUIRE_INPUT`);
/// the lease change itself arrives as `PHUX_CLIENT_STATUS_INPUT_HOLDER`.
/// Needs only a negotiated client.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread; the ID and
/// its host span are readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_acquire_input(
    client: *mut PhuxClient,
    request_id: u32,
    terminal_id: *const PhuxResourceId,
    seize: bool,
) -> PhuxClientResult {
    // SAFETY: forwards the caller's contract.
    unsafe {
        queue_lease(client, request_id, terminal_id, |id| {
            (
                Command::AcquireInput {
                    terminal_id: id.clone(),
                    mode: if seize {
                        InputMode::Seize
                    } else {
                        InputMode::Cooperative
                    },
                    ttl_ms: 0,
                },
                Pending::AcquireInput(id),
            )
        })
    }
}

/// Queue `RELEASE_INPUT` for `terminal_id` (ADR-0033); a no-op server-side
/// when this connection does not hold the lease. Correlated as kind
/// `PHUX_OPERATION_RELEASE_INPUT`.
///
/// # Safety
/// As [`phux_client_queue_acquire_input`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_release_input(
    client: *mut PhuxClient,
    request_id: u32,
    terminal_id: *const PhuxResourceId,
) -> PhuxClientResult {
    // SAFETY: forwards the caller's contract.
    unsafe {
        queue_lease(client, request_id, terminal_id, |id| {
            (
                Command::ReleaseInput {
                    terminal_id: id.clone(),
                },
                Pending::ReleaseInput(id),
            )
        })
    }
}

/// # Safety
/// As [`phux_client_queue_acquire_input`].
pub(super) unsafe fn queue_lease(
    client: *mut PhuxClient,
    request_id: u32,
    terminal_id: *const PhuxResourceId,
    build: impl FnOnce(ResourceId) -> (Command, Pending),
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if !client.protocol_ready || client.detached {
            return Err(BridgeError::state(
                "an input lease needs a negotiated connection",
            ));
        }
        // SAFETY: caller supplies a readable ID and its host span.
        let id = unsafe { terminal_id_in(terminal_id) }?;
        valid_terminal(&id)?;
        ensure_queue_capacity(client, request_id)?;
        let (command, pending) = build(id);
        client.queue_frame(&FrameKind::Command {
            request_id,
            command,
        })?;
        client.operations.insert(request_id, pending);
        Ok(())
    })
}

/// The instance token the completion at `index` carried: a bound spawn's.
///
/// # Safety
/// Client is live and unmodified for the call; `out_instance` is 16 writable
/// bytes and `out_bound` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_operation_instance(
    client: *const PhuxClient,
    index: usize,
    out_instance: *mut u8,
    out_bound: *mut bool,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies a writable flag when non-null.
        let bound = unsafe { out_bound.as_mut() }
            .ok_or_else(|| BridgeError::invalid("bound output is null"))?;
        *bound = false;
        if out_instance.is_null() {
            return Err(BridgeError::invalid("instance output is null"));
        }
        let result = client
            .operations
            .completed
            .get(index)
            .ok_or_else(|| BridgeError {
                result: PhuxClientResult::NoValue,
                message: String::new(),
            })?;
        if let Some(token) = result.instance {
            // SAFETY: caller supplies 16 writable bytes, disjoint from the client.
            unsafe { ptr::copy_nonoverlapping(token.as_ptr(), out_instance, token.len()) };
            *bound = true;
        }
        Ok(())
    })
}

/// Whether `HELLO_OK` advertised `CONDITIONAL_KILL`.
///
/// # Safety
/// Client is live and unmodified for the call; `out_supported` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_conditional_kill_supported(
    client: *const PhuxClient,
    out_supported: *mut bool,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies a writable output when non-null.
        let out = unsafe { out_supported.as_mut() }
            .ok_or_else(|| BridgeError::invalid("supported output is null"))?;
        *out = client.conditional_kill;
        Ok(())
    })
}
