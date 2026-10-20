//! Decoding the positional bodies of individual commands, with their bounds.

use super::{
    AgentEvent, ClientId, Command, DecodeError, Decoder, FileUploadId, InputMode,
    MAX_FILE_UPLOAD_CHUNK, MAX_FILE_UPLOAD_SIZE, ReportedAgentState, ResourceEventType,
    STATE_SCOPE_TAG_SERVER, StateScope, TerminalSignal, decode_asked_event, decode_client_id,
    decode_terminal_id, decode_trailing_key,
};

pub(super) fn decode_resource_ids(
    dec: &mut Decoder<'_>,
) -> Result<Vec<crate::ids::ResourceId>, DecodeError> {
    let count = dec.read_u16_be()? as usize;
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        ids.push(decode_terminal_id(dec)?);
    }
    Ok(ids)
}

pub(super) fn decode_detach_clients_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let session = if dec.read_u8()? != 0 {
        Some(dec.read_str()?.to_owned())
    } else {
        None
    };
    Ok(Command::DetachClients { session })
}

pub(super) fn decode_get_terminal_state_command(
    dec: &mut Decoder<'_>,
) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let include_scrollback = dec.read_u8()? != 0;
    let max_scrollback_lines = dec.read_u16_be()?;
    Ok(Command::GetTerminalState {
        terminal_id,
        include_scrollback,
        max_scrollback_lines,
    })
}

pub(super) fn decode_subscribe_terminal_events_command(
    dec: &mut Decoder<'_>,
) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let count = dec.read_u16_be()? as usize;
    let mut event_types = Vec::with_capacity(count);
    for _ in 0..count {
        if let Some(et) = ResourceEventType::from_u8(dec.read_u8()?) {
            event_types.push(et);
        }
    }
    Ok(Command::SubscribeResourceEvents {
        terminal_id,
        event_types,
    })
}

pub(super) fn decode_acquire_input_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let mode = InputMode::from_u8(dec.read_u8()?).ok_or(DecodeError::UnknownEnumValue {
        field: "InputMode",
        value: 0,
    })?;
    let ttl_ms = dec.read_u32_be()?;
    Ok(Command::AcquireInput {
        terminal_id,
        mode,
        ttl_ms,
    })
}

pub(super) fn decode_signal_terminal_command(
    dec: &mut Decoder<'_>,
) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let signal = TerminalSignal::from_u8(dec.read_u8()?).ok_or(DecodeError::UnknownEnumValue {
        field: "TerminalSignal",
        value: 0,
    })?;
    Ok(Command::SignalTerminal {
        terminal_id,
        signal,
        operation_id: decode_trailing_key(dec)?,
    })
}

pub(super) fn decode_report_agent_state_command(
    dec: &mut Decoder<'_>,
) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let value = dec.read_u8()?;
    let state = ReportedAgentState::from_u8(value)
        .ok_or_else(|| DecodeError::unknown_enum("ReportedAgentState", value))?;
    Ok(Command::ReportAgentState { terminal_id, state })
}

pub(super) fn decode_put_file_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let upload_id = decode_file_upload_id(dec)?;
    let terminal_id = decode_terminal_id(dec)?;
    let extension = decode_put_file_extension(dec)?;
    let offset = dec.read_u64_be()?;
    let data = decode_put_file_chunk(dec, offset)?;
    let final_chunk = dec.read_u8()? != 0;
    let sha256 = decode_optional_sha256(dec)?;
    Ok(Command::PutFile {
        upload_id,
        terminal_id,
        extension,
        offset,
        data,
        final_chunk,
        sha256,
    })
}

pub(super) fn decode_get_file_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let host = if dec.read_u8()? == 0 {
        None
    } else {
        Some(dec.read_str()?.to_owned())
    };
    let path = dec.read_str()?.to_owned();
    let offset = dec.read_u64_be()?;
    let max_bytes = dec.read_u32_be()?;
    if usize::try_from(max_bytes).is_ok_and(|max| max > MAX_FILE_UPLOAD_CHUNK) {
        return Err(DecodeError::FileUploadLimitExceeded);
    }
    Ok(Command::GetFile {
        host,
        path,
        offset,
        max_bytes,
    })
}

pub(super) fn decode_checkout_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let request =
        crate::checkout::decode_request(dec.read_bytes()?).map_err(checkout_decode_error)?;
    Ok(Command::Checkout {
        terminal_id,
        request,
    })
}

pub(super) const fn checkout_decode_error(
    error: crate::checkout::CheckoutDecodeError,
) -> DecodeError {
    match error {
        crate::checkout::CheckoutDecodeError::TooLarge => DecodeError::CheckoutLimitExceeded,
        crate::checkout::CheckoutDecodeError::Invalid => DecodeError::InvalidCheckoutPayload,
    }
}

pub(super) fn decode_file_upload_id(dec: &mut Decoder<'_>) -> Result<FileUploadId, DecodeError> {
    let mut id_bytes = [0; 16];
    for byte in &mut id_bytes {
        *byte = dec.read_u8()?;
    }
    FileUploadId::new(id_bytes).ok_or(DecodeError::InvalidFileUploadId)
}

pub(super) fn decode_put_file_extension(dec: &mut Decoder<'_>) -> Result<String, DecodeError> {
    let extension = dec.read_str()?;
    if extension.len() > 16 {
        return Err(DecodeError::FileUploadLimitExceeded);
    }
    Ok(extension.to_owned())
}

/// Read a `PUT_FILE` chunk within the per-chunk and whole-file caps.
pub(super) fn decode_put_file_chunk(
    dec: &mut Decoder<'_>,
    offset: u64,
) -> Result<Vec<u8>, DecodeError> {
    let data = dec.read_bytes()?;
    let data_len = u64::try_from(data.len()).map_err(|_| DecodeError::FileUploadLimitExceeded)?;
    if data.len() > MAX_FILE_UPLOAD_CHUNK
        || offset
            .checked_add(data_len)
            .is_none_or(|end| end > MAX_FILE_UPLOAD_SIZE)
    {
        return Err(DecodeError::FileUploadLimitExceeded);
    }
    Ok(data.to_vec())
}

pub(super) fn decode_optional_sha256(
    dec: &mut Decoder<'_>,
) -> Result<Option<[u8; 32]>, DecodeError> {
    if dec.read_u8()? == 0 {
        return Ok(None);
    }
    let mut digest = [0; 32];
    for byte in &mut digest {
        *byte = dec.read_u8()?;
    }
    Ok(Some(digest))
}

pub(super) fn decode_report_asked_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let AgentEvent::Asked {
        id,
        question,
        suggestions,
        elapsed_seconds,
    } = decode_asked_event(dec)?
    else {
        unreachable!("decode_asked_event always returns AgentEvent::Asked");
    };
    Ok(Command::ReportAsked {
        terminal_id,
        id,
        question,
        suggestions,
        elapsed_seconds,
    })
}

pub(super) fn decode_optional_client_id(
    dec: &mut Decoder<'_>,
) -> Result<Option<ClientId>, DecodeError> {
    dec.read_option("Option<ClientId> tag", decode_client_id)
}

pub(super) fn decode_state_scope(dec: &mut Decoder<'_>) -> Result<StateScope, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        STATE_SCOPE_TAG_SERVER => Ok(StateScope::Server),
        other => Err(DecodeError::unknown_enum("StateScope", other)),
    }
}
