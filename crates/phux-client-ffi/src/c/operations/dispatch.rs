//! Settling operations from server replies.

use super::{
    BridgeError, Client, CommandResult, DeferredSends, ERROR_DOMAIN_ORGANIZATION, FrameKind,
    InputEligibility, KernelSend, Pending, ResourceId, SatelliteHost, ServerInstance, SpawnError,
    SpawnResult, valid_terminal,
};
use phux_protocol::wire::frame::ErrorCode;

/// `PHUX_OPERATION_UNKNOWN_OUTCOME`: the server answered without saying
/// whether the operation took effect; reconcile, never retry creation.
const UNKNOWN_OUTCOME: u32 = 3;

/// Answers to the bridge-internal parts of a close terminated one terminal
/// at a time, re-addressed to the host request they belong to. Runs ahead
/// of the arrangement dispatcher, which owns every other internal id. A part
/// of a request that already settled is consumed silently.
pub(crate) fn dispatch_parts(
    client: &mut Client,
    frame: FrameKind,
) -> Result<Option<FrameKind>, BridgeError> {
    let (part, frame) = match frame {
        FrameKind::CommandResult { request_id, result }
            if client.operations.parts.contains_key(&request_id) =>
        {
            let host = client.operations.parts.remove(&request_id);
            (
                host,
                host.map(|host| FrameKind::CommandResult {
                    request_id: host,
                    result,
                }),
            )
        }
        FrameKind::Error {
            request_id: Some(request_id),
            code,
            message,
        } if client.operations.parts.contains_key(&request_id) => {
            let host = client.operations.parts.remove(&request_id);
            (
                host,
                host.map(|host| FrameKind::Error {
                    request_id: Some(host),
                    code,
                    message,
                }),
            )
        }
        other => return Ok(Some(other)),
    };
    if let (Some(host), Some(frame)) = (part, frame)
        && client.operations.pending.contains_key(&host)
    {
        dispatch(client, frame)?;
    }
    Ok(None)
}

pub(crate) fn dispatch(
    client: &mut Client,
    frame: FrameKind,
) -> Result<Option<FrameKind>, BridgeError> {
    match frame {
        FrameKind::ResourceSpawned { request_id, result } => {
            complete_spawn(client, request_id, result)?;
        }
        FrameKind::CommandResult { request_id, result } => {
            complete_subscription(client, request_id, result)?;
        }
        FrameKind::Error {
            request_id: Some(request_id),
            code,
            message,
        } => {
            let pending = client.operations.pending(request_id)?;
            // A spawn answered with a code this build does not know
            // (`UNREADABLE_REPLY` included) may have created its pane.
            if matches!(pending, Pending::Spawn { .. }) && matches!(code, ErrorCode::Unknown(_)) {
                client.operations.complete(
                    request_id,
                    UNKNOWN_OUTCOME,
                    None,
                    2,
                    u32::from(code.as_wire()),
                    &message,
                );
            } else {
                refuse_operation(client, request_id, 2, u32::from(code.as_wire()), &message)?;
            }
        }
        other => return Ok(Some(other)),
    }
    Ok(None)
}

pub(super) fn complete_spawn(
    client: &mut Client,
    request_id: u32,
    result: SpawnResult,
) -> Result<(), BridgeError> {
    let Pending::Spawn { satellite } = client.operations.pending(request_id)? else {
        return Err(BridgeError::protocol(
            "RESOURCE_SPAWNED does not answer a spawn",
        ));
    };
    // A bound spawn's reply names the id space its terminal came from
    // (phux_client_operation_instance); an unbound one carries none.
    let instance = result.instance().map(|token| *token.as_bytes());
    match result {
        SpawnResult::Ok(id) | SpawnResult::OkBound { id, .. } => {
            validate_spawn_reply(client, &id, satellite.as_ref())?;
            if matches!(id, ResourceId::Local { .. }) {
                if !client.control().admit_external_spawn(&id) {
                    return Err(BridgeError::protocol(
                        "spawn reply reused a runtime-admitted terminal ID",
                    ));
                }
                client.operations.dynamic.insert(id.clone());
            }
            if let Some(token) = instance {
                client
                    .operations
                    .instances
                    .insert(id.clone(), ServerInstance::new(token));
            }
            client
                .operations
                .complete(request_id, 1, Some(id), 0, 0, "");
            client.operations.bind_last(instance);
        }
        // A newer server's refusal does not say whether a pane exists.
        SpawnResult::Err(SpawnError::Unknown(tag)) => client.operations.complete(
            request_id,
            UNKNOWN_OUTCOME,
            None,
            1,
            u32::from(tag.tag()),
            "the spawn's outcome is unknown: a newer server's answer",
        ),
        SpawnResult::Err(error) => {
            let (code, message) = spawn_error(error);
            client
                .operations
                .complete(request_id, 2, None, 1, code, &message);
        }
        _ => return Err(BridgeError::protocol("unknown spawn result")),
    }
    Ok(())
}

pub(super) fn validate_spawn_reply(
    client: &Client,
    id: &ResourceId,
    satellite: Option<&SatelliteHost>,
) -> Result<(), BridgeError> {
    valid_terminal(id).map_err(|error| BridgeError::protocol(error.message))?;
    let host = match id {
        ResourceId::Local { .. } => None,
        ResourceId::Satellite { host, .. } => Some(host),
    };
    if host != satellite {
        return Err(BridgeError::protocol(
            "spawn reply host differs from request",
        ));
    }
    if client.control().terminal_is_admitted(id) || client.operations.admitted(id) {
        return Err(BridgeError::protocol(
            "spawn reply reused an admitted terminal ID",
        ));
    }
    Ok(())
}

pub(super) fn spawn_error(error: SpawnError) -> (u32, String) {
    match error {
        SpawnError::GroupNotFound => (0, "group not found".into()),
        SpawnError::SpawnFailed(message) => (1, message),
        SpawnError::UnsupportedSatelliteRoute => (2, "unsupported satellite route".into()),
        SpawnError::SatelliteUnreachable(message) => (3, message),
        _ => (u32::MAX, "unknown spawn refusal".into()),
    }
}

pub(super) fn complete_subscription(
    client: &mut Client,
    request_id: u32,
    result: CommandResult,
) -> Result<(), BridgeError> {
    if matches!(
        client.operations.pending(request_id)?,
        Pending::Spawn { .. }
    ) {
        return Err(BridgeError::protocol(
            "COMMAND_RESULT does not answer a terminal subscription operation",
        ));
    }
    match result {
        // A `RESOURCE_TERMINATE` close is accepted, not committed: its end
        // arrives as `RESOURCE_CLOSED`, exactly like a kill's.
        CommandResult::Accepted { .. }
            if matches!(
                client.operations.pending(request_id)?,
                Pending::Close(_) | Pending::CloseMany(_)
            ) =>
        {
            client.operations.acknowledge_close(request_id)?;
        }
        CommandResult::Refused(refusal) => {
            let code = crate::projection::organization::refusal_code(&refusal);
            let message = phux_client_core::org_ops::refusal_sentence(&refusal);
            refuse_operation(
                client,
                request_id,
                ERROR_DOMAIN_ORGANIZATION,
                code,
                &message,
            )?;
        }
        CommandResult::Ok => {
            if client.operations.acknowledge_close(request_id)? {
                return Ok(());
            }
            match client.operations.pending(request_id)?.clone() {
                Pending::Detach(id) => complete_detach(client, &id)?,
                Pending::Kill(id) => {
                    // A never-attached satellite may send no RESOURCE_CLOSED.
                    // Forget only binding metadata, never subscription admission.
                    client.operations.instances.remove(&id);
                }
                _ => {}
            }
            client.operations.complete(request_id, 1, None, 0, 0, "");
        }
        CommandResult::Error { code, message } => {
            refuse_operation(client, request_id, 2, u32::from(code.as_wire()), &message)?;
        }
        _ => {
            return Err(BridgeError::protocol(
                "unexpected terminal subscription result value",
            ));
        }
    }
    Ok(())
}

pub(super) fn complete_detach(client: &mut Client, id: &ResourceId) -> Result<(), BridgeError> {
    client.operations.retire(id);
    if !client.release_terminal(id) {
        return Err(BridgeError::state("cannot detach before ATTACH barrier"));
    }
    crate::c::forget_terminal(client, id);
    client
        .owned_effects
        .push(crate::c::OwnedEffect::simple(1, 3, id.clone()));
    client.publish_effects();
    Ok(())
}

pub(super) fn refuse_operation(
    client: &mut Client,
    request_id: u32,
    domain: u32,
    code: u32,
    message: &str,
) -> Result<(), BridgeError> {
    let deferred = match client.operations.pending(request_id)? {
        Pending::Detach(id) => {
            let id = id.clone();
            client.operations.deferred_sends.remove(&id)
        }
        _ => None,
    };
    if let Pending::Attach(id) = client.operations.pending(request_id)?.clone() {
        // A bootstrap may already have published before refusal. Revoke only this operation's admission.
        let published = client.has_projection(&id);
        release_terminal(client, &id)?;
        crate::c::forget_terminal(client, &id);
        if published {
            client
                .owned_effects
                .push(crate::c::OwnedEffect::simple(1, 3, id));
            client.publish_effects();
        }
    }
    client
        .operations
        .complete(request_id, 2, None, domain, code, message);
    resume_deferred(client, deferred)
}

pub(super) fn resume_deferred(
    client: &mut Client,
    deferred: Option<DeferredSends>,
) -> Result<(), BridgeError> {
    if let Some(deferred) = deferred {
        for send in deferred.ack.into_iter().chain(deferred.history) {
            if deferred_send_is_current(client, &send) {
                client.queue_frame(&kernel_send_frame(send)?)?;
            }
        }
    }
    Ok(())
}

/// The explicit admission gate replaces permanent kernel death records for
/// dynamic subscriptions. Initial ATTACH participants retain the kernel barrier.
pub(crate) fn release_terminal(client: &mut Client, id: &ResourceId) -> Result<(), BridgeError> {
    client.operations.deferred_sends.remove(id);
    if !client.operations.admitted(id) {
        return Ok(());
    }
    client.operations.retire(id);
    if !client.release_terminal(id) {
        return Err(BridgeError::state(
            "cannot release an initial ATTACH participant",
        ));
    }
    Ok(())
}

pub(crate) fn kernel_send_frame(send: KernelSend) -> Result<FrameKind, BridgeError> {
    match send {
        KernelSend::Input { terminal_id, event } => Ok(match event {
            phux_protocol::input::InputEvent::Key(event) => {
                FrameKind::InputKey { terminal_id, event }
            }
            phux_protocol::input::InputEvent::Mouse(event) => {
                FrameKind::InputMouse { terminal_id, event }
            }
            phux_protocol::input::InputEvent::Focus(event) => {
                FrameKind::InputFocus { terminal_id, event }
            }
            phux_protocol::input::InputEvent::Paste(event) => {
                FrameKind::InputPaste { terminal_id, event }
            }
            _ => return Err(BridgeError::engine("unsupported deferred input event")),
        }),
        KernelSend::FrameAck {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
        } => Ok(FrameKind::FrameAck {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
        }),
        KernelSend::HistoryRequest {
            key,
            cursor,
            max_bytes,
            max_rows,
        } => Ok(FrameKind::HistoryRequest {
            terminal_id: key.terminal_id,
            stream_id: key.stream_id,
            bootstrap_id: key.bootstrap_id,
            cursor: cursor.into(),
            max_bytes,
            max_rows,
        }),
        KernelSend::SubscribeEvents {
            terminal,
            after_seq,
        } => Ok(FrameKind::SubscribeEvents {
            terminal,
            after_seq,
        }),
    }
}

pub(super) fn deferred_send_is_current(client: &Client, send: &KernelSend) -> bool {
    match send {
        KernelSend::HistoryRequest { key, cursor, .. } => {
            history_request_is_current(client, key, cursor)
        }
        KernelSend::FrameAck {
            terminal_id,
            stream_id,
            bootstrap_id,
            ..
        } => {
            matches!(
                client.input_eligibility(terminal_id),
                InputEligibility::Eligible { .. }
            ) && client
                .terminal_key(terminal_id)
                .is_ok_and(|key| key.stream_id == *stream_id && key.bootstrap_id == *bootstrap_id)
        }
        _ => false,
    }
}

pub(super) fn history_request_is_current(
    client: &Client,
    key: &phux_client_core::session::ReplicaKey,
    cursor: &[u8],
) -> bool {
    if !matches!(
        client.input_eligibility(&key.terminal_id),
        InputEligibility::Eligible { .. }
    ) || !client
        .terminal_key(&key.terminal_id)
        .is_ok_and(|current| current == *key)
    {
        return false;
    }
    client
        .control()
        .engine()
        .and_then(|engine| engine.replica_info(&key.terminal_id).ok())
        .and_then(|info| info.history)
        .and_then(|history| history.next_cursor)
        .is_some_and(|current| current.matches_bytes(cursor))
}
