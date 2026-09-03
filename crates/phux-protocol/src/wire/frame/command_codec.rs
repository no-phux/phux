//! Control-plane command, command-result, and agent-event codecs —
//! SPEC §5 (phux-k61 / ADR-0021) and SPEC §7.5 / §10.3 (phux-y2t).

use bytes::BytesMut;

use crate::ids::{ClientId, FileUploadId, GroupId, InputOperationId};
use crate::input::InputEvent;
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::field;
use crate::wire::info::{
    decode_client_id, decode_session_snapshot, encode_client_id, encode_session_snapshot,
};

use super::codec::encode_optional_u32;
use super::{
    AgentEvent, COMMAND_RESULT_TAG_ERROR, COMMAND_RESULT_TAG_OK, COMMAND_RESULT_TAG_OK_WITH,
    COMMAND_TAG_ACQUIRE_INPUT, COMMAND_TAG_APPLY_INPUT, COMMAND_TAG_ATTACH_TERMINAL,
    COMMAND_TAG_DETACH_CLIENTS, COMMAND_TAG_DETACH_TERMINAL, COMMAND_TAG_GET_PERF,
    COMMAND_TAG_GET_SCREEN, COMMAND_TAG_GET_STATE, COMMAND_TAG_GET_TERMINAL_STATE,
    COMMAND_TAG_KILL_TERMINAL, COMMAND_TAG_KILL_TERMINALS, COMMAND_TAG_PUT_FILE,
    COMMAND_TAG_RELEASE_INPUT, COMMAND_TAG_REPORT_AGENT_STATE, COMMAND_TAG_REPORT_ASKED,
    COMMAND_TAG_ROUTE_INPUT, COMMAND_TAG_SHUTDOWN, COMMAND_TAG_SIGNAL_TERMINAL,
    COMMAND_TAG_SUBSCRIBE_TERMINAL_EVENTS, COMMAND_TAG_UPGRADE, COMMAND_VALUE_TAG_BYTES,
    COMMAND_VALUE_TAG_FILE_UPLOAD, COMMAND_VALUE_TAG_GROUP_ID, COMMAND_VALUE_TAG_JSON,
    COMMAND_VALUE_TAG_STATE, COMMAND_VALUE_TAG_TERMINAL_ID, Command, CommandResult, CommandValue,
    ControlAction, EVENT_TAG_ASKED, EVENT_TAG_BELL, EVENT_TAG_COMMAND_FINISHED,
    EVENT_TAG_COMMAND_STARTED, EVENT_TAG_CWD_CHANGED, EVENT_TAG_DIRTY, EVENT_TAG_IDLE,
    EVENT_TAG_PANE_CLOSED, EVENT_TAG_PANE_SPAWNED, EVENT_TAG_TERMINAL_CONTROL,
    EVENT_TAG_TITLE_CHANGED, ErrorCode, FileUploadAck, INPUT_EVENT_TAG_FOCUS, INPUT_EVENT_TAG_KEY,
    INPUT_EVENT_TAG_MOUSE, INPUT_EVENT_TAG_PASTE, InputMode, MAX_APPLY_INPUT_COMMAND_BODY,
    MAX_APPLY_INPUT_EVENTS, MAX_FILE_UPLOAD_CHUNK, MAX_FILE_UPLOAD_SIZE, ReportedAgentState,
    STATE_SCOPE_TAG_SERVER, StateScope, TerminalEventType, TerminalLifecycle, TerminalSignal,
    decode_focus_event, decode_key_event, decode_mouse_event, decode_optional_u32,
    decode_paste_event, decode_terminal_id, encode_focus_event, encode_key_event,
    encode_mouse_event, encode_paste_event, encode_terminal_id,
};

// -----------------------------------------------------------------------------
// Control-plane command codec — SPEC §5 (phux-k61 / ADR-0021).
//
// COMMAND body:        u32 request_id, then Command (tag + body).
// COMMAND_RESULT body: u32 request_id, then CommandResult (tag + body).
//
// Command tags follow the SPEC §5.1 catalog order; KILL_TERMINAL (0x03)
// and GET_STATE (0x05) are wired in v0.1, plus the appended GET_SCREEN
// (0x07, after RUN_HOOK's reserved 0x06), ROUTE_INPUT (0x08), and
// KILL_TERMINALS (0x09, reusing the slot freed by the v0.3.0 dissolution
// of the L2 lifecycle verbs). CommandResult / CommandValue tags use the
// same `Ok = 0x00` / sequential convention as the rest of the wire.
// -----------------------------------------------------------------------------

#[allow(
    clippy::too_many_lines,
    reason = "one match arm per Command wire tag; the dispatch is a flat encode table, clearer whole than split"
)]
pub(in crate::wire) fn encode_command(command: &Command, enc: &mut Encoder<'_>) {
    match command {
        Command::AttachTerminal { terminal_id } => {
            enc.write_u8(COMMAND_TAG_ATTACH_TERMINAL);
            encode_terminal_id(terminal_id, enc);
        }
        Command::DetachTerminal { terminal_id } => {
            enc.write_u8(COMMAND_TAG_DETACH_TERMINAL);
            encode_terminal_id(terminal_id, enc);
        }
        Command::KillTerminal { terminal_id } => {
            enc.write_u8(COMMAND_TAG_KILL_TERMINAL);
            encode_terminal_id(terminal_id, enc);
        }
        Command::GetState { scope } => {
            enc.write_u8(COMMAND_TAG_GET_STATE);
            encode_state_scope(scope, enc);
        }
        Command::GetScreen {
            terminal_id,
            request_scrollback,
            cells,
        } => {
            enc.write_u8(COMMAND_TAG_GET_SCREEN);
            encode_terminal_id(terminal_id, enc);
            encode_optional_u32(*request_scrollback, enc);
            enc.write_u8(u8::from(*cells));
        }
        Command::RouteInput { terminal_id, event } => {
            enc.write_u8(COMMAND_TAG_ROUTE_INPUT);
            encode_terminal_id(terminal_id, enc);
            encode_input_event(event, enc);
        }
        Command::ApplyInput {
            operation_id,
            terminal_id,
            events,
        } => {
            enc.write_u8(COMMAND_TAG_APPLY_INPUT);
            for byte in operation_id.as_bytes() {
                enc.write_u8(*byte);
            }
            encode_terminal_id(terminal_id, enc);
            enc.write_u16_be(u16::try_from(events.len()).unwrap_or(u16::MAX));
            for event in events {
                encode_input_event(event, enc);
            }
        }
        Command::KillTerminals { ids } => {
            enc.write_u8(COMMAND_TAG_KILL_TERMINALS);
            // Length-prefixed list: u16 count, then each tagged TerminalId.
            // u16 is ample — a single kill-group never approaches 65 535
            // panes — and matches the count-prefix width used elsewhere
            // (e.g. `SubscribeTerminalEvents.event_types`).
            enc.write_u16_be(u16::try_from(ids.len()).unwrap_or(u16::MAX));
            for id in ids {
                encode_terminal_id(id, enc);
            }
        }
        Command::DetachClients { session } => {
            enc.write_u8(COMMAND_TAG_DETACH_CLIENTS);
            // Presence byte + optional session name (u32-BE-len-prefixed
            // UTF-8 via `write_str`), mirroring the other optional-string
            // args.
            match session {
                Some(name) => {
                    enc.write_u8(1);
                    enc.write_str(name);
                }
                None => enc.write_u8(0),
            }
        }
        Command::GetTerminalState {
            terminal_id,
            include_scrollback,
            max_scrollback_lines,
        } => {
            enc.write_u8(COMMAND_TAG_GET_TERMINAL_STATE);
            encode_terminal_id(terminal_id, enc);
            enc.write_u8(u8::from(*include_scrollback));
            enc.write_u16_be(*max_scrollback_lines);
        }
        Command::SubscribeTerminalEvents {
            terminal_id,
            event_types,
        } => {
            enc.write_u8(COMMAND_TAG_SUBSCRIBE_TERMINAL_EVENTS);
            encode_terminal_id(terminal_id, enc);
            enc.write_u16_be(u16::try_from(event_types.len()).unwrap_or(0));
            for et in event_types {
                enc.write_u8(et.to_u8());
            }
        }
        Command::Upgrade => {
            enc.write_u8(COMMAND_TAG_UPGRADE);
        }
        Command::Shutdown => {
            enc.write_u8(COMMAND_TAG_SHUTDOWN);
        }
        Command::GetPerf { reset } => {
            enc.write_u8(COMMAND_TAG_GET_PERF);
            enc.write_u8(u8::from(*reset));
        }
        Command::AcquireInput {
            terminal_id,
            mode,
            ttl_ms,
        } => {
            enc.write_u8(COMMAND_TAG_ACQUIRE_INPUT);
            encode_terminal_id(terminal_id, enc);
            enc.write_u8(mode.to_u8());
            enc.write_u32_be(*ttl_ms);
        }
        Command::ReleaseInput { terminal_id } => {
            enc.write_u8(COMMAND_TAG_RELEASE_INPUT);
            encode_terminal_id(terminal_id, enc);
        }
        Command::SignalTerminal {
            terminal_id,
            signal,
        } => {
            enc.write_u8(COMMAND_TAG_SIGNAL_TERMINAL);
            encode_terminal_id(terminal_id, enc);
            enc.write_u8(signal.to_u8());
        }
        Command::PutFile {
            upload_id,
            terminal_id,
            extension,
            offset,
            data,
            final_chunk,
            sha256,
        } => {
            enc.write_u8(COMMAND_TAG_PUT_FILE);
            for byte in upload_id.as_bytes() {
                enc.write_u8(*byte);
            }
            encode_terminal_id(terminal_id, enc);
            enc.write_str(extension);
            enc.write_u64_be(*offset);
            enc.write_bytes(data);
            enc.write_u8(u8::from(*final_chunk));
            match sha256 {
                Some(digest) => {
                    enc.write_u8(1);
                    for byte in digest {
                        enc.write_u8(*byte);
                    }
                }
                None => enc.write_u8(0),
            }
        }
        Command::ReportAsked {
            terminal_id,
            id,
            question,
            suggestions,
            elapsed_seconds,
        } => {
            enc.write_u8(COMMAND_TAG_REPORT_ASKED);
            encode_terminal_id(terminal_id, enc);
            encode_asked_fields(id, question, suggestions, *elapsed_seconds, enc);
        }
        Command::ReportAgentState { terminal_id, state } => {
            enc.write_u8(COMMAND_TAG_REPORT_AGENT_STATE);
            encode_terminal_id(terminal_id, enc);
            enc.write_u8(state.to_u8());
        }
    }
}

fn encode_input_event(event: &InputEvent, enc: &mut Encoder<'_>) {
    match event {
        InputEvent::Key(event) => {
            enc.write_u8(INPUT_EVENT_TAG_KEY);
            encode_key_event(event, enc);
        }
        InputEvent::Mouse(event) => {
            enc.write_u8(INPUT_EVENT_TAG_MOUSE);
            encode_mouse_event(event, enc);
        }
        InputEvent::Focus(event) => {
            enc.write_u8(INPUT_EVENT_TAG_FOCUS);
            enc.write_u8(encode_focus_event(*event));
        }
        InputEvent::Paste(event) => {
            enc.write_u8(INPUT_EVENT_TAG_PASTE);
            encode_paste_event(event, enc);
        }
    }
}

fn decode_input_event(dec: &mut Decoder<'_>) -> Result<InputEvent, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        INPUT_EVENT_TAG_KEY => Ok(InputEvent::Key(decode_key_event(dec)?)),
        INPUT_EVENT_TAG_MOUSE => Ok(InputEvent::Mouse(decode_mouse_event(dec)?)),
        INPUT_EVENT_TAG_FOCUS => Ok(InputEvent::Focus(decode_focus_event(dec.read_u8()?)?)),
        INPUT_EVENT_TAG_PASTE => Ok(InputEvent::Paste(decode_paste_event(dec)?)),
        other => Err(DecodeError::UnknownEnumValue {
            field: "InputEvent",
            value: u32::from(other),
        }),
    }
}

pub(in crate::wire) fn decode_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let command_body_len = dec.remaining_in_body();
    let tag = dec.read_u8()?;
    // Dispatch is split by the SPEC §5.1 command families rather than one flat
    // table: each family owns a disjoint set of tags and returns `None` for a
    // tag it does not claim, so the first family that recognises `tag` is the
    // only one that reads from `dec`.
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
    Err(DecodeError::UnknownEnumValue {
        field: "Command",
        value: u32::from(tag),
    })
}

/// Decode the per-Terminal subscription and single-Terminal destroy verbs
/// (SPEC §5.1): `ATTACH_TERMINAL`, `DETACH_TERMINAL`, `KILL_TERMINAL`.
///
/// Returns `Ok(None)` — without reading from `dec` — when `tag` belongs to
/// another family.
fn decode_terminal_subscription_command(
    tag: u8,
    dec: &mut Decoder<'_>,
) -> Result<Option<Command>, DecodeError> {
    let command = match tag {
        COMMAND_TAG_ATTACH_TERMINAL => Command::AttachTerminal {
            terminal_id: decode_terminal_id(dec)?,
        },
        COMMAND_TAG_DETACH_TERMINAL => Command::DetachTerminal {
            terminal_id: decode_terminal_id(dec)?,
        },
        COMMAND_TAG_KILL_TERMINAL => Command::KillTerminal {
            terminal_id: decode_terminal_id(dec)?,
        },
        _ => return Ok(None),
    };
    Ok(Some(command))
}

/// Decode the live agent affordances (SPEC §6): `GET_SCREEN`, `ROUTE_INPUT`,
/// `APPLY_INPUT`, `GET_TERMINAL_STATE`, `SUBSCRIBE_TERMINAL_EVENTS`, and
/// `PUT_FILE`.
///
/// `command_body_len` is the Command body length measured *before* the tag
/// byte was read — `APPLY_INPUT` bounds itself on it. Returns `Ok(None)` —
/// without reading from `dec` — when `tag` belongs to another family.
fn decode_live_affordance_command(
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
        COMMAND_TAG_SUBSCRIBE_TERMINAL_EVENTS => decode_subscribe_terminal_events_command(dec)?,
        COMMAND_TAG_PUT_FILE => decode_put_file_command(dec)?,
        _ => return Ok(None),
    };
    Ok(Some(command))
}

/// Decode the supervisory verbs of ADR-0033 ("take the wheel + kill"):
/// `ACQUIRE_INPUT`, `RELEASE_INPUT`, `SIGNAL_TERMINAL`.
///
/// Returns `Ok(None)` — without reading from `dec` — when `tag` belongs to
/// another family.
fn decode_supervisory_command(
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

/// Decode the agent-evidence reports: `REPORT_ASKED` (ADR-0036) and
/// `REPORT_AGENT_STATE`.
///
/// Returns `Ok(None)` — without reading from `dec` — when `tag` belongs to
/// another family.
fn decode_agent_report_command(
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

/// Decode the commands whose subject is the session or the server rather than
/// one Terminal: `GET_STATE`, `KILL_TERMINALS` (§5.2), `DETACH_CLIENTS`,
/// `UPGRADE`, `SHUTDOWN`, `GET_PERF`.
///
/// Returns `Ok(None)` — without reading from `dec` — when `tag` belongs to
/// another family.
fn decode_session_command(tag: u8, dec: &mut Decoder<'_>) -> Result<Option<Command>, DecodeError> {
    let command = match tag {
        COMMAND_TAG_GET_STATE => Command::GetState {
            scope: decode_state_scope(dec)?,
        },
        COMMAND_TAG_KILL_TERMINALS => decode_kill_terminals_command(dec)?,
        COMMAND_TAG_DETACH_CLIENTS => decode_detach_clients_command(dec)?,
        COMMAND_TAG_UPGRADE => Command::Upgrade,
        COMMAND_TAG_SHUTDOWN => Command::Shutdown,
        COMMAND_TAG_GET_PERF => Command::GetPerf {
            reset: dec.read_u8()? != 0,
        },
        _ => return Ok(None),
    };
    Ok(Some(command))
}

fn decode_get_screen_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let request_scrollback = decode_optional_u32(dec)?;
    // `cells` is a trailing additive bool (`phux-8yl`): a
    // pre-`phux-8yl` body ends after `request_scrollback`, so an
    // absent byte (cursor already at the frame-body end) means
    // `false`. A present byte is read as a bool (non-zero is
    // `true`). `at_body_end` (not `remaining().is_empty()`) keeps a
    // following frame's bytes from being misread as `cells`.
    let cells = if dec.at_body_end() {
        false
    } else {
        dec.read_u8()? != 0
    };
    Ok(Command::GetScreen {
        terminal_id,
        request_scrollback,
        cells,
    })
}

/// Decode `APPLY_INPUT`, bounded twice: once on the whole Command body
/// (`command_body_len`, measured before the tag byte was read) and once on the
/// declared event count.
fn decode_apply_input_command(
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

fn decode_kill_terminals_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let count = dec.read_u16_be()? as usize;
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        ids.push(decode_terminal_id(dec)?);
    }
    Ok(Command::KillTerminals { ids })
}

fn decode_detach_clients_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let session = if dec.read_u8()? != 0 {
        Some(dec.read_str()?.to_owned())
    } else {
        None
    };
    Ok(Command::DetachClients { session })
}

fn decode_get_terminal_state_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let include_scrollback = dec.read_u8()? != 0;
    let max_scrollback_lines = dec.read_u16_be()?;
    Ok(Command::GetTerminalState {
        terminal_id,
        include_scrollback,
        max_scrollback_lines,
    })
}

fn decode_subscribe_terminal_events_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let count = dec.read_u16_be()? as usize;
    let mut event_types = Vec::with_capacity(count);
    for _ in 0..count {
        if let Some(et) = TerminalEventType::from_u8(dec.read_u8()?) {
            event_types.push(et);
        }
    }
    Ok(Command::SubscribeTerminalEvents {
        terminal_id,
        event_types,
    })
}

fn decode_acquire_input_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
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

fn decode_signal_terminal_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let signal = TerminalSignal::from_u8(dec.read_u8()?).ok_or(DecodeError::UnknownEnumValue {
        field: "TerminalSignal",
        value: 0,
    })?;
    Ok(Command::SignalTerminal {
        terminal_id,
        signal,
    })
}

fn decode_report_agent_state_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let terminal_id = decode_terminal_id(dec)?;
    let value = dec.read_u8()?;
    let state =
        ReportedAgentState::from_u8(value).ok_or_else(|| DecodeError::UnknownEnumValue {
            field: "ReportedAgentState",
            value: u32::from(value),
        })?;
    Ok(Command::ReportAgentState { terminal_id, state })
}

fn decode_put_file_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
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

fn decode_file_upload_id(dec: &mut Decoder<'_>) -> Result<FileUploadId, DecodeError> {
    let mut id_bytes = [0; 16];
    for byte in &mut id_bytes {
        *byte = dec.read_u8()?;
    }
    FileUploadId::new(id_bytes).ok_or(DecodeError::InvalidFileUploadId)
}

fn decode_put_file_extension(dec: &mut Decoder<'_>) -> Result<String, DecodeError> {
    let extension = dec.read_str()?;
    if extension.len() > 16 {
        return Err(DecodeError::FileUploadLimitExceeded);
    }
    Ok(extension.to_owned())
}

/// Read the `PUT_FILE` chunk payload, rejecting a chunk that exceeds the
/// per-chunk cap or whose `offset + len` would carry the upload past the
/// whole-file cap.
fn decode_put_file_chunk(dec: &mut Decoder<'_>, offset: u64) -> Result<Vec<u8>, DecodeError> {
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

fn decode_optional_sha256(dec: &mut Decoder<'_>) -> Result<Option<[u8; 32]>, DecodeError> {
    if dec.read_u8()? == 0 {
        return Ok(None);
    }
    let mut digest = [0; 32];
    for byte in &mut digest {
        *byte = dec.read_u8()?;
    }
    Ok(Some(digest))
}

fn decode_report_asked_command(dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
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

// -----------------------------------------------------------------------------
// `Option<ClientId>` codec — used by `AgentEvent::TerminalControl` (ADR-0033)
// for `input_holder` and `actor`. Tag convention matches every other `Option`
// on the wire (`0 = None`, `1 = Some`); the body is the inner `u32` via the
// shared `ClientId` codec ([`encode_client_id`] / [`decode_client_id`]).
// -----------------------------------------------------------------------------

fn encode_optional_client_id(value: Option<ClientId>, enc: &mut Encoder<'_>) {
    match value {
        None => enc.write_u8(0),
        Some(id) => {
            enc.write_u8(1);
            encode_client_id(id, enc);
        }
    }
}

fn decode_optional_client_id(dec: &mut Decoder<'_>) -> Result<Option<ClientId>, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        0 => Ok(None),
        1 => Ok(Some(decode_client_id(dec)?)),
        other => Err(DecodeError::UnknownEnumValue {
            field: "Option<ClientId> tag",
            value: u32::from(other),
        }),
    }
}

fn encode_state_scope(scope: &StateScope, enc: &mut Encoder<'_>) {
    match scope {
        StateScope::Server => enc.write_u8(STATE_SCOPE_TAG_SERVER),
    }
}

fn decode_state_scope(dec: &mut Decoder<'_>) -> Result<StateScope, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        STATE_SCOPE_TAG_SERVER => Ok(StateScope::Server),
        other => Err(DecodeError::UnknownEnumValue {
            field: "StateScope",
            value: u32::from(other),
        }),
    }
}

pub(in crate::wire) fn encode_command_result(result: &CommandResult, enc: &mut Encoder<'_>) {
    match result {
        CommandResult::Ok => enc.write_u8(COMMAND_RESULT_TAG_OK),
        CommandResult::OkWith(value) => {
            enc.write_u8(COMMAND_RESULT_TAG_OK_WITH);
            encode_command_value(value, enc);
        }
        CommandResult::Error { code, message } => {
            enc.write_u8(COMMAND_RESULT_TAG_ERROR);
            enc.write_u16_be(code.as_wire());
            enc.write_str(message);
        }
    }
}

pub(in crate::wire) fn decode_command_result(
    dec: &mut Decoder<'_>,
) -> Result<CommandResult, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        COMMAND_RESULT_TAG_OK => Ok(CommandResult::Ok),
        COMMAND_RESULT_TAG_OK_WITH => Ok(CommandResult::OkWith(decode_command_value(dec)?)),
        COMMAND_RESULT_TAG_ERROR => {
            let code_raw = dec.read_u16_be()?;
            let code =
                ErrorCode::from_wire(code_raw).ok_or_else(|| DecodeError::UnknownEnumValue {
                    field: "ErrorCode",
                    value: u32::from(code_raw),
                })?;
            let message = dec.read_str()?.to_owned();
            Ok(CommandResult::Error { code, message })
        }
        other => Err(DecodeError::UnknownEnumValue {
            field: "CommandResult",
            value: u32::from(other),
        }),
    }
}

fn encode_command_value(value: &CommandValue, enc: &mut Encoder<'_>) {
    match value {
        CommandValue::TerminalId(id) => {
            enc.write_u8(COMMAND_VALUE_TAG_TERMINAL_ID);
            encode_terminal_id(id, enc);
        }
        CommandValue::GroupId(id) => {
            enc.write_u8(COMMAND_VALUE_TAG_GROUP_ID);
            enc.write_u32_be(id.get());
        }
        CommandValue::State(snapshot) => {
            enc.write_u8(COMMAND_VALUE_TAG_STATE);
            encode_session_snapshot(snapshot, enc);
        }
        CommandValue::Json(s) => {
            enc.write_u8(COMMAND_VALUE_TAG_JSON);
            enc.write_str(s);
        }
        CommandValue::Bytes(b) => {
            enc.write_u8(COMMAND_VALUE_TAG_BYTES);
            enc.write_bytes(b);
        }
        CommandValue::FileUpload(ack) => {
            enc.write_u8(COMMAND_VALUE_TAG_FILE_UPLOAD);
            enc.write_u64_be(ack.next_offset);
            match &ack.path {
                Some(path) => {
                    enc.write_u8(1);
                    enc.write_str(path);
                }
                None => enc.write_u8(0),
            }
        }
    }
}

fn decode_command_value(dec: &mut Decoder<'_>) -> Result<CommandValue, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        COMMAND_VALUE_TAG_TERMINAL_ID => Ok(CommandValue::TerminalId(decode_terminal_id(dec)?)),
        COMMAND_VALUE_TAG_GROUP_ID => Ok(CommandValue::GroupId(GroupId::new(dec.read_u32_be()?))),
        COMMAND_VALUE_TAG_STATE => Ok(CommandValue::State(decode_session_snapshot(dec)?)),
        COMMAND_VALUE_TAG_JSON => Ok(CommandValue::Json(dec.read_str()?.to_owned())),
        COMMAND_VALUE_TAG_BYTES => Ok(CommandValue::Bytes(dec.read_bytes()?.to_vec())),
        COMMAND_VALUE_TAG_FILE_UPLOAD => {
            let next_offset = dec.read_u64_be()?;
            let path = if dec.read_u8()? == 0 {
                None
            } else {
                Some(dec.read_str()?.to_owned())
            };
            Ok(CommandValue::FileUpload(FileUploadAck {
                next_offset,
                path,
            }))
        }
        other => Err(DecodeError::UnknownEnumValue {
            field: "CommandValue",
            value: u32::from(other),
        }),
    }
}

// -----------------------------------------------------------------------------
// `Option<i32>` codec — used by `TERMINAL_CLOSED.exit_status` (SPEC §10.1).
//
// Tag convention matches every other `Option` on the wire: `0 = None`,
// `1 = Some(value)`. The body is the two's-complement bit pattern
// reinterpreted as `u32` (matching how the `i64` encoder treats
// timestamps in `info.rs`).
// -----------------------------------------------------------------------------

fn encode_optional_i32(value: Option<i32>, enc: &mut Encoder<'_>) {
    match value {
        None => enc.write_u8(0),
        Some(n) => {
            enc.write_u8(1);
            // Two's-complement bit pattern reinterpreted as u32 — bit-
            // identical to the `i64` encoder treatment in `info.rs`. Using
            // `i32::to_be_bytes` avoids the sign-loss clippy lint that a
            // direct `n as u32` cast triggers (the in-memory bits are the
            // same; the lint is right that the *value* changes meaning).
            enc.write_u32_be(u32::from_be_bytes(n.to_be_bytes()));
        }
    }
}

pub(in crate::wire) fn decode_optional_i32(
    dec: &mut Decoder<'_>,
) -> Result<Option<i32>, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        0 => Ok(None),
        1 => {
            // Symmetric to the encoder: reinterpret the u32's big-endian
            // bytes as the i32 two's-complement bit pattern.
            let bits = dec.read_u32_be()?;
            Ok(Some(i32::from_be_bytes(bits.to_be_bytes())))
        }
        other => Err(DecodeError::UnknownEnumValue {
            field: "Option<i32> tag",
            value: u32::from(other),
        }),
    }
}

// `SPAWN_TERMINAL.env`'s optionality is now carried by TLV field presence (an
// absent `env` field = `None`; a present field holds a concrete, possibly
// empty, list via [`encode_env`] / [`decode_env`]). The old
// `encode_optional_env` / `decode_optional_env` presence-tag helpers were
// retired with the field-tagged migration.

// -----------------------------------------------------------------------------
// AgentEvent codec — SPEC §7.5 / §10.3 (phux-y2t).
//
// TLV layout: `tag: u8`, then a length-prefixed `body: bytes`. The
// length prefix is the forward-compat lever — a decoder that doesn't
// recognise `tag` reads the body length, captures the bytes verbatim as
// `AgentEvent::Unknown { tag, body }`, and moves on without failing the
// frame. Known bodies decode from a sub-`Decoder` over the captured body
// slice, so a body that declares more fields than this version knows is
// still bounded by its own length (trailing additive fields inside a
// known event are likewise skippable).
//
// Body shapes by tag:
//   COMMAND_STARTED  (0x00) → empty
//   COMMAND_FINISHED (0x01) → optional<i32> exit_code
//   TITLE_CHANGED    (0x02) → str title
//   BELL             (0x03) → empty
//   PANE_SPAWNED     (0x04) → empty (the id rides the EVENT envelope)
//   PANE_CLOSED      (0x05) → optional<i32> exit_status
//   DIRTY            (0x06) → empty
//   IDLE             (0x07) → empty
//   ASKED            (0x09) → field-tagged TLV: str id, str question,
//                            repeated str suggestion, optional u64 elapsed_seconds
//   CWD_CHANGED      (0x0a) → str cwd
// -----------------------------------------------------------------------------

pub(in crate::wire) fn encode_agent_event(event: &AgentEvent, enc: &mut Encoder<'_>) {
    // Encode the variant body into a scratch buffer first, then write the
    // tag + the body as a single length-prefixed block. Keeping the body
    // length-delimited is what lets an older decoder skip an unknown tag.
    let mut body = BytesMut::new();
    let tag = {
        let mut body_enc = Encoder::new(&mut body);
        match event {
            AgentEvent::CommandStarted => EVENT_TAG_COMMAND_STARTED,
            AgentEvent::CommandFinished { exit_code } => {
                encode_optional_i32(*exit_code, &mut body_enc);
                EVENT_TAG_COMMAND_FINISHED
            }
            AgentEvent::TitleChanged { title } => {
                body_enc.write_str(title);
                EVENT_TAG_TITLE_CHANGED
            }
            AgentEvent::Bell => EVENT_TAG_BELL,
            AgentEvent::PaneSpawned => EVENT_TAG_PANE_SPAWNED,
            AgentEvent::PaneClosed { exit_status } => {
                encode_optional_i32(*exit_status, &mut body_enc);
                EVENT_TAG_PANE_CLOSED
            }
            AgentEvent::Dirty => EVENT_TAG_DIRTY,
            AgentEvent::Idle => EVENT_TAG_IDLE,
            AgentEvent::TerminalControl {
                lifecycle,
                exit_status,
                input_holder,
                action,
                actor,
            } => {
                body_enc.write_u8(lifecycle.to_u8());
                encode_optional_i32(*exit_status, &mut body_enc);
                encode_optional_client_id(*input_holder, &mut body_enc);
                body_enc.write_u8(action.to_u8());
                encode_optional_client_id(*actor, &mut body_enc);
                EVENT_TAG_TERMINAL_CONTROL
            }
            AgentEvent::Asked {
                id,
                question,
                suggestions,
                elapsed_seconds,
            } => {
                encode_asked_fields(id, question, suggestions, *elapsed_seconds, &mut body_enc);
                EVENT_TAG_ASKED
            }
            AgentEvent::CwdChanged { cwd } => {
                body_enc.write_str(cwd);
                EVENT_TAG_CWD_CHANGED
            }
            // `Unknown` is decoder-only: an encoder that reaches here has
            // round-tripped an event this version did not understand.
            // Re-emit the captured body verbatim so a relay (a hub
            // forwarding a satellite's event, say) is lossless rather than
            // dropping the event or panicking. The raw bytes are appended
            // after this block to sidestep the `body`/`body_enc` borrow.
            AgentEvent::Unknown { tag, .. } => *tag,
        }
    };
    if let AgentEvent::Unknown { body: raw, .. } = event {
        body.extend_from_slice(raw);
    }
    enc.write_u8(tag);
    enc.write_bytes(&body);
}

fn encode_asked_fields(
    id: &str,
    question: &str,
    suggestions: &[String],
    elapsed_seconds: Option<u64>,
    enc: &mut Encoder<'_>,
) {
    // Field-tagged TLV body: id, question, then one repeated SUGGESTION field
    // per suggestion (in order), then an optional ELAPSED_SECONDS. An absent
    // suggestion list writes no field; an absent elapsed counter writes no
    // field. The same body shape backs both Command::ReportAsked and
    // AgentEvent::Asked so the hook and event payload cannot drift.
    enc.write_field(field::event_asked::ID, id.as_bytes());
    enc.write_field(field::event_asked::QUESTION, question.as_bytes());
    for suggestion in suggestions {
        enc.write_field(field::event_asked::SUGGESTION, suggestion.as_bytes());
    }
    if let Some(secs) = elapsed_seconds {
        enc.write_field_with(field::event_asked::ELAPSED_SECONDS, |e| {
            e.write_u64_be(secs);
        });
    }
}

pub(in crate::wire) fn decode_agent_event(
    dec: &mut Decoder<'_>,
) -> Result<AgentEvent, DecodeError> {
    let tag = dec.read_u8()?;
    let body = dec.read_bytes()?;
    // Sub-decoder over just this event's body. A known body that declares
    // fewer bytes than expected errors with `UnexpectedEof`; an unknown
    // tag is captured verbatim and skipped.
    let mut body_dec = Decoder::new(body);
    let event = match tag {
        EVENT_TAG_COMMAND_STARTED => AgentEvent::CommandStarted,
        EVENT_TAG_COMMAND_FINISHED => AgentEvent::CommandFinished {
            exit_code: decode_optional_i32(&mut body_dec)?,
        },
        EVENT_TAG_TITLE_CHANGED => AgentEvent::TitleChanged {
            title: body_dec.read_str()?.to_owned(),
        },
        EVENT_TAG_BELL => AgentEvent::Bell,
        EVENT_TAG_PANE_SPAWNED => AgentEvent::PaneSpawned,
        EVENT_TAG_PANE_CLOSED => AgentEvent::PaneClosed {
            exit_status: decode_optional_i32(&mut body_dec)?,
        },
        EVENT_TAG_DIRTY => AgentEvent::Dirty,
        EVENT_TAG_IDLE => AgentEvent::Idle,
        EVENT_TAG_TERMINAL_CONTROL => decode_terminal_control_event(&mut body_dec)?,
        EVENT_TAG_ASKED => decode_asked_event(&mut body_dec)?,
        EVENT_TAG_CWD_CHANGED => AgentEvent::CwdChanged {
            cwd: body_dec.read_str()?.to_owned(),
        },
        // Unknown event tag: preserve the body verbatim and skip. This is
        // the forward-compat path — a v0.2.x server may add event kinds an
        // older client does not know.
        other => AgentEvent::Unknown {
            tag: other,
            body: body.to_vec(),
        },
    };
    Ok(event)
}

/// Decode an [`AgentEvent::TerminalControl`] body (ADR-0033): lifecycle,
/// optional exit status, optional input-lease holder, the control action, and
/// the optional actor that caused it.
fn decode_terminal_control_event(dec: &mut Decoder<'_>) -> Result<AgentEvent, DecodeError> {
    let lifecycle =
        TerminalLifecycle::from_u8(dec.read_u8()?).ok_or(DecodeError::UnknownEnumValue {
            field: "TerminalLifecycle",
            value: 0,
        })?;
    let exit_status = decode_optional_i32(dec)?;
    let input_holder = decode_optional_client_id(dec)?;
    let action = ControlAction::from_u8(dec.read_u8()?).ok_or(DecodeError::UnknownEnumValue {
        field: "ControlAction",
        value: 0,
    })?;
    let actor = decode_optional_client_id(dec)?;
    Ok(AgentEvent::TerminalControl {
        lifecycle,
        exit_status,
        input_holder,
        action,
        actor,
    })
}

/// Decode an [`AgentEvent::Asked`] body (field-tagged TLV).
///
/// Loops over the body's TLV fields by id, accumulating suggestions as the
/// repeated `SUGGESTION` field appears. An unrecognised field id is skipped by
/// its length (forward-compat), `id` / `question` default to empty when their
/// field is absent, and `elapsed_seconds` is `None` unless its field is
/// present. The whole body is bounded by the event's outer length prefix, so a
/// trailing future field cannot bleed into the next event.
fn decode_asked_event(dec: &mut Decoder<'_>) -> Result<AgentEvent, DecodeError> {
    let mut id = String::new();
    let mut question = String::new();
    let mut suggestions = Vec::new();
    let mut elapsed_seconds = None;
    while let Some((field_id, value)) = dec.read_field()? {
        match field_id {
            field::event_asked::ID => {
                core::str::from_utf8(value)
                    .map_err(|_| DecodeError::InvalidUtf8)?
                    .clone_into(&mut id);
            }
            field::event_asked::QUESTION => {
                core::str::from_utf8(value)
                    .map_err(|_| DecodeError::InvalidUtf8)?
                    .clone_into(&mut question);
            }
            field::event_asked::SUGGESTION => {
                suggestions.push(
                    core::str::from_utf8(value)
                        .map_err(|_| DecodeError::InvalidUtf8)?
                        .to_owned(),
                );
            }
            field::event_asked::ELAPSED_SECONDS => {
                elapsed_seconds = Some(Decoder::new(value).read_u64_be()?);
            }
            // Unknown field id: skip by length (already consumed by
            // `read_field`) — the forward-compat additive-field path.
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
