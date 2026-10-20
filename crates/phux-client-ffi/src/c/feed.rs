//! Feeding server frames: decode, validate, and dispatch to the control plane.

use phux_protocol::caps::BootstrapLimits;
use phux_protocol::wire::frame::FrameKind;

use super::{
    BridgeError, Client, PhuxClient, PhuxClientResult, arrangement, bytes_in, directory,
    invoke_attached, operations, projection, session_create, session_query, session_rename,
    with_client_mut,
};

/// Processes one complete server frame through the runtime control plane.
///
/// # Safety
/// `client` is exclusively owned for the call and `data` is readable for `len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_feed_frame(
    client: *mut PhuxClient,
    data: *const u8,
    len: usize,
) -> PhuxClientResult {
    let mut notify_attached = false;
    let result = with_client_mut(client, |client| {
        if client.is_connected_lane() {
            return Err(BridgeError::state(
                "this client's runtime owns the socket; poll instead of feeding",
            ));
        }
        let data = unsafe { bytes_in(data, len) }?;
        let frame = decode_server_frame(client, data)?;
        notify_attached = apply_server_frame(client, frame, None)?;
        Ok(())
    });
    if result == PhuxClientResult::Ok && notify_attached {
        invoke_attached(client)
    } else {
        result
    }
}

pub(super) fn decode_server_frame(client: &Client, data: &[u8]) -> Result<FrameKind, BridgeError> {
    let limits = client.control().decode_limits().unwrap_or_else(|| {
        BootstrapLimits::new(client.limits.bootstrap_chunk, client.limits.history_page)
            .unwrap_or_default()
    });
    decode_server_frame_with_limits(data, limits)
}

fn decode_server_frame_with_limits(
    data: &[u8],
    limits: BootstrapLimits,
) -> Result<FrameKind, BridgeError> {
    let (frame, tail) = FrameKind::decode_with_limits(data, limits)
        .map_err(|error| BridgeError::protocol(error.to_string()))?;
    if !tail.is_empty() {
        return Err(BridgeError::protocol(
            "feed_frame accepts exactly one complete frame",
        ));
    }
    Ok(frame)
}

pub(super) fn apply_server_frame(
    client: &mut Client,
    frame: FrameKind,
    epoch: Option<u64>,
) -> Result<bool, BridgeError> {
    if epoch.is_some_and(|epoch| !client.control().accepts_inbound(epoch)) {
        return Ok(false);
    }
    if consume_retired_close(client, &frame) {
        return Ok(false);
    }
    let validation = validate_runtime_frame(client, &frame);
    let runtime_result = {
        let mut control = client.control();
        if epoch.is_some_and(|epoch| !control.accepts_inbound(epoch)) {
            return Ok(false);
        }
        validation?;
        let result = control.feed(frame);
        if let Err(phux_client_runtime::control::ControlError::Refused(message)) = &result {
            control.fail(message.clone());
        }
        drop(control);
        result.map_err(control_error)
    };
    let attached = client.process_runtime_events()?;
    runtime_result?;
    follow_negotiated_session(client)?;
    Ok(attached)
}

pub(super) fn follow_negotiated_session(client: &mut Client) -> Result<(), BridgeError> {
    if matches!(
        client.control().status(),
        phux_client_runtime::control::Status::Negotiated
    ) {
        session_rename::negotiated(client)?;
    }
    Ok(())
}

fn consume_retired_close(client: &mut Client, frame: &FrameKind) -> bool {
    let FrameKind::ResourceClosed { terminal_id, .. } = frame else {
        return false;
    };
    if !client.arrangement.subscriptions.was_withdrawn(terminal_id)
        && !client.arrangement.subscriptions.was_closed(terminal_id)
    {
        return false;
    }
    client.arrangement.subscriptions.cancel(terminal_id);
    client.arrangement.subscriptions.mark_closed(terminal_id);
    client
        .resources
        .retain(|resource| &resource.id != terminal_id);
    true
}

pub(super) fn validate_runtime_frame(
    client: &Client,
    frame: &FrameKind,
) -> Result<(), BridgeError> {
    if client.detached {
        return Err(BridgeError::protocol("server frame arrived after DETACHED"));
    }
    let terminal = match frame {
        FrameKind::BootstrapBegin {
            terminal_id,
            profile,
            ..
        } => {
            let agent_profile = matches!(
                profile,
                phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1
                    | phux_protocol::BootstrapStreamProfile::AgentEventsJsonlResumeV1 { .. }
            );
            if client.is_agent_stream(terminal_id) != agent_profile {
                return Err(BridgeError::protocol(
                    "bootstrap profile does not match the declared resource kind",
                ));
            }
            Some(terminal_id)
        }
        FrameKind::BootstrapChunk { terminal_id, .. }
        | FrameKind::BootstrapReady { terminal_id, .. }
        | FrameKind::BootstrapTombstone { terminal_id, .. }
        | FrameKind::ResourceOutput { terminal_id, .. }
        | FrameKind::HistoryPage { terminal_id, .. }
        | FrameKind::HistoryTombstone { terminal_id, .. }
        | FrameKind::HistoryRejected { terminal_id, .. }
        | FrameKind::ResourceClosed { terminal_id, .. } => Some(terminal_id),
        _ => None,
    };
    if let Some(terminal) = terminal {
        let retired_close = matches!(frame, FrameKind::ResourceClosed { .. })
            && (client.arrangement.subscriptions.was_withdrawn(terminal)
                || client.arrangement.subscriptions.was_closed(terminal));
        if !retired_close {
            client.ensure_participant(terminal)?;
        }
    }
    Ok(())
}

/// Feed runtime-owned values through the binding-specific extension projections.
pub(super) fn dispatch_extension_frame(
    client: &mut Client,
    frame: FrameKind,
) -> Result<(), BridgeError> {
    let Some(frame) = directory::dispatch(client, frame) else {
        return Ok(());
    };
    let Some(frame) = session_create::dispatch(client, frame)? else {
        return Ok(());
    };
    let Some(frame) = session_query::dispatch(client, frame)? else {
        return Ok(());
    };
    let Some(frame) = session_rename::dispatch(client, frame)? else {
        return Ok(());
    };
    let Some(frame) = projection::dispatch(client, frame) else {
        return Ok(());
    };
    let Some(frame) = operations::dispatch_parts(client, frame)? else {
        return Ok(());
    };
    if arrangement::resources::dispatch(client, &frame)? {
        return Ok(());
    }
    let Some(frame) = arrangement::dispatch(client, frame) else {
        return Ok(());
    };
    let _ = operations::dispatch(client, frame)?;
    Ok(())
}

pub(super) fn forget_terminal(client: &mut Client, id: &phux_protocol::ResourceId) {
    client.render.remove(id);
    client.resources.retain(|resource| &resource.id != id);
}

pub(super) fn control_error(error: phux_client_runtime::control::ControlError) -> BridgeError {
    use phux_client_runtime::control::ControlError;
    match error {
        ControlError::Protocol(message) | ControlError::Refused(message) => {
            BridgeError::protocol(message)
        }
        ControlError::InvalidState(message) => BridgeError::state(message),
        ControlError::Resync => BridgeError::state("a replica needs a fresh bootstrap"),
        ControlError::Closed => BridgeError::state("the session was detached"),
    }
}
