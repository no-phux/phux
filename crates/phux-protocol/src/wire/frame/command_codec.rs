//! Control-plane command, command-result, and agent-event codecs (SPEC §5,
//! ADR-0021; SPEC §7.5 / §10.3).

use bytes::BytesMut;

use crate::ids::{ClientId, FileUploadId, GroupId, IdempotencyKey, InputOperationId, ResourceKind};
use crate::input::InputEvent;
use crate::wire::decode::{Decoder, utf8_value};
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::field;
use crate::wire::info::{
    decode_client_id, decode_session_snapshot, encode_client_id, encode_session_snapshot,
};

use super::codec::encode_optional_u32;
use super::{
    AgentEvent, ApprovalOutcome, COMMAND_RESULT_TAG_ERROR, COMMAND_RESULT_TAG_OK,
    COMMAND_RESULT_TAG_OK_WITH, COMMAND_TAG_ACQUIRE_INPUT, COMMAND_TAG_APPEND_RESOURCE_OUTPUT,
    COMMAND_TAG_APPLY_INPUT, COMMAND_TAG_ATTACH_RESOURCE, COMMAND_TAG_CLOSE_TAB_RESOURCES,
    COMMAND_TAG_DETACH_CLIENTS, COMMAND_TAG_DETACH_RESOURCE, COMMAND_TAG_GET_FILE,
    COMMAND_TAG_GET_PERF, COMMAND_TAG_GET_SCREEN, COMMAND_TAG_GET_STATE,
    COMMAND_TAG_GET_TERMINAL_STATE, COMMAND_TAG_KILL_RESOURCE, COMMAND_TAG_KILL_RESOURCE_IF,
    COMMAND_TAG_KILL_RESOURCES, COMMAND_TAG_OPEN_LISTENER, COMMAND_TAG_PERF_CONTROL,
    COMMAND_TAG_PUT_FILE, COMMAND_TAG_RELEASE_INPUT, COMMAND_TAG_REPORT_AGENT_STATE,
    COMMAND_TAG_REPORT_ASKED, COMMAND_TAG_ROUTE_INPUT, COMMAND_TAG_SHUTDOWN,
    COMMAND_TAG_SIGNAL_TERMINAL, COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS, COMMAND_TAG_TRANSCRIBE,
    COMMAND_TAG_UPGRADE, COMMAND_VALUE_TAG_BYTES, COMMAND_VALUE_TAG_FILE_CHUNK,
    COMMAND_VALUE_TAG_FILE_UPLOAD, COMMAND_VALUE_TAG_GROUP_ID, COMMAND_VALUE_TAG_JSON,
    COMMAND_VALUE_TAG_RESOURCE_ID, COMMAND_VALUE_TAG_STATE, Command, CommandResult, CommandValue,
    ControlAction, EVENT_TAG_APPROVAL_DECIDED, EVENT_TAG_APPROVAL_REQUESTED, EVENT_TAG_ASKED,
    EVENT_TAG_BELL, EVENT_TAG_COMMAND_FINISHED, EVENT_TAG_COMMAND_STARTED, EVENT_TAG_CWD_CHANGED,
    EVENT_TAG_DIRTY, EVENT_TAG_IDLE, EVENT_TAG_INPUT_ACTIVITY, EVENT_TAG_JOURNAL_GAP,
    EVENT_TAG_RESOURCE_CLOSED, EVENT_TAG_RESOURCE_SPAWNED, EVENT_TAG_SOURCE_GAP,
    EVENT_TAG_TERMINAL_CONTROL, EVENT_TAG_TITLE_CHANGED, ErrorCode, FileChunk, FileUploadAck,
    INPUT_EVENT_TAG_FOCUS, INPUT_EVENT_TAG_KEY, INPUT_EVENT_TAG_MOUSE, INPUT_EVENT_TAG_PASTE,
    InputMode, KillConditions, KillPrecondition, ListenerTransport, MAX_APPEND_BYTES,
    MAX_APPLY_INPUT_COMMAND_BODY, MAX_APPLY_INPUT_EVENTS, MAX_FILE_UPLOAD_CHUNK,
    MAX_FILE_UPLOAD_SIZE, PerfOp, ReportedAgentState, ResourceEventType, ResourceLifecycle,
    STATE_SCOPE_TAG_SERVER, StateScope, TerminalSignal, decode_focus_event, decode_key_event,
    decode_mouse_event, decode_optional_u32, decode_paste_event, decode_terminal_id,
    encode_focus_event, encode_key_event, encode_mouse_event, encode_paste_event,
    encode_terminal_id,
};

mod command_bodies;
mod command_decode;
mod command_encode;
mod event_decode;
mod event_encode;
mod result;
mod service_http;

use command_bodies::{
    checkout_decode_error, decode_acquire_input_command, decode_checkout_command,
    decode_detach_clients_command, decode_file_upload_id, decode_get_file_command,
    decode_get_terminal_state_command, decode_optional_client_id, decode_optional_sha256,
    decode_put_file_command, decode_report_agent_state_command, decode_report_asked_command,
    decode_resource_ids, decode_signal_terminal_command, decode_state_scope,
    decode_subscribe_terminal_events_command,
};
pub(in crate::wire) use command_decode::decode_command;
use command_decode::decode_optional_key;
pub(in crate::wire) use command_encode::encode_command;
use command_encode::encode_optional_client_id;
pub(in crate::wire) use event_decode::decode_agent_event;
use event_decode::decode_asked_event;
pub(in crate::wire) use event_encode::encode_agent_event;
use event_encode::{decode_approval_event, encode_asked_fields};
pub(in crate::wire) use result::{
    decode_command_result, decode_optional_i32, encode_command_result, encode_optional_i32,
};

// `Command` and `CommandResult`: a tag, then a positional body.

// `AgentEvent` (SPEC §7.5 / §10.3): `tag: u8` + a length-prefixed body, so an
// unknown tag is captured verbatim as `Unknown` and a known body is bounded
// by its own length.
