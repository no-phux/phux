//! Frame header and `FrameKind` enum.
//!
//! See `docs/protocol.md#framing`; every frame is listed in `docs/spec/wire-schema.json`.
//!
//! Wire layout (per `docs/protocol.md#framing`):
//!
//! ```text
//! +-------------------------+
//! | length: u32 big-endian  |   number of bytes that follow
//! +-------------------------+
//! | type:   u8              |   message discriminant from §7
//! +-------------------------+
//! | payload: length-1 bytes |
//! +-------------------------+
//! ```
//!
//! `length` is at least `1` (the type byte) and at most `MAX_FRAME_LEN`.
//! Terminal content is raw VT bytes in `RESOURCE_OUTPUT` (ADR-0013); the
//! retired `PANE_DIFF` slot `0x40` stays unassigned.

/// Maximum permitted value of the wire-frame `length` field, per `docs/protocol.md#framing`
/// ("at most `16_777_216` (16 MiB)").
pub const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;
/// Maximum bytes in one opaque engine-owned history cursor.
pub const MAX_HISTORY_CURSOR_BYTES: usize = 4 * 1024;
/// Hard upper bound for rows requested or reported in one native history page.
pub const MAX_HISTORY_PAGE_ROWS: u32 = 4 * 1024;
/// Maximum bytes in one opaque client terminal-emulator PTY reply; matches
/// the 64 KiB input-command bound.
pub const MAX_INPUT_TERMINAL_REPLY_BYTES: usize = 64 * 1024;
/// Maximum payload bytes in one [`Command::PutFile`] chunk, well under the
/// 16 MiB frame cap.
pub const MAX_FILE_UPLOAD_CHUNK: usize = 8 * 1024 * 1024;
/// Maximum completed file size accepted by [`Command::PutFile`].
pub const MAX_FILE_UPLOAD_SIZE: u64 = 64 * 1024 * 1024;
/// Maximum payload bytes in one [`Command::AppendResourceOutput`] call: one
/// or more complete records, never a bulk upload.
pub const MAX_APPEND_BYTES: usize = 64 * 1024;
/// Maximum payload bytes in one [`Command::PublishResourceState`] call: one
/// whole state document, such as an application's accessibility tree.
pub const MAX_PUBLISH_BYTES: usize = 4 * 1024 * 1024;
/// Maximum bytes in a `SPAWN_RESOURCE.provider` string (field 13): the
/// `integration_id` bound of the `phux.agent-session/v1` record
/// (`docs/program-sequences.md#the-agent-record`).
pub const MAX_RESOURCE_PROVIDER_BYTES: usize = 120;
/// Maximum bytes in a `SPAWN_RESOURCE.native_id` string (field 14): the
/// `native_id` bound of the `phux.agent-session/v1` record
/// (`docs/program-sequences.md#the-agent-record`).
pub const MAX_RESOURCE_NATIVE_ID_BYTES: usize = 1024;

// Message discriminants (SPEC §7). Each `wire_tags!` declaration reserves its
// byte through a trait impl, so reusing a byte in one namespace is a
// conflicting impl (E0119); `tests/wire_tag_registry.rs` proves it.
#[allow(dead_code)]
mod tag_registry {
    pub(super) trait Allocated<const TAG: u8> {}
    pub(super) struct Message;
    pub(super) struct SpawnResult;
    pub(super) struct SpawnError;
    pub(super) struct MoveResult;
    pub(super) struct MoveError;
    pub(super) struct Scope;
    pub(super) struct Event;
    pub(super) struct Command;
    pub(super) struct InputEvent;
    pub(super) struct StateScope;
    pub(super) struct CommandResult;
    pub(super) struct CommandValue;
    pub(super) struct AttachTarget;
}

macro_rules! wire_tags {
    ($namespace:ident; $( $(#[$attr:meta])* $vis:vis const $name:ident: u8 = $tag:expr; )*) => {
        $(
            $(#[$attr])*
            $vis const $name: u8 = $tag;
            impl tag_registry::Allocated<{ $tag }> for tag_registry::$namespace {}
        )*
    };
}

wire_tags! { Message;
/// Discriminant for `HELLO` (client to server, `docs/protocol.md#handshake`).
pub const TYPE_HELLO: u8 = 0x01;
/// Discriminant for `ATTACH` (client to server, `docs/protocol.md`).
pub const TYPE_ATTACH: u8 = 0x02;
/// Discriminant for `DETACH` (client to server, `docs/protocol.md`).
pub const TYPE_DETACH: u8 = 0x03;
/// Discriminant for `INPUT_KEY` (client to server, `docs/protocol.md#input`).
pub const TYPE_INPUT_KEY: u8 = 0x10;
/// Discriminant for `INPUT_PASTE` (client to server, `docs/protocol.md#input`).
pub const TYPE_INPUT_PASTE: u8 = 0x11;
/// Discriminant for `INPUT_MOUSE` (client to server, `docs/protocol.md#input`).
pub const TYPE_INPUT_MOUSE: u8 = 0x12;
/// Discriminant for `INPUT_FOCUS` (client to server, `docs/protocol.md#input`).
pub const TYPE_INPUT_FOCUS: u8 = 0x14;
// 0x15 (`INPUT_SELECTION`) was removed by ADR-0030 and stays unassigned.
/// Discriminant for `HISTORY_REQUEST` (client to server, `docs/protocol.md`).
pub const TYPE_HISTORY_REQUEST: u8 = 0x16;
/// Discriminant for `INPUT_TERMINAL_REPLY` (client to server,
/// `docs/protocol.md#terminal-queries`).
pub const TYPE_INPUT_TERMINAL_REPLY: u8 = 0x17;
/// Discriminant for `CLIPBOARD_REPLY` (client to server,
/// `docs/program-sequences.md#clipboard-osc-52`).
pub const TYPE_CLIPBOARD_REPLY: u8 = 0x18;
/// Discriminant for StateSync-only `FRAME_ACK` (client to server,
/// `docs/protocol.md#generations-publication-and-history`).
///
/// Cumulative within one `(ResourceId, StreamId, BootstrapId)` after the
/// client applies the acknowledged transition. Raw profiles never send it.
pub const TYPE_FRAME_ACK: u8 = 0x21;
/// Discriminant for `VIEWPORT_RESIZE` (client to server, `docs/protocol.md`):
/// the outer terminal changed size; payload is the
/// [`ViewportInfo`] shape `ATTACH` carries.
pub const TYPE_VIEWPORT_RESIZE: u8 = 0x20;
/// Discriminant for `PING` (client to server, `docs/protocol.md#handshake`).
pub const TYPE_PING: u8 = 0x7F;
/// Discriminant for `HELLO_OK` (server to client, `docs/protocol.md#handshake`).
pub const TYPE_HELLO_OK: u8 = 0x80;
/// Discriminant for `ATTACHED` (server to client, `docs/protocol.md#attach-and-output`).
pub const TYPE_ATTACHED: u8 = 0x81;
/// Discriminant for `DETACHED` (server to client, `docs/protocol.md#commands-events-resources`).
pub const TYPE_DETACHED: u8 = 0x82;
/// Discriminant for `ATTACH_READY` (server to client, `docs/protocol.md#attach-and-output`).
pub const TYPE_ATTACH_READY: u8 = 0x83;
/// Discriminant for `BELL` (server to client, `docs/protocol.md#commands-events-resources`).
pub const TYPE_BELL: u8 = 0xB0;
/// Discriminant for `ERROR` (server to client, `docs/protocol.md`). Fatal
/// errors MUST be followed by `DETACHED { PROTOCOL_ERROR }` and transport close.
pub const TYPE_ERROR: u8 = 0xC1;
/// Discriminant for `PONG` (server to client, `docs/protocol.md#handshake`).
pub const TYPE_PONG: u8 = 0xFF;
/// Discriminant for generation-bound `RESOURCE_OUTPUT` (server to client,
/// `docs/protocol.md#attach-and-output`). Native-profile payloads are byte-identical raw PTY
/// bytes and are never capability-rewritten.
pub const TYPE_RESOURCE_OUTPUT: u8 = 0x90;
// 0x91 (`TERMINAL_SNAPSHOT`) is retired by ADR-0070; never decode or reassign it.
/// Discriminant for `BOOTSTRAP_BEGIN` (server to client, `docs/protocol.md#attach-and-output`).
pub const TYPE_BOOTSTRAP_BEGIN: u8 = 0x93;
/// Discriminant for `BOOTSTRAP_CHUNK` (server to client, `docs/protocol.md#attach-and-output`).
pub const TYPE_BOOTSTRAP_CHUNK: u8 = 0x94;
/// Discriminant for `BOOTSTRAP_READY` (server to client, `docs/protocol.md#attach-and-output`).
pub const TYPE_BOOTSTRAP_READY: u8 = 0x95;
/// Discriminant for `HISTORY_PAGE` (server to client, `docs/protocol.md`).
pub const TYPE_HISTORY_PAGE: u8 = 0x96;
/// Discriminant for `BOOTSTRAP_TOMBSTONE` (server to client, `docs/protocol.md`).
pub const TYPE_BOOTSTRAP_TOMBSTONE: u8 = 0x97;
/// Discriminant for cursor-scoped `HISTORY_TOMBSTONE` (server to client,
/// `docs/protocol.md#generations-publication-and-history`).
pub const TYPE_HISTORY_TOMBSTONE: u8 = 0x98;
/// Discriminant for retryable cursor-scoped `HISTORY_REJECTED` (server to
/// client, `docs/protocol.md#generations-publication-and-history`).
pub const TYPE_HISTORY_REJECTED: u8 = 0x99;
/// Discriminant for `FRAME_COMPRESSED` (server to client): a negotiated
/// envelope carrying one deflated inner frame (`docs/protocol.md#framing`).
pub const TYPE_FRAME_COMPRESSED: u8 = 0x9A;
}

// L3 metadata discriminants (SPEC §7.4): C→S `0x50..=0x5F`, S→C `0xD0..=0xDF`.

wire_tags! { Message;
/// Discriminant for `GET_METADATA` (client to server, `docs/protocol.md`).
pub const TYPE_GET_METADATA: u8 = 0x50;
/// Discriminant for `SET_METADATA` (client to server, `docs/protocol.md`).
pub const TYPE_SET_METADATA: u8 = 0x51;
/// Discriminant for `DELETE_METADATA` (client to server, `docs/protocol.md`).
pub const TYPE_DELETE_METADATA: u8 = 0x52;
/// Discriminant for `LIST_METADATA` (client to server, `docs/protocol.md`).
pub const TYPE_LIST_METADATA: u8 = 0x53;
/// Discriminant for `SUBSCRIBE_METADATA` (client to server, `docs/protocol.md`).
pub const TYPE_SUBSCRIBE_METADATA: u8 = 0x54;

/// Discriminant for `METADATA_CHANGED` (server to client, `docs/protocol.md`).
pub const TYPE_METADATA_CHANGED: u8 = 0xD0;
}

mod command_tags;
mod tags;
pub use command_tags::*;
pub use tags::*;

mod codec;
mod command;
mod command_codec;
mod directory;
mod kind;
mod metadata_keys;
mod org;
mod path;
mod payload;
mod role;
mod status;
pub mod typed_document;
mod whoami;

pub use command::{
    AgentEvent, ApprovalOutcome, CONTROL_UNRESTRICTED, Command, CommandResult, CommandValue,
    ControlAction, ControlClass, ControlClasses, DocumentInput, DocumentKey, FileChunk,
    FileUploadAck, GET_SCREEN_FORMAT_SELECTOR_MASK, GET_SCREEN_FORMAT_UNWRAP, InputMode,
    KillConditions, KillPrecondition, ListenerTransport, MAX_CLIPBOARD_TEXT,
    MAX_DOCUMENT_INPUT_TEXT, MAX_MIRROR_TARGET, MAX_MIRROR_TARGETS, MAX_ON_BEHALF_OF, MirrorOp,
    OnBehalfOf, PerfOp, ReportedAgentState, ResourceEventType, ResourceLifecycle, StateScope,
    TerminalSignal,
};
pub use directory::{
    DirectoryEntry, DirectoryErrorCode, DirectoryListing, DirectoryListingError,
    DirectoryListingResult, MAX_DIRECTORY_ENTRIES,
};
pub use kind::FrameKind;
pub use metadata_keys::*;
pub use org::{
    ActivationInfo, ArrangementDoc, ArrangementGroup, ArrangementNode, ArrangementOwner,
    ArrangementValue, Committed, CreatePlacement, Delegation, EntryRef, EntryTarget, ExecState,
    ExitPolicy, LaunchMode, LaunchSpec, LiveTarget, MAX_ARRANGEMENT_BYTES, MAX_ARRANGEMENT_DEPTH,
    MAX_CREDENTIAL_ID_BYTES, MAX_LAUNCH_LIST, MAX_LAUNCH_RESTORE_BYTES, MAX_ORG_ATTRS,
    MAX_ORG_BATCH, MAX_ORG_CURSOR_BYTES, MAX_ORG_NAME_BYTES, MAX_SOCKET_PATH_BYTES, MoveItem,
    OrgFilter, OrganizationPage, Outcome, RecordRevision, Refusal, ResourceRecord, SessionLifetime,
    SessionRecord, SplitDirection, TerminateMode,
};
pub use path::{
    MAX_PATH_RESULTS, PathErrorCode, PathKind, PathQueryError, PathQueryResult, PathResults,
    PathRow, PathStatus,
};
pub(in crate::wire) use path::{decode_query, decode_results};
pub use payload::{
    ActorRef, AttachTarget, EventStamp, MoveError, MoveResult, Scope, SpawnError, SpawnResource,
    SpawnResult, ViewportInfo,
};
pub use role::{RolePolicy, TakeoverPolicy, TerminalRole};
pub use status::{
    CloseReason, DetachReason, ErrorCode, ErrorScope, HistoryRejectionReason,
    HistoryTombstoneReason, TombstoneReason,
};
pub use whoami::{
    AUTH_ROUTE_BEARER_QUIC, AUTH_ROUTE_BEARER_WEBTRANSPORT, AUTH_ROUTE_BEARER_WSS,
    AUTH_ROUTE_LOOPBACK_QUIC, AUTH_ROUTE_LOOPBACK_WEBTRANSPORT, AUTH_ROUTE_LOOPBACK_WS,
    AUTH_ROUTE_SSH_STDIO, AUTH_ROUTE_UDS, ServingUser, SshClient, WHOAMI_KEY,
    WHOAMI_SCHEMA_VERSION, WhoamiRecord,
};

pub(in crate::wire) use codec::{
    decode_actor_ref, decode_attach_target, decode_bootstrap_codec, decode_bootstrap_id,
    decode_bootstrap_profile, decode_bootstrap_stream_profile, decode_env, decode_focus_event,
    decode_idempotency_key, decode_key_event, decode_metadata_scope_key, decode_mouse_event,
    decode_move_result, decode_optional_u32, decode_paste_event, decode_scope,
    decode_server_instance, decode_spawn_result, decode_stream_id, decode_string_list,
    decode_terminal_id, decode_viewport_info, encode_actor_ref, encode_attach_target,
    encode_bootstrap_codec, encode_bootstrap_profile, encode_env, encode_focus_event,
    encode_key_event, encode_mouse_event, encode_move_result, encode_paste_event, encode_scope,
    encode_server_instance, encode_spawn_result, encode_string_list, encode_terminal_id,
    encode_viewport_info,
};
pub(in crate::wire) use command_codec::{
    decode_agent_event, decode_command, decode_command_result, decode_optional_i32,
    encode_agent_event, encode_command, encode_command_result, encode_optional_i32,
};
pub(in crate::wire) use directory::{decode_directory_listing, decode_list_directory};

#[cfg(test)]
mod tests;
