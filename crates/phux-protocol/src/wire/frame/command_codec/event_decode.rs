//! Decoding an `AgentEvent`; an unknown tag or opaque body decodes as `Unknown`.

use super::{
    AgentEvent, ControlAction, DecodeError, Decoder, EVENT_TAG_APPROVAL_DECIDED,
    EVENT_TAG_APPROVAL_REQUESTED, EVENT_TAG_ASKED, EVENT_TAG_BELL, EVENT_TAG_COMMAND_FINISHED,
    EVENT_TAG_COMMAND_STARTED, EVENT_TAG_CWD_CHANGED, EVENT_TAG_DIRTY, EVENT_TAG_IDLE,
    EVENT_TAG_INPUT_ACTIVITY, EVENT_TAG_JOURNAL_GAP, EVENT_TAG_RESOURCE_CLOSED,
    EVENT_TAG_RESOURCE_SPAWNED, EVENT_TAG_SOURCE_GAP, EVENT_TAG_TERMINAL_CONTROL,
    EVENT_TAG_TITLE_CHANGED, ResourceKind, ResourceLifecycle, decode_approval_event,
    decode_optional_client_id, decode_optional_i32, decode_terminal_id, field, utf8_value,
};

pub(super) fn decode_native_result_event(body: &[u8]) -> Result<AgentEvent, DecodeError> {
    if body.len() > crate::native_agent::MAX_NATIVE_PAYLOAD_BYTES {
        return Err(DecodeError::NativeAgentLimitExceeded);
    }
    let mut dec = Decoder::new(body);
    let agent_session_id = decode_terminal_id(&mut dec)?;
    let operation_id = dec.read_u64_be()?;
    let Some(disposition) = crate::native_agent::NativeDisposition::from_u8(dec.read_u8()?) else {
        return Ok(AgentEvent::Unknown {
            tag: crate::wire::frame::EVENT_TAG_NATIVE_AGENT_RESULT,
            body: body.to_vec(),
        });
    };
    // A presence byte, never "whatever bytes remain": a newer peer's bytes
    // after it are skipped.
    let detail = dec.read_option("NativeAgentResult detail", |d| Ok(d.read_str()?.to_owned()))?;
    Ok(AgentEvent::NativeAgentResult {
        agent_session_id,
        operation_id,
        disposition,
        detail,
    })
}

pub(in crate::wire) fn decode_agent_event(
    dec: &mut Decoder<'_>,
) -> Result<AgentEvent, DecodeError> {
    let tag = dec.read_u8()?;
    let body = dec.read_bytes()?;

    let mut body_dec = Decoder::new(body);
    let event = match tag {
        crate::wire::frame::EVENT_TAG_NATIVE_AGENT_RESULT => decode_native_result_event(body)?,
        EVENT_TAG_COMMAND_STARTED => AgentEvent::CommandStarted,
        EVENT_TAG_COMMAND_FINISHED => AgentEvent::CommandFinished {
            exit_code: decode_optional_i32(&mut body_dec)?,
        },
        EVENT_TAG_TITLE_CHANGED => AgentEvent::TitleChanged {
            title: body_dec.read_str()?.to_owned(),
        },
        EVENT_TAG_BELL => AgentEvent::Bell,
        EVENT_TAG_RESOURCE_SPAWNED => decode_pane_spawned_event(&mut body_dec)?,
        EVENT_TAG_RESOURCE_CLOSED => AgentEvent::ResourceClosed {
            exit_status: decode_optional_i32(&mut body_dec)?,
        },
        EVENT_TAG_DIRTY => AgentEvent::Dirty,
        EVENT_TAG_IDLE => AgentEvent::Idle,
        // An unknown lifecycle or action byte makes the event opaque.
        EVENT_TAG_TERMINAL_CONTROL => {
            decode_terminal_control_event(&mut body_dec)?.unwrap_or_else(|| AgentEvent::Unknown {
                tag,
                body: body.to_vec(),
            })
        }
        EVENT_TAG_ASKED => decode_asked_event(&mut body_dec)?,
        EVENT_TAG_CWD_CHANGED => AgentEvent::CwdChanged {
            cwd: body_dec.read_str()?.to_owned(),
        },
        EVENT_TAG_JOURNAL_GAP => decode_journal_gap_event(&mut body_dec)?,
        EVENT_TAG_SOURCE_GAP => decode_source_gap_event(&mut body_dec)?,
        // A zero id or unknown outcome makes the event opaque.
        EVENT_TAG_APPROVAL_REQUESTED | EVENT_TAG_APPROVAL_DECIDED => {
            decode_approval_event(tag, &mut body_dec)?.unwrap_or_else(|| AgentEvent::Unknown {
                tag,
                body: body.to_vec(),
            })
        }
        EVENT_TAG_INPUT_ACTIVITY => AgentEvent::InputActivity {
            origin: body_dec.read_str()?.to_owned(),
        },

        // An organization event whose body is malformed or names an unknown
        // enum value is kept opaque rather than failing the frame.
        other => crate::wire::frame::org::decode_org_event(other, body).unwrap_or_else(|| {
            AgentEvent::Unknown {
                tag: other,
                body: body.to_vec(),
            }
        }),
    };
    Ok(event)
}

/// Decode an [`AgentEvent::TerminalControl`] body (ADR-0033); `Ok(None)` for
/// an unknown lifecycle or action byte.
pub(super) fn decode_terminal_control_event(
    dec: &mut Decoder<'_>,
) -> Result<Option<AgentEvent>, DecodeError> {
    let lifecycle = ResourceLifecycle::from_u8(dec.read_u8()?);
    let exit_status = decode_optional_i32(dec)?;
    let input_holder = decode_optional_client_id(dec)?;
    let action = ControlAction::from_u8(dec.read_u8()?);
    let actor = decode_optional_client_id(dec)?;
    let (Some(lifecycle), Some(action)) = (lifecycle, action) else {
        return Ok(None);
    };
    Ok(Some(AgentEvent::TerminalControl {
        lifecycle,
        exit_status,
        input_holder,
        action,
        actor,
    }))
}

/// Read a field-tagged `u64` value.
pub(super) fn read_u64_value(value: &[u8]) -> Result<u64, DecodeError> {
    Decoder::new(value).read_u64_be()
}

/// Decode an [`AgentEvent::JournalGap`] body; an absent bound reads as `0`.
pub(super) fn decode_journal_gap_event(dec: &mut Decoder<'_>) -> Result<AgentEvent, DecodeError> {
    let mut first_missing = 0;
    let mut last_missing = 0;
    while let Some((field_id, value)) = dec.read_field()? {
        match field_id {
            field::event_journal_gap::FIRST_MISSING => first_missing = read_u64_value(value)?,
            field::event_journal_gap::LAST_MISSING => last_missing = read_u64_value(value)?,
            _ => {}
        }
    }
    Ok(AgentEvent::JournalGap {
        first_missing,
        last_missing,
    })
}

/// Decode an [`AgentEvent::SourceGap`] body (field-tagged TLV).
pub(super) fn decode_source_gap_event(dec: &mut Decoder<'_>) -> Result<AgentEvent, DecodeError> {
    let mut dropped = 0;
    while let Some((field_id, value)) = dec.read_field()? {
        if field_id == field::event_source_gap::DROPPED {
            dropped = read_u64_value(value)?;
        }
    }
    Ok(AgentEvent::SourceGap { dropped })
}

/// Decode an [`AgentEvent::ResourceSpawned`] body; empty is a root Terminal.
pub(super) fn decode_pane_spawned_event(dec: &mut Decoder<'_>) -> Result<AgentEvent, DecodeError> {
    let mut kind = ResourceKind::Terminal;
    let mut parent = None;
    while let Some((field_id, value)) = dec.read_field()? {
        match field_id {
            field::event_pane_spawned::KIND => {
                kind = ResourceKind::from_wire(Decoder::new(value).read_u8()?);
            }
            field::event_pane_spawned::PARENT => {
                parent = Some(decode_terminal_id(&mut Decoder::new(value))?);
            }
            _ => {}
        }
    }
    Ok(AgentEvent::ResourceSpawned { kind, parent })
}

/// Decode an [`AgentEvent::Asked`] body; absent `id` / `question` are empty.
pub(super) fn decode_asked_event(dec: &mut Decoder<'_>) -> Result<AgentEvent, DecodeError> {
    let mut id = String::new();
    let mut question = String::new();
    let mut suggestions = Vec::new();
    let mut elapsed_seconds = None;
    while let Some((field_id, value)) = dec.read_field()? {
        match field_id {
            field::event_asked::ID => id = utf8_value(value)?,
            field::event_asked::QUESTION => question = utf8_value(value)?,
            field::event_asked::SUGGESTION => {
                suggestions.push(utf8_value(value)?);
            }
            field::event_asked::ELAPSED_SECONDS => {
                elapsed_seconds = Some(Decoder::new(value).read_u64_be()?);
            }

            _ => {}
        }
    }
    Ok(AgentEvent::Asked {
        id,
        question,
        suggestions,
        elapsed_seconds,
    })
}
