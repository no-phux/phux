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
//! | type:   u8              |   message discriminant
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

// Message discriminants. Each `wire_tags!` declaration reserves its
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

// Every `FrameKind` discriminant is a row of the frame catalog.
#[macro_use]
mod catalog;
frame_catalog!(frame_type_consts);

wire_tags! { Message;
/// Discriminant for `FRAME_COMPRESSED` (server to client): a negotiated
/// envelope carrying one deflated inner frame (`docs/protocol.md#framing`).
pub const TYPE_FRAME_COMPRESSED: u8 = 0x9A;
}

mod command_tags;
mod tags;
pub use command_tags::*;
pub(crate) use tags::*;

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
mod unseen_keys;
mod whoami;

pub(in crate::wire) use catalog::frame_catalog;
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
pub use unseen_keys::*;
pub use whoami::{
    AUTH_ROUTE_BEARER_QUIC, AUTH_ROUTE_BEARER_WEBTRANSPORT, AUTH_ROUTE_BEARER_WSS,
    AUTH_ROUTE_LOOPBACK_QUIC, AUTH_ROUTE_LOOPBACK_WEBTRANSPORT, AUTH_ROUTE_LOOPBACK_WS,
    AUTH_ROUTE_SSH_STDIO, AUTH_ROUTE_UDS, ServingUser, SshClient, WHOAMI_KEY,
    WHOAMI_SCHEMA_VERSION, WhoamiRecord,
};

pub(in crate::wire) use codec::{
    decode_actor_ref, decode_attach_target, decode_bootstrap_codec, decode_bootstrap_id,
    decode_bootstrap_profile, decode_bootstrap_stream_profile, decode_env, decode_focus_event,
    decode_idempotency_key, decode_key_event, decode_mouse_event, decode_move_result,
    decode_optional_u32, decode_paste_event, decode_scope, decode_server_instance,
    decode_spawn_result, decode_stream_id, decode_string_list, decode_terminal_id,
    decode_viewport_info, encode_actor_ref, encode_attach_target, encode_bootstrap_codec,
    encode_bootstrap_profile, encode_env, encode_focus_event, encode_key_event, encode_mouse_event,
    encode_move_result, encode_paste_event, encode_scope, encode_server_instance,
    encode_spawn_result, encode_string_list, encode_terminal_id, encode_viewport_info,
};
pub(in crate::wire) use command_codec::{
    decode_agent_event, decode_command, decode_command_result, decode_optional_i32,
    encode_agent_event, encode_command, encode_command_result, encode_optional_i32,
};
pub(in crate::wire) use directory::{decode_directory_listing, decode_list_directory};

#[cfg(test)]
mod tests;
