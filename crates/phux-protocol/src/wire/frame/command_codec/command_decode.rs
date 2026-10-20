//! Decoding a `Command` by its tag, one family of tags per function.

use super::{
    COMMAND_TAG_ACQUIRE_INPUT, COMMAND_TAG_APPEND_RESOURCE_OUTPUT, COMMAND_TAG_APPLY_INPUT,
    COMMAND_TAG_ATTACH_RESOURCE, COMMAND_TAG_CLOSE_TAB_RESOURCES, COMMAND_TAG_DETACH_CLIENTS,
    COMMAND_TAG_DETACH_RESOURCE, COMMAND_TAG_GET_FILE, COMMAND_TAG_GET_PERF,
    COMMAND_TAG_GET_SCREEN, COMMAND_TAG_GET_STATE, COMMAND_TAG_GET_TERMINAL_STATE,
    COMMAND_TAG_KILL_RESOURCE, COMMAND_TAG_KILL_RESOURCE_IF, COMMAND_TAG_KILL_RESOURCES,
    COMMAND_TAG_OPEN_LISTENER, COMMAND_TAG_PERF_CONTROL, COMMAND_TAG_PUT_FILE,
    COMMAND_TAG_RELEASE_INPUT, COMMAND_TAG_REPORT_AGENT_STATE, COMMAND_TAG_REPORT_ASKED,
    COMMAND_TAG_ROUTE_INPUT, COMMAND_TAG_SHUTDOWN, COMMAND_TAG_SIGNAL_TERMINAL,
    COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS, COMMAND_TAG_TRANSCRIBE, COMMAND_TAG_UPGRADE, Command,
    DecodeError, Decoder, INPUT_EVENT_TAG_FOCUS, INPUT_EVENT_TAG_KEY, INPUT_EVENT_TAG_MOUSE,
    INPUT_EVENT_TAG_PASTE, IdempotencyKey, InputEvent, InputOperationId, KillConditions,
    KillPrecondition, ListenerTransport, MAX_APPEND_BYTES, MAX_APPLY_INPUT_COMMAND_BODY,
    MAX_APPLY_INPUT_EVENTS, PerfOp, decode_acquire_input_command, decode_checkout_command,
    decode_detach_clients_command, decode_file_upload_id, decode_focus_event,
    decode_get_file_command, decode_get_terminal_state_command, decode_key_event,
    decode_mouse_event, decode_optional_u32, decode_paste_event, decode_put_file_command,
    decode_report_agent_state_command, decode_report_asked_command, decode_resource_ids,
    decode_signal_terminal_command, decode_state_scope, decode_subscribe_terminal_events_command,
    decode_terminal_id,
};

pub(super) fn decode_input_event(dec: &mut Decoder<'_>) -> Result<InputEvent, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        INPUT_EVENT_TAG_KEY => Ok(InputEvent::Key(decode_key_event(dec)?)),
        INPUT_EVENT_TAG_MOUSE => Ok(InputEvent::Mouse(decode_mouse_event(dec)?)),
        INPUT_EVENT_TAG_FOCUS => Ok(InputEvent::Focus(decode_focus_event(dec.read_u8()?)?)),
        INPUT_EVENT_TAG_PASTE => Ok(InputEvent::Paste(decode_paste_event(dec)?)),
        other => Err(DecodeError::unknown_enum("InputEvent", other)),
    }
}

pub(in crate::wire) fn decode_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let command_body_len = dec.remaining_in_body();
    let tag = dec.read_u8()?;
    if tag == crate::wire::frame::COMMAND_TAG_NATIVE_AGENT {
        let bytes = dec.read_bytes()?;
        if bytes.len() > crate::native_agent::MAX_NATIVE_PAYLOAD_BYTES {
            return Err(DecodeError::NativeAgentLimitExceeded);
        }
        let request = crate::native_agent::NativeRequestPayload::decode(bytes)
            .map_err(|_| DecodeError::InvalidNativeAgentPayload)?;
        return Ok(Command::NativeAgent { request });
    }
    // Each family claims a disjoint tag set and returns `None` (reading
    // nothing) for the rest.
    if let Some(command) = decode_terminal_subscription_command(tag, dec)? {
        return Ok(command);
    }
    if let Some(command) = decode_live_affordance_command(tag, dec, command_body_len)? {
        return Ok(command);
    }
    if let Some(command) = decode_supervisory_command(tag, dec)? {
        return Ok(command);
    }
    if let Some(command) = decode_agent_report_command(tag, dec)? {
        return Ok(command);
    }
    if let Some(command) = decode_session_command(tag, dec)? {
        return Ok(command);
    }
    if let Some(command) = decode_resource_command(tag, dec)? {
        return Ok(command);
    }
    if let Some(command) = crate::wire::frame::org::decode_org_command(tag, dec)? {
        return Ok(command);
    }
    Err(DecodeError::unknown_enum("Command", tag))
}

/// Decode the producer verb `APPEND_RESOURCE_OUTPUT`.
pub(super) fn decode_resource_command(
    tag: u8,
    dec: &mut Decoder<'_>,
) -> Result<Option<Command>, DecodeError> {
    let command = match tag {
        COMMAND_TAG_APPEND_RESOURCE_OUTPUT => decode_append_resource_output_command(dec)?,
        _ => return Ok(None),
    };
    Ok(Some(command))
}

/// Decode `APPEND_RESOURCE_OUTPUT`, refusing an empty or over-
/// [`MAX_APPEND_BYTES`] payload before copying it.
pub(super) fn decode_append_resource_output_command(
    dec: &mut Decoder<'_>,
) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let bytes = dec.read_bytes()?;
    if bytes.is_empty() || bytes.len() > MAX_APPEND_BYTES {
        return Err(DecodeError::AppendResourceOutputLimitExceeded);
    }
    Ok(Command::AppendResourceOutput {
        terminal_id,
        bytes: bytes.to_vec(),
    })
}

/// Decode `ATTACH_RESOURCE`, `DETACH_RESOURCE`, and `KILL_RESOURCE`.
pub(super) fn decode_terminal_subscription_command(
    tag: u8,
    dec: &mut Decoder<'_>,
) -> Result<Option<Command>, DecodeError> {
    let command = match tag {
        COMMAND_TAG_ATTACH_RESOURCE => Command::AttachResource {
            terminal_id: decode_terminal_id(dec)?,
            // Trailing additive role byte (ADR-0127); absent = default.
            role_policy: if dec.at_body_end() {
                None
            } else {
                Some(crate::wire::frame::RolePolicy::from_u8(dec.read_u8()?))
            },
            agent_after_seq: if dec.at_body_end() {
                None
            } else {
                Some(dec.read_u64_be()?)
            },
        },
        COMMAND_TAG_DETACH_RESOURCE => Command::DetachResource {
            terminal_id: decode_terminal_id(dec)?,
        },
        COMMAND_TAG_KILL_RESOURCE => Command::KillResource {
            terminal_id: decode_terminal_id(dec)?,
            operation_id: decode_trailing_key(dec)?,
        },
        COMMAND_TAG_KILL_RESOURCE_IF => Command::KillResourceIf {
            terminal_id: decode_terminal_id(dec)?,
            precondition: decode_kill_precondition(dec)?,
            operation_id: decode_trailing_key(dec)?,
        },
        _ => return Ok(None),
    };
    Ok(Some(command))
}

/// Read the trailing `operation_id` when bytes remain; it must be 16
/// non-zero bytes.
pub(super) fn decode_trailing_key(
    dec: &mut Decoder<'_>,
) -> Result<Option<IdempotencyKey>, DecodeError> {
    if dec.at_body_end() {
        return Ok(None);
    }
    let mut bytes = [0; 16];
    for byte in &mut bytes {
        *byte = dec.read_u8()?;
    }
    IdempotencyKey::new(bytes)
        .map(Some)
        .ok_or(DecodeError::InvalidIdempotencyKey)
}

/// Read a `KILL_RESOURCE_IF` precondition, keeping unknown condition bits so
/// the server can refuse them with a typed error.
pub(super) fn decode_kill_precondition(
    dec: &mut Decoder<'_>,
) -> Result<KillPrecondition, DecodeError> {
    let instance = dec.read_option(
        "Option<ServerInstance> tag",
        crate::wire::frame::codec::decode_server_instance,
    )?;
    let conditions = KillConditions::from_bits(dec.read_u8()?);
    Ok(KillPrecondition {
        instance,
        conditions,
    })
}

/// Decode the agent affordances (SPEC §6). `command_body_len`, measured before
/// the tag byte, bounds `APPLY_INPUT`.
pub(super) fn decode_live_affordance_command(
    tag: u8,
    dec: &mut Decoder<'_>,
    command_body_len: usize,
) -> Result<Option<Command>, DecodeError> {
    let command = match tag {
        COMMAND_TAG_GET_SCREEN => decode_get_screen_command(dec)?,
        COMMAND_TAG_ROUTE_INPUT => Command::RouteInput {
            terminal_id: decode_terminal_id(dec)?,
            event: decode_input_event(dec)?,
        },
        COMMAND_TAG_APPLY_INPUT => decode_apply_input_command(dec, command_body_len)?,
        COMMAND_TAG_GET_TERMINAL_STATE => decode_get_terminal_state_command(dec)?,
        COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS => decode_subscribe_terminal_events_command(dec)?,
        COMMAND_TAG_PUT_FILE => decode_put_file_command(dec)?,
        COMMAND_TAG_GET_FILE => decode_get_file_command(dec)?,
        crate::wire::frame::COMMAND_TAG_CHECKOUT => decode_checkout_command(dec)?,
        COMMAND_TAG_TRANSCRIBE => Command::Transcribe {
            upload_id: decode_file_upload_id(dec)?,
            terminal_id: decode_terminal_id(dec)?,
        },
        _ => return Ok(None),
    };
    Ok(Some(command))
}

/// Decode the ADR-0033 supervisory verbs.
pub(super) fn decode_supervisory_command(
    tag: u8,
    dec: &mut Decoder<'_>,
) -> Result<Option<Command>, DecodeError> {
    let command = match tag {
        COMMAND_TAG_ACQUIRE_INPUT => decode_acquire_input_command(dec)?,
        COMMAND_TAG_RELEASE_INPUT => Command::ReleaseInput {
            terminal_id: decode_terminal_id(dec)?,
        },
        COMMAND_TAG_SIGNAL_TERMINAL => decode_signal_terminal_command(dec)?,
        _ => return Ok(None),
    };
    Ok(Some(command))
}

/// Decode `REPORT_ASKED` (ADR-0036) and `REPORT_AGENT_STATE`.
pub(super) fn decode_agent_report_command(
    tag: u8,
    dec: &mut Decoder<'_>,
) -> Result<Option<Command>, DecodeError> {
    let command = match tag {
        COMMAND_TAG_REPORT_ASKED => decode_report_asked_command(dec)?,
        COMMAND_TAG_REPORT_AGENT_STATE => decode_report_agent_state_command(dec)?,
        _ => return Ok(None),
    };
    Ok(Some(command))
}

/// Decode the session- and server-scoped commands.
pub(super) fn decode_session_command(
    tag: u8,
    dec: &mut Decoder<'_>,
) -> Result<Option<Command>, DecodeError> {
    let command = match tag {
        COMMAND_TAG_GET_STATE => Command::GetState {
            scope: decode_state_scope(dec)?,
        },
        COMMAND_TAG_KILL_RESOURCES => Command::KillResources {
            ids: decode_resource_ids(dec)?,
            operation_id: decode_trailing_key(dec)?,
        },
        COMMAND_TAG_CLOSE_TAB_RESOURCES => Command::CloseTabResources {
            ids: decode_resource_ids(dec)?,
        },
        COMMAND_TAG_DETACH_CLIENTS => decode_detach_clients_command(dec)?,
        COMMAND_TAG_UPGRADE => Command::Upgrade,
        COMMAND_TAG_SHUTDOWN => Command::Shutdown,
        COMMAND_TAG_OPEN_LISTENER => decode_open_listener_command(dec)?,
        COMMAND_TAG_GET_PERF => Command::GetPerf {
            reset: dec.read_u8()? != 0,
        },
        // Total: an undefined op decodes so the server can refuse it by name.
        COMMAND_TAG_PERF_CONTROL => Command::PerfControl {
            op: PerfOp::from_u8(dec.read_u8()?),
        },
        _ => return Ok(None),
    };
    Ok(Some(command))
}

/// Decode an `OPEN_LISTENER` body. Total: unknown transports and bad ranges
/// decode so the server can refuse them with a message; `0, 0` is any port.
pub(super) fn decode_open_listener_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let transport = ListenerTransport::from_u8(dec.read_u8()?);
    let min = dec.read_u16_be()?;
    let max = dec.read_u16_be()?;
    let linger_secs = dec.read_u32_be()?;
    Ok(Command::OpenListener {
        transport,
        port_range: (min != 0 || max != 0).then_some((min, max)),
        linger_secs,
    })
}

pub(super) fn decode_get_screen_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let request_scrollback = decode_optional_u32(dec)?;
    // `cells` and `format` are trailing additive bytes; absent = `false` / `0`.
    let cells = if dec.at_body_end() {
        false
    } else {
        dec.read_u8()? != 0
    };

    let format = if dec.at_body_end() { 0 } else { dec.read_u8()? };
    Ok(Command::GetScreen {
        terminal_id,
        request_scrollback,
        cells,
        format,
    })
}

/// Decode `APPLY_INPUT`, bounded on the body length and the event count.
pub(super) fn decode_apply_input_command(
    dec: &mut Decoder<'_>,
    command_body_len: usize,
) -> Result<Command, DecodeError> {
    if command_body_len > MAX_APPLY_INPUT_COMMAND_BODY {
        return Err(DecodeError::ApplyInputLimitExceeded);
    }
    let mut bytes = [0; 16];
    for byte in &mut bytes {
        *byte = dec.read_u8()?;
    }
    let operation_id = InputOperationId::new(bytes).ok_or(DecodeError::InvalidInputOperationId)?;
    let terminal_id = decode_terminal_id(dec)?;
    let count = dec.read_u16_be()? as usize;
    if count > MAX_APPLY_INPUT_EVENTS {
        return Err(DecodeError::ApplyInputLimitExceeded);
    }
    let mut events = dec.bounded_capacity(count);
    for _ in 0..count {
        events.push(decode_input_event(dec)?);
    }
    Ok(Command::ApplyInput {
        operation_id,
        terminal_id,
        events,
    })
}
