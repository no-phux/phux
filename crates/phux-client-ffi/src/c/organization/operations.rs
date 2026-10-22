//! C exports: typed organization operations.

use super::{
    BridgeError, Committed, Kind, MAX_MOVE_MANY, PhuxClient, PhuxClientResult, TerminateMode,
    lifetime_in, resource_key_in, session_key_in, submit, with_client_mut,
};

// ----- exports: operations --------------------------------------------------------

/// Move one resource into another session (membership only; it keeps
/// running).
///
/// # Safety
/// Client is live and exclusively accessed; both keys are 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_move(
    client: *mut PhuxClient,
    request_id: u32,
    resource: *const u8,
    to_session: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes for each key.
        let (resource, to) = unsafe { (resource_key_in(resource)?, session_key_in(to_session)?) };
        submit(
            client,
            request_id,
            Kind::Move,
            vec![vec![*resource.as_bytes()]],
            false,
            |control| control.move_resource(resource, to).map(|op| vec![op]),
        )
    })
}

/// Move `count` resources into one session, one atomic batch per chunk of
/// the server's bound, each with its own per-target result.
///
/// # Safety
/// Client is live and exclusively accessed; `resources` is `count * 16`
/// readable bytes; `to_session` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_move_many(
    client: *mut PhuxClient,
    request_id: u32,
    resources: *const u8,
    count: usize,
    to_session: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if resources.is_null() || count == 0 || count > MAX_MOVE_MANY {
            return Err(BridgeError::invalid(
                "a bulk move names 1..1024 readable resource keys",
            ));
        }
        // SAFETY: caller supplies 16 readable bytes for the destination.
        let to = unsafe { session_key_in(to_session) }?;
        let mut keys = Vec::with_capacity(count);
        for index in 0..count {
            // SAFETY: caller supplies `count * 16` readable bytes; `index * 16`
            // stays inside them.
            let key = unsafe { resource_key_in(resources.add(index * 16)) }?;
            if keys.contains(&key) {
                return Err(BridgeError::invalid("a bulk move repeats a resource key"));
            }
            keys.push(key);
        }
        let chunks: Vec<Vec<[u8; 16]>> = keys
            .chunks(phux_protocol::wire::frame::MAX_ORG_BATCH)
            .map(|chunk| chunk.iter().map(|key| *key.as_bytes()).collect())
            .collect();
        submit(
            client,
            request_id,
            Kind::MoveMany,
            chunks,
            false,
            |control| control.move_resources(&keys, to),
        )
    })
}

/// Terminate a resource's current activation here; the result is accepted,
/// and the end arrives as a record change.
///
/// # Safety
/// Client is live and exclusively accessed; `resource` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_terminate(
    client: *mut PhuxClient,
    request_id: u32,
    resource: *const u8,
    force: bool,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let resource = unsafe { resource_key_in(resource) }?;
        let mode = if force {
            TerminateMode::Force
        } else {
            TerminateMode::Graceful
        };
        submit(
            client,
            request_id,
            Kind::Terminate,
            vec![vec![*resource.as_bytes()]],
            false,
            |control| {
                control
                    .terminate_resource(resource, mode)
                    .map(|op| vec![op])
            },
        )
    })
}

/// Remove an inactive resource record.
///
/// # Safety
/// Client is live and exclusively accessed; `resource` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_remove(
    client: *mut PhuxClient,
    request_id: u32,
    resource: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let resource = unsafe { resource_key_in(resource) }?;
        submit(
            client,
            request_id,
            Kind::Remove,
            vec![vec![*resource.as_bytes()]],
            false,
            |control| control.remove_resource(resource).map(|op| vec![op]),
        )
    })
}

/// Start a new activation of an inactive resource from its launch.
///
/// # Safety
/// Client is live and exclusively accessed; `resource` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_relaunch(
    client: *mut PhuxClient,
    request_id: u32,
    resource: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let resource = unsafe { resource_key_in(resource) }?;
        submit(
            client,
            request_id,
            Kind::Relaunch,
            vec![vec![*resource.as_bytes()]],
            false,
            |control| control.relaunch_resource(resource).map(|op| vec![op]),
        )
    })
}

/// Delete an empty session; refused `NOT_EMPTY` while it has members.
///
/// # Safety
/// Client is live and exclusively accessed; `session` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_delete_session(
    client: *mut PhuxClient,
    request_id: u32,
    session: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let session = unsafe { session_key_in(session) }?;
        submit(
            client,
            request_id,
            Kind::DeleteSession,
            vec![vec![*session.as_bytes()]],
            true,
            |control| control.delete_session(session).map(|op| vec![op]),
        )
    })
}

/// Make a session persistent or ephemeral; ephemeral on an empty session
/// deletes it.
///
/// # Safety
/// Client is live and exclusively accessed; `session` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_set_lifetime(
    client: *mut PhuxClient,
    request_id: u32,
    session: *const u8,
    lifetime: u32,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let session = unsafe { session_key_in(session) }?;
        let lifetime = lifetime_in(lifetime)?;
        submit(
            client,
            request_id,
            Kind::SetLifetime,
            vec![vec![*session.as_bytes()]],
            true,
            |control| {
                control
                    .set_session_lifetime(session, lifetime)
                    .map(|op| vec![op])
            },
        )
    })
}

/// Legacy ABI name for clearing an imported session's Intake mark; members stay.
///
/// # Safety
/// Client is live and exclusively accessed; `session` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_inbox_resolve(
    client: *mut PhuxClient,
    request_id: u32,
    session: *const u8,
) -> PhuxClientResult {
    // SAFETY: the legacy entry point has the same caller requirements.
    unsafe { phux_client_org_intake_resolve(client, request_id, session) }
}

/// Clear an imported session's Intake mark; its members and execution stay.
///
/// # Safety
/// Client is live and exclusively accessed; `session` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_intake_resolve(
    client: *mut PhuxClient,
    request_id: u32,
    session: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let session = unsafe { session_key_in(session) }?;
        submit(
            client,
            request_id,
            Kind::IntakeResolve,
            vec![vec![*session.as_bytes()]],
            true,
            |control| control.intake_resolve(session).map(|op| vec![op]),
        )
    })
}

/// Undo a committed, undoable earlier request by its receipts. Never
/// reverses execution.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_undo(
    client: *mut PhuxClient,
    request_id: u32,
    original_request_id: u32,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let original = client
            .org
            .requests
            .get(&original_request_id)
            .ok_or_else(|| BridgeError::invalid("unknown organization request"))?;
        if !original.view.undoable {
            return Err(BridgeError::state(
                "that request is not a committed, undoable edit",
            ));
        }
        let (receipts, keys, is_session): (Vec<Committed>, Vec<Vec<[u8; 16]>>, bool) = (
            original
                .parts
                .iter()
                .filter_map(|part| part.receipt.clone())
                .collect(),
            original
                .parts
                .iter()
                .map(|part| part.keys.clone())
                .collect(),
            original.parts.first().is_some_and(|part| part.is_session),
        );
        submit(
            client,
            request_id,
            Kind::Undo,
            keys,
            is_session,
            |control| {
                receipts
                    .iter()
                    .map(|receipt| control.undo(receipt))
                    .collect()
            },
        )
    })
}

/// Kill a whole session with one `SESSION_KILL`: the server ends every
/// member and deletes the session. Refused `INVALID_STATE` unless the server
/// advertises `SESSION_KILL`.
///
/// # Safety
/// Client is live and exclusively accessed; `session` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_kill_session(
    client: *mut PhuxClient,
    request_id: u32,
    session: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let session = unsafe { session_key_in(session) }?;
        submit(
            client,
            request_id,
            Kind::KillSession,
            vec![vec![*session.as_bytes()]],
            true,
            |control| control.kill_session(session).map(|op| vec![op]),
        )
    })
}

/// Resolve operations past their retry horizon.
///
/// The connected lane's driver does this itself; an embedded host calls it
/// on a timer while an organization operation is pending. `out_next_ms` receives the
/// milliseconds until the next deadline, or `UINT64_MAX` when none is due.
///
/// # Safety
/// Client is live and exclusively accessed; `out_next_ms` is writable when
/// non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_expire_operations(
    client: *mut PhuxClient,
    out_next_ms: *mut u64,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let _ = client.control().expire_operations();
        client.process_runtime_events()?;
        let next = client.control().next_operation_deadline();
        // SAFETY: caller supplies a writable output when non-null.
        if let Some(out) = unsafe { out_next_ms.as_mut() } {
            *out = next.map_or(u64::MAX, |deadline| {
                u64::try_from(
                    deadline
                        .saturating_duration_since(std::time::Instant::now())
                        .as_millis(),
                )
                .unwrap_or(u64::MAX)
            });
        }
        Ok(())
    })
}
