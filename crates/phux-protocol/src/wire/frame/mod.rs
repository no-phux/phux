//! Frame header and `FrameKind` enum.
//!
//! See `docs/spec/proto.md` §5 (framing) and §7 (message catalog).
//!
//! Wire layout (per `docs/spec/proto.md` §5):
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

/// Maximum permitted value of the wire-frame `length` field, per `docs/spec/proto.md` §5
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
/// Maximum bytes in a `SPAWN_RESOURCE.provider` string (field 13): the
/// `integration_id` bound of the `phux.agent-session/v1` record
/// (`docs/spec/L3.md` §3.7.1).
pub const MAX_RESOURCE_PROVIDER_BYTES: usize = 120;
/// Maximum bytes in a `SPAWN_RESOURCE.native_id` string (field 14): the
/// `native_id` bound of the `phux.agent-session/v1` record
/// (`docs/spec/L3.md` §3.7.1).
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

/// Declare a fieldless `#[repr(u8)]` wire enum with its `$to` / `$from` byte
/// conversions; `$from` is `None` for a byte this build does not define.
macro_rules! wire_enum {
    ($to:ident / $from:ident;
     $(#[$meta:meta])*
     pub enum $name:ident {
         $( $(#[$vmeta:meta])* $variant:ident = $tag:literal, )*
     }) => {
        $(#[$meta])*
        #[repr(u8)]
        pub enum $name {
            $( $(#[$vmeta])* $variant = $tag, )*
        }

        impl $name {
            /// Wire byte for this value.
            #[must_use]
            pub const fn $to(self) -> u8 {
                self as u8
            }

            /// Decode a wire byte; `None` for a value this build does not define.
            #[must_use]
            pub const fn $from(value: u8) -> Option<Self> {
                match value {
                    $( $tag => Some(Self::$variant), )*
                    _ => None,
                }
            }
        }
    };
}

/// Wire type byte for one SPEC §7 message-catalog entry. As a `#[repr(u8)]`
/// enum a duplicated type byte is compile error E0081. New allocations come
/// from `docs/spec/appendix-reserved.md` §1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
enum FrameType {
    Hello = 0x01,
    Attach = 0x02,
    Detach = 0x03,
    InputKey = 0x10,
    InputPaste = 0x11,
    InputMouse = 0x12,
    InputFocus = 0x14,
    HistoryRequest = 0x16,
    InputTerminalReply = 0x17,
    FrameAck = 0x21,
    ViewportResize = 0x20,
    Ping = 0x7F,
    HelloOk = 0x80,
    Attached = 0x81,
    Detached = 0x82,
    AttachReady = 0x83,
    Bell = 0xB0,
    Error = 0xC1,
    Pong = 0xFF,
    ResourceOutput = 0x90,
    BootstrapBegin = 0x93,
    BootstrapChunk = 0x94,
    BootstrapReady = 0x95,
    HistoryPage = 0x96,
    BootstrapTombstone = 0x97,
    HistoryTombstone = 0x98,
    HistoryRejected = 0x99,
    FrameCompressed = 0x9A,
    GetMetadata = 0x50,
    SetMetadata = 0x51,
    DeleteMetadata = 0x52,
    ListMetadata = 0x53,
    SubscribeMetadata = 0x54,
    MetadataChanged = 0xD0,
    MetadataValue = 0xD1,
    MetadataKeys = 0xD2,
    ListDirectory = 0x55,
    DirectoryListing = 0xD3,
    PathQuery = 0x56,
    PathResults = 0xD4,
    SpawnResource = 0x22,
    ResizeTerminal = 0x23,
    MoveResource = 0x2A,
    ResourceMoved = 0xA8,
    ResourceClosed = 0xA1,
    ResourceSpawned = 0xA2,
    Command = 0x31,
    CommandResult = 0xC2,
    SubscribeEvents = 0x41,
    Event = 0xB3,
}

wire_tags! { Message;
/// Discriminant for `HELLO` (client to server, `docs/spec/proto.md` §6.1).
pub const TYPE_HELLO: u8 = FrameType::Hello as u8;
/// Discriminant for `ATTACH` (client to server, `docs/spec/proto.md` §7.1 / §13).
pub const TYPE_ATTACH: u8 = FrameType::Attach as u8;
/// Discriminant for `DETACH` (client to server, `docs/spec/proto.md` §7.1 / §7.3).
pub const TYPE_DETACH: u8 = FrameType::Detach as u8;
/// Discriminant for `INPUT_KEY` (client to server, `docs/spec/input.md` §2).
pub const TYPE_INPUT_KEY: u8 = FrameType::InputKey as u8;
/// Discriminant for `INPUT_PASTE` (client to server, `docs/spec/input.md` §5).
pub const TYPE_INPUT_PASTE: u8 = FrameType::InputPaste as u8;
/// Discriminant for `INPUT_MOUSE` (client to server, `docs/spec/input.md` §3).
pub const TYPE_INPUT_MOUSE: u8 = FrameType::InputMouse as u8;
/// Discriminant for `INPUT_FOCUS` (client to server, `docs/spec/input.md` §4).
pub const TYPE_INPUT_FOCUS: u8 = FrameType::InputFocus as u8;
// 0x15 (`INPUT_SELECTION`) was removed by ADR-0030 and stays unassigned.
/// Discriminant for `HISTORY_REQUEST` (client to server, `docs/spec/L1.md` §4.5).
pub const TYPE_HISTORY_REQUEST: u8 = FrameType::HistoryRequest as u8;
/// Discriminant for `INPUT_TERMINAL_REPLY` (client to server,
/// `docs/spec/input.md` §6).
pub const TYPE_INPUT_TERMINAL_REPLY: u8 = FrameType::InputTerminalReply as u8;
/// Discriminant for StateSync-only `FRAME_ACK` (client to server,
/// `docs/spec/proto.md` §8.2).
///
/// Cumulative within one `(ResourceId, StreamId, BootstrapId)` after the
/// client applies the acknowledged transition. Raw profiles never send it.
pub const TYPE_FRAME_ACK: u8 = FrameType::FrameAck as u8;
/// Discriminant for `VIEWPORT_RESIZE` (client to server, `docs/spec/proto.md`
/// §7.1 / §10.5): the outer terminal changed size; payload is the
/// [`ViewportInfo`] shape `ATTACH` carries.
pub const TYPE_VIEWPORT_RESIZE: u8 = FrameType::ViewportResize as u8;
/// Discriminant for `PING` (client to server, `docs/spec/proto.md` §7.4).
pub const TYPE_PING: u8 = FrameType::Ping as u8;
/// Discriminant for `HELLO_OK` (server to client, `docs/spec/proto.md` §6.1).
pub const TYPE_HELLO_OK: u8 = FrameType::HelloOk as u8;
/// Discriminant for `ATTACHED` (server to client, `docs/spec/L1.md` §8).
pub const TYPE_ATTACHED: u8 = FrameType::Attached as u8;
/// Discriminant for `DETACHED` (server to client, `docs/spec/L1.md` §1 / §7.3).
pub const TYPE_DETACHED: u8 = FrameType::Detached as u8;
/// Discriminant for `ATTACH_READY` (server to client, `docs/spec/L1.md` §8).
pub const TYPE_ATTACH_READY: u8 = FrameType::AttachReady as u8;
/// Discriminant for `BELL` (server to client, `docs/spec/L1.md` §1.2).
pub const TYPE_BELL: u8 = FrameType::Bell as u8;
/// Discriminant for `ERROR` (server to client, `docs/spec/proto.md` §9). Fatal
/// errors MUST be followed by `DETACHED { PROTOCOL_ERROR }` and transport close.
pub const TYPE_ERROR: u8 = FrameType::Error as u8;
/// Discriminant for `PONG` (server to client, `docs/spec/proto.md` §7.4).
pub const TYPE_PONG: u8 = FrameType::Pong as u8;
/// Discriminant for generation-bound `RESOURCE_OUTPUT` (server to client,
/// `docs/spec/L1.md` §4.1). Native-profile payloads are byte-identical raw PTY
/// bytes and are never capability-rewritten.
pub const TYPE_RESOURCE_OUTPUT: u8 = FrameType::ResourceOutput as u8;
// 0x91 (`TERMINAL_SNAPSHOT`) is retired by ADR-0070; never decode or reassign it.
/// Discriminant for `BOOTSTRAP_BEGIN` (server to client, `docs/spec/L1.md` §4.3).
pub const TYPE_BOOTSTRAP_BEGIN: u8 = FrameType::BootstrapBegin as u8;
/// Discriminant for `BOOTSTRAP_CHUNK` (server to client, `docs/spec/L1.md` §4.3).
pub const TYPE_BOOTSTRAP_CHUNK: u8 = FrameType::BootstrapChunk as u8;
/// Discriminant for `BOOTSTRAP_READY` (server to client, `docs/spec/L1.md` §4.3).
pub const TYPE_BOOTSTRAP_READY: u8 = FrameType::BootstrapReady as u8;
/// Discriminant for `HISTORY_PAGE` (server to client, `docs/spec/L1.md` §4.5).
pub const TYPE_HISTORY_PAGE: u8 = FrameType::HistoryPage as u8;
/// Discriminant for `BOOTSTRAP_TOMBSTONE` (server to client, `docs/spec/L1.md` §4.6).
pub const TYPE_BOOTSTRAP_TOMBSTONE: u8 = FrameType::BootstrapTombstone as u8;
/// Discriminant for cursor-scoped `HISTORY_TOMBSTONE` (server to client,
/// `docs/spec/L1.md` §4.5).
pub const TYPE_HISTORY_TOMBSTONE: u8 = FrameType::HistoryTombstone as u8;
/// Discriminant for retryable cursor-scoped `HISTORY_REJECTED` (server to
/// client, `docs/spec/L1.md` §4.5).
pub const TYPE_HISTORY_REJECTED: u8 = FrameType::HistoryRejected as u8;
/// Discriminant for `FRAME_COMPRESSED` (server to client): a negotiated
/// envelope carrying one deflated inner frame (`docs/spec/proto.md` §6.4).
pub const TYPE_FRAME_COMPRESSED: u8 = FrameType::FrameCompressed as u8;
}

// L3 metadata discriminants (SPEC §7.4): C→S `0x50..=0x5F`, S→C `0xD0..=0xDF`.

wire_tags! { Message;
/// Discriminant for `GET_METADATA` (client to server, `docs/spec/L3.md` §1 / §11.L3).
pub const TYPE_GET_METADATA: u8 = FrameType::GetMetadata as u8;
/// Discriminant for `SET_METADATA` (client to server, `docs/spec/L3.md` §1 / §11.L3).
pub const TYPE_SET_METADATA: u8 = FrameType::SetMetadata as u8;
/// Discriminant for `DELETE_METADATA` (client to server, `docs/spec/L3.md` §1 / §11.L3).
pub const TYPE_DELETE_METADATA: u8 = FrameType::DeleteMetadata as u8;
/// Discriminant for `LIST_METADATA` (client to server, `docs/spec/L3.md` §1 / §11.L3).
pub const TYPE_LIST_METADATA: u8 = FrameType::ListMetadata as u8;
/// Discriminant for `SUBSCRIBE_METADATA` (client to server, `docs/spec/L3.md` §1).
pub const TYPE_SUBSCRIBE_METADATA: u8 = FrameType::SubscribeMetadata as u8;

/// Discriminant for `METADATA_CHANGED` (server to client, `docs/spec/L3.md` §1).
pub const TYPE_METADATA_CHANGED: u8 = FrameType::MetadataChanged as u8;
}

/// Conventional L3 metadata key renaming a session (ADR-0019 / ADR-0027): the
/// server intercepts a `SET_METADATA` of `current\0new` and applies the
/// registry rename.
pub const SESSION_NAME_KEY: &str = "phux.session.name/v1";

/// Encode a [`SESSION_NAME_KEY`] value: `current\0new`.
#[must_use]
pub fn encode_session_rename(current: &str, new_name: &str) -> Vec<u8> {
    let mut value = current.as_bytes().to_vec();
    value.push(0);
    value.extend_from_slice(new_name.as_bytes());
    value
}

/// Decode a [`SESSION_NAME_KEY`] value into `(current, new)`.
///
/// `None` for anything other than UTF-8 `current\0new` with both sides
/// non-empty and no extra NULs.
#[must_use]
pub fn decode_session_rename(value: &[u8]) -> Option<(&str, &str)> {
    let text = std::str::from_utf8(value).ok()?;
    let (current, new_name) = text.split_once('\0')?;
    if current.is_empty() || new_name.is_empty() || new_name.contains('\0') {
        return None;
    }
    Some((current, new_name))
}

/// Conventional L3 metadata key creating a named session without attaching.
///
/// ADR-0019 / ADR-0027. Value: UTF-8 JSON `{ "name", "command"?, "cwd"? }`
/// written under `Scope::Global`; the server seeds the session and pane as
/// `ATTACH { CreateIfMissing }` would and records the result.
pub const SESSION_CREATE_KEY: &str = "phux.session.create/v1";

/// Global key where the server publishes the latest [`SESSION_CREATE_KEY`]
/// result as JSON `{ "name", "terminal_id" }`, since `SET_METADATA` has no
/// reply frame.
pub const SESSION_CREATE_RESULT_KEY: &str = "phux.session.created/v1";

/// Prefix for one-shot session-create result keys: a client that sends a
/// `request_token` reads `"{prefix}{request_token}"`, consumed on first GET.
pub const SESSION_CREATE_RESULT_KEY_PREFIX: &str = "phux.session.created/v1/";

/// Global key setting a session's keep-empty mark (ADR-0105).
///
/// Value `name\0true` or `name\0false`, applied and broadcast, not stored. Clearing
/// the mark on a windowless session removes it. Gated on
/// [`ServerFeature::KeepEmptySessions`](crate::caps::ServerFeature::KeepEmptySessions).
pub const SESSION_KEEP_EMPTY_KEY: &str = "phux.session.keep_empty/v1";

/// Encode a [`SESSION_KEEP_EMPTY_KEY`] value: `name\0true` or `name\0false`.
#[must_use]
pub fn encode_session_keep_empty(name: &str, keep: bool) -> Vec<u8> {
    format!("{name}\0{keep}").into_bytes()
}

/// Decode a [`SESSION_KEEP_EMPTY_KEY`] value into the session name and the
/// mark. `None` for anything other than UTF-8 `name\0true` or `name\0false`.
#[must_use]
pub fn decode_session_keep_empty(value: &[u8]) -> Option<(&str, bool)> {
    let (name, flag) = std::str::from_utf8(value).ok()?.split_once('\0')?;
    match flag {
        "true" => Some((name, true)),
        "false" => Some((name, false)),
        _ => None,
    }
}

/// Terminal-scoped key holding freeform tags as a JSON array of unique,
/// non-empty strings (`docs/spec/L3.md` §3.6, ADR-0027). Stored opaquely.
pub const RESOURCE_TAGS_KEY: &str = "phux.tags/v1";

/// Terminal-scoped key holding outgoing link edges as a JSON array of
/// `{ "target": u32, "kind": str }`; `kind` is an open enum (ADR-0027).
/// Stored opaquely.
pub const RESOURCE_LINK_KEY: &str = "phux.link/v1";

/// Terminal-scoped key holding the declared agent identity and lifecycle.
///
/// `docs/spec/L3.md` §3.7, ADR-0040. A consumer that finds it MUST
/// prefer it over title or screen heuristics. Stored opaquely.
pub const RESOURCE_AGENT_KEY: &str = "phux.agent/v1";

/// Server-owned projection of a pending `AgentEvent::Asked` (ADR-0035, ADR-0136).
///
/// Value is the single byte `1` while any ask source still holds, and the
/// key is absent once none does. There is no wire event for a question
/// going away, so this tombstone is how a consumer learns the clear. A hub
/// copies the key read-only onto `Satellite` scopes; clients must not set
/// or delete it.
pub const RESOURCE_ASKED_KEY: &str = "phux.agent.asked/v1";

/// Conventional Terminal-scoped provenance for provider-native session resume.
///
/// Value: bounded UTF-8 JSON `{plugin_id, integration_id, native_id}` owned by
/// ADR-0068. `SPAWN_RESOURCE.agent_session` may install these opaque bytes
/// atomically with a local spawn; ordinary L3 reads and writes use this key.
pub const RESOURCE_AGENT_SESSION_KEY: &str = "phux.agent-session/v1";

/// Conventional Terminal-scoped metadata key for the server-observed
/// foreground process and available-shell answer. Clients may read and
/// subscribe to this key but MUST NOT set or delete it.
pub const RESOURCE_PANE_OCCUPANT_KEY: &str = "phux.pane-occupant/v1";

/// Maximum encoded `phux.agent-session/v1` record accepted by server mutations.
pub const MAX_AGENT_SESSION_RECORD_BYTES: usize = 4 * 1024;

/// Global config-reload doorbell.
///
/// The value is a fresh nonce (so SET dedup
/// does not swallow it); subscribers re-read their own local config on each
/// non-tombstone change and MUST keep the previous config if the re-read
/// fails. Config never crosses the wire.
pub const CONFIG_RELOAD_KEY: &str = "phux.config.reload/v1";

/// `Global`-scope key family of the server-owned approval records (ADR-0128).
///
/// `phux.approval/v1/<id>` holds one held action's JSON description
/// (`docs/spec/L3.md` §3.10) while it awaits a decision, and is deleted when
/// the action is decided, expires, or is withdrawn. No client may set or
/// delete a key in this family.
pub const APPROVAL_KEY_PREFIX: &str = "phux.approval/v1/";

/// `Global`-scope key family a decision is written to (ADR-0128).
///
/// `SET_METADATA { Global, "phux.approval.decide/v1/<id>" }` with value
/// `approve` or `deny`. The server intercepts the write, classifies it as
/// `SIGNAL` on the held action's subject, and stores nothing.
pub const APPROVAL_DECIDE_KEY_PREFIX: &str = "phux.approval.decide/v1/";

/// A decision's value: release the held action once.
pub const APPROVAL_APPROVE: &[u8] = b"approve";

/// A decision's value: refuse the held action.
pub const APPROVAL_DENY: &[u8] = b"deny";

/// Whether `value` is a decision a decide key accepts.
#[must_use]
pub fn is_approval_decision(value: &[u8]) -> bool {
    value == APPROVAL_APPROVE || value == APPROVAL_DENY
}

wire_tags! { Message;
/// Discriminant for `METADATA_VALUE` (server to client, `docs/spec/L3.md` §1):
/// the `GET_METADATA` reply, `None` when the key is absent.
pub const TYPE_METADATA_VALUE: u8 = FrameType::MetadataValue as u8;

/// Discriminant for `METADATA_KEYS` (server to client, `docs/spec/L3.md` §1):
/// the `LIST_METADATA` reply, sorted key names only.
pub const TYPE_METADATA_KEYS: u8 = FrameType::MetadataKeys as u8;

/// Discriminant for `LIST_DIRECTORY` (client to server, `docs/spec/L3.md` §4);
/// gated on [`ServerFeature::ListDirectory`](crate::caps::ServerFeature::ListDirectory).
pub const TYPE_LIST_DIRECTORY: u8 = FrameType::ListDirectory as u8;

/// Discriminant for `DIRECTORY_LISTING` (server to client, `docs/spec/L3.md`
/// §4): the listing or a typed refusal.
pub const TYPE_DIRECTORY_LISTING: u8 = FrameType::DirectoryListing as u8;

/// `PATH_QUERY` (client to server, L3 §5).
pub const TYPE_PATH_QUERY: u8 = FrameType::PathQuery as u8;
/// `PATH_RESULTS` (server to client, L3 §5).
pub const TYPE_PATH_RESULTS: u8 = FrameType::PathResults as u8;
}

// L1 resource lifecycle discriminants (SPEC §7.2 / §10.1).

wire_tags! { Message;
/// Discriminant for `SPAWN_RESOURCE` (client to server, `docs/spec/L1.md` §1 /
/// §10.1); answered by [`TYPE_RESOURCE_SPAWNED`].
pub const TYPE_SPAWN_RESOURCE: u8 = FrameType::SpawnResource as u8;
/// Discriminant for `RESIZE_TERMINAL` (client to server, `docs/spec/L1.md` §1 /
/// §10.2): a per-Terminal PTY resize.
pub const TYPE_RESIZE_TERMINAL: u8 = FrameType::ResizeTerminal as u8;

// `0x24..=0x29` and `0xA3..=0xA7` stay unallocated: non-Terminal things are a
// `ResourceKind`, not a parallel frame family.

/// Discriminant for `MOVE_RESOURCE` (client to server, `docs/spec/L1.md` §1 /
/// §10.1, ADR-0056): re-parent a live Terminal into the window owning
/// `owner_terminal`, leaving its process and state untouched. Gated on
/// `MOVE_RESOURCE`; answered by [`TYPE_RESOURCE_MOVED`].
pub const TYPE_MOVE_RESOURCE: u8 = FrameType::MoveResource as u8;
/// Discriminant for `RESOURCE_MOVED` (server to client): the
/// `MOVE_RESOURCE` reply carrying a [`MoveResult`].
pub const TYPE_RESOURCE_MOVED: u8 = FrameType::ResourceMoved as u8;

/// Discriminant for `RESOURCE_CLOSED` (server to client, `docs/spec/L1.md` §1 /
/// §10.1): a resource ended.
pub const TYPE_RESOURCE_CLOSED: u8 = FrameType::ResourceClosed as u8;
/// Discriminant for `RESOURCE_SPAWNED` (server to client): the
/// `SPAWN_RESOURCE` reply carrying a [`SpawnResult`].
pub const TYPE_RESOURCE_SPAWNED: u8 = FrameType::ResourceSpawned as u8;
}

// `Result`-shaped unions use `Ok = 0`, `Err = 1`, mirroring `Option`'s
// `None = 0`, `Some = 1`.
wire_tags! { SpawnResult;
/// Wire tag for [`SpawnResult::Ok`].
pub(crate) const SPAWN_RESULT_OK: u8 = 0;
/// Wire tag for [`SpawnResult::Err`].
pub(crate) const SPAWN_RESULT_ERR: u8 = 1;
}

// Wire tags for the `SpawnError` tagged union (SPEC §7.2 / §10.1).
wire_tags! { SpawnError;
/// Wire tag for [`SpawnError::GroupNotFound`].
pub(crate) const SPAWN_ERROR_TAG_GROUP_NOT_FOUND: u8 = 0;
/// Wire tag for [`SpawnError::SpawnFailed`].
pub(crate) const SPAWN_ERROR_TAG_SPAWN_FAILED: u8 = 1;
/// Wire tag for [`SpawnError::UnsupportedSatelliteRoute`].
pub(crate) const SPAWN_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE: u8 = 2;
/// Wire tag for [`SpawnError::SatelliteUnreachable`].
pub(crate) const SPAWN_ERROR_TAG_SATELLITE_UNREACHABLE: u8 = 3;
/// Wire tag for [`SpawnError::UnsupportedKind`].
pub(crate) const SPAWN_ERROR_TAG_UNSUPPORTED_KIND: u8 = 4;
/// Wire tag for [`SpawnError::ParentNotFound`].
pub(crate) const SPAWN_ERROR_TAG_PARENT_NOT_FOUND: u8 = 5;
/// Wire tag for [`SpawnError::ParentKindMismatch`].
pub(crate) const SPAWN_ERROR_TAG_PARENT_KIND_MISMATCH: u8 = 6;
/// Wire tag for [`SpawnError::IdempotencyConflict`] (ADR-0126).
pub(crate) const SPAWN_ERROR_TAG_IDEMPOTENCY_CONFLICT: u8 = 7;
}

// `MoveResult` / `MoveError` tags (ADR-0056).
wire_tags! { MoveResult;
/// Wire tag for [`MoveResult::Ok`].
pub(crate) const MOVE_RESULT_OK: u8 = 0;
/// Wire tag for [`MoveResult::Err`].
pub(crate) const MOVE_RESULT_ERR: u8 = 1;
}
wire_tags! { MoveError;
/// Wire tag for [`MoveError::MoveFailed`].
pub(crate) const MOVE_ERROR_TAG_MOVE_FAILED: u8 = 0;
/// Wire tag for [`MoveError::UnsupportedSatelliteRoute`].
pub(crate) const MOVE_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE: u8 = 1;
}

// Wire tags for the `Scope` tagged union (SPEC §7.4 / §11.L3).
wire_tags! { Scope;
/// Wire tag for [`Scope::Resource`].
pub(crate) const SCOPE_TAG_RESOURCE: u8 = 0;
/// Wire tag for [`Scope::Group`].
pub(crate) const SCOPE_TAG_GROUP: u8 = 1;
/// Wire tag for [`Scope::Global`].
pub(crate) const SCOPE_TAG_GLOBAL: u8 = 2;
}

// Control-plane command envelope (SPEC §5, ADR-0021): typed `Command`s
// correlated by `request_id` instead of per-verb frames.

wire_tags! { Message;
/// Discriminant for `COMMAND` (client to server, `docs/spec/L1.md` §5).
pub const TYPE_COMMAND: u8 = FrameType::Command as u8;
/// Discriminant for `COMMAND_RESULT` (server to client, `docs/spec/L1.md` §5).
pub const TYPE_COMMAND_RESULT: u8 = FrameType::CommandResult as u8;
}

// Agent-event push stream (SPEC §7.5, ADR-0022).

wire_tags! { Message;
/// Discriminant for `SUBSCRIBE_EVENTS` (client to server, `docs/spec/L1.md` §7.5).
pub const TYPE_SUBSCRIBE_EVENTS: u8 = FrameType::SubscribeEvents as u8;
/// Discriminant for `EVENT` (server to client, `docs/spec/L1.md` §7.5).
pub const TYPE_EVENT: u8 = FrameType::Event as u8;
}

// `AgentEvent` tags (SPEC §7.5 / §10.3). Each event is `tag: u8` + a
// length-prefixed body, so an unknown tag skips to [`AgentEvent::Unknown`].
wire_tags! { Event;
/// Wire tag for [`AgentEvent::CommandStarted`].
pub(crate) const EVENT_TAG_COMMAND_STARTED: u8 = 0x00;
/// Wire tag for [`AgentEvent::CommandFinished`].
pub(crate) const EVENT_TAG_COMMAND_FINISHED: u8 = 0x01;
/// Wire tag for [`AgentEvent::TitleChanged`].
pub(crate) const EVENT_TAG_TITLE_CHANGED: u8 = 0x02;
/// Wire tag for [`AgentEvent::Bell`].
pub(crate) const EVENT_TAG_BELL: u8 = 0x03;
/// Wire tag for [`AgentEvent::ResourceSpawned`].
pub(crate) const EVENT_TAG_RESOURCE_SPAWNED: u8 = 0x04;
/// Wire tag for [`AgentEvent::ResourceClosed`].
pub(crate) const EVENT_TAG_RESOURCE_CLOSED: u8 = 0x05;
/// Wire tag for [`AgentEvent::Dirty`].
pub(crate) const EVENT_TAG_DIRTY: u8 = 0x06;
/// Wire tag for [`AgentEvent::Idle`].
pub(crate) const EVENT_TAG_IDLE: u8 = 0x07;
/// Wire tag for [`AgentEvent::TerminalControl`]. The supervisory broadcast
/// (ADR-0033): emitted to every subscriber whenever a Terminal's input lease
/// or process lifecycle changes — who holds the wheel, and `Running` /
/// `Frozen` / `Exited`. The live-dashboard signal and the seed of the
/// recorded audit trail.
pub(crate) const EVENT_TAG_TERMINAL_CONTROL: u8 = 0x08;
/// Wire tag for [`AgentEvent::Asked`]. Appended after `TERMINAL_CONTROL`'s
/// `0x08`; `ASKED` is an additive agent-surface event (phux-2sl6) that carries
/// an agent's pending human-answerable question so a projection consumer can
/// render the waiting prompt without re-deriving it from the grid. Its body
/// is field-tagged TLV (not positional) so the suggestion list and the
/// optional elapsed counter are additive and an older decoder skips the whole
/// event by its length prefix as [`AgentEvent::Unknown`].
pub(crate) const EVENT_TAG_ASKED: u8 = 0x09;
/// Wire tag for [`AgentEvent::CwdChanged`]. Appended after `ASKED`'s `0x09`
/// (phux-foz.4): the scoped Terminal's working directory changed. Sourced
/// server-side from the kernel cwd of the PTY child (the same query the
/// spawn-inheritance path uses), polled at OSC-133 prompt boundaries and
/// output-idle and coalesced on change. Backs the `cwd` status widget.
pub(crate) const EVENT_TAG_CWD_CHANGED: u8 = 0x0a;
/// Wire tag for [`AgentEvent::JournalGap`] (ADR-0123): the subscription
/// missed a range of journaled events and the consumer re-reads level state.
/// A per-subscription notice, never journaled itself.
pub(crate) const EVENT_TAG_JOURNAL_GAP: u8 = 0x0b;
/// Wire tag for [`AgentEvent::SourceGap`] (ADR-0123): the scoped resource
/// produced events the server dropped before it could journal them.
pub(crate) const EVENT_TAG_SOURCE_GAP: u8 = 0x0c;
/// Wire tag for [`AgentEvent::ApprovalRequested`] (ADR-0128): a `SIGNAL`
/// action was held for approval.
pub(crate) const EVENT_TAG_APPROVAL_REQUESTED: u8 = 0x0d;
/// Wire tag for [`AgentEvent::ApprovalDecided`] (ADR-0128): a held action
/// was approved, denied, expired, or withdrawn.
pub(crate) const EVENT_TAG_APPROVAL_DECIDED: u8 = 0x0e;
}

/// Wire tag for one [`Command`] variant inside the `COMMAND` envelope (SPEC
/// §5.1); duplicates are E0081. Registry: `docs/spec/appendix-reserved.md`
/// §2. `0x0a` / `0x0b` are freed and reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
enum CommandTag {
    AttachResource = 0x01,
    DetachResource = 0x02,
    KillResource = 0x03,
    GetState = 0x05,
    GetScreen = 0x07,
    RouteInput = 0x08,
    KillResources = 0x09,
    GetTerminalState = 0x0c,
    SubscribeResourceEvents = 0x0d,
    Upgrade = 0x0e,
    AcquireInput = 0x0f,
    ReleaseInput = 0x10,
    SignalTerminal = 0x11,
    ReportAsked = 0x12,
    DetachClients = 0x13,
    ApplyInput = 0x14,
    PutFile = 0x15,
    Shutdown = 0x16,
    ReportAgentState = 0x17,
    GetPerf = 0x18,
    Transcribe = 0x19,
    AppendResourceOutput = 0x1a,
    KillResourceIf = 0x1b,
    OpenListener = 0x1c,
    CloseTabResources = 0x1d,
}

// `Command` tags (SPEC §5.1). `0x00`, `0x04`, and `0x06` are reserved catalog
// slots and decode as `UnknownEnumValue`.
wire_tags! { Command;
/// Wire tag for [`Command::AttachResource`].
pub(crate) const COMMAND_TAG_ATTACH_RESOURCE: u8 = CommandTag::AttachResource as u8;
/// Wire tag for [`Command::DetachResource`].
pub(crate) const COMMAND_TAG_DETACH_RESOURCE: u8 = CommandTag::DetachResource as u8;
/// Wire tag for [`Command::KillResource`].
pub(crate) const COMMAND_TAG_KILL_RESOURCE: u8 = CommandTag::KillResource as u8;
/// Wire tag for [`Command::GetState`].
pub(crate) const COMMAND_TAG_GET_STATE: u8 = CommandTag::GetState as u8;
/// Wire tag for [`Command::GetScreen`].
pub(crate) const COMMAND_TAG_GET_SCREEN: u8 = CommandTag::GetScreen as u8;
/// Wire tag for [`Command::RouteInput`].
pub(crate) const COMMAND_TAG_ROUTE_INPUT: u8 = CommandTag::RouteInput as u8;
/// Wire tag for [`Command::KillResources`], reusing the slot the dissolved
/// L2 lifecycle verbs freed (ADR-0019 / ADR-0027).
pub(crate) const COMMAND_TAG_KILL_RESOURCES: u8 = CommandTag::KillResources as u8;
/// Wire tag for [`Command::GetTerminalState`].
pub(crate) const COMMAND_TAG_GET_TERMINAL_STATE: u8 = CommandTag::GetTerminalState as u8;
/// Wire tag for [`Command::SubscribeResourceEvents`].
pub(crate) const COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS: u8 =
    CommandTag::SubscribeResourceEvents as u8;
/// Wire tag for [`Command::Upgrade`].
pub(crate) const COMMAND_TAG_UPGRADE: u8 = CommandTag::Upgrade as u8;
/// Wire tag for [`Command::AcquireInput`].
pub(crate) const COMMAND_TAG_ACQUIRE_INPUT: u8 = CommandTag::AcquireInput as u8;
/// Wire tag for [`Command::ReleaseInput`].
pub(crate) const COMMAND_TAG_RELEASE_INPUT: u8 = CommandTag::ReleaseInput as u8;
/// Wire tag for [`Command::SignalTerminal`].
pub(crate) const COMMAND_TAG_SIGNAL_TERMINAL: u8 = CommandTag::SignalTerminal as u8;
/// Wire tag for [`Command::ReportAsked`].
pub(crate) const COMMAND_TAG_REPORT_ASKED: u8 = CommandTag::ReportAsked as u8;
/// Wire tag for [`Command::DetachClients`].
pub(crate) const COMMAND_TAG_DETACH_CLIENTS: u8 = CommandTag::DetachClients as u8;
/// Wire tag for [`Command::ApplyInput`].
pub(crate) const COMMAND_TAG_APPLY_INPUT: u8 = CommandTag::ApplyInput as u8;
}
/// Maximum number of events in one [`Command::ApplyInput`] batch.
pub const MAX_APPLY_INPUT_EVENTS: usize = 256;
/// Maximum encoded bytes in the nested [`Command::ApplyInput`] command body.
pub const MAX_APPLY_INPUT_COMMAND_BODY: usize = 64 * 1024;
wire_tags! { Command;
/// Wire tag for [`Command::PutFile`].
pub(crate) const COMMAND_TAG_PUT_FILE: u8 = CommandTag::PutFile as u8;
/// Wire tag for [`Command::Shutdown`].
pub(crate) const COMMAND_TAG_SHUTDOWN: u8 = CommandTag::Shutdown as u8;
/// Wire tag for [`Command::ReportAgentState`].
pub(crate) const COMMAND_TAG_REPORT_AGENT_STATE: u8 = CommandTag::ReportAgentState as u8;
/// Wire tag for [`Command::GetPerf`].
pub(crate) const COMMAND_TAG_GET_PERF: u8 = CommandTag::GetPerf as u8;
/// Wire tag for [`Command::Transcribe`].
pub(crate) const COMMAND_TAG_TRANSCRIBE: u8 = CommandTag::Transcribe as u8;
/// Wire tag for [`Command::AppendResourceOutput`].
pub(crate) const COMMAND_TAG_APPEND_RESOURCE_OUTPUT: u8 = CommandTag::AppendResourceOutput as u8;
/// Wire tag for [`Command::KillResourceIf`]: a new tag rather than a field on
/// `KILL_RESOURCE`, so a peer without `CONDITIONAL_KILL` fails to decode it
/// instead of killing unconditionally (ADR-0109).
pub(crate) const COMMAND_TAG_KILL_RESOURCE_IF: u8 = CommandTag::KillResourceIf as u8;
/// Wire tag for [`Command::OpenListener`].
pub(crate) const COMMAND_TAG_OPEN_LISTENER: u8 = CommandTag::OpenListener as u8;
/// Wire tag for [`Command::CloseTabResources`].
pub(crate) const COMMAND_TAG_CLOSE_TAB_RESOURCES: u8 = CommandTag::CloseTabResources as u8;
}

// `InputEvent` tags (`ROUTE_INPUT`), mirroring the `INPUT_*` frame atoms.
wire_tags! { InputEvent;
/// Wire tag for [`InputEvent::Key`].
pub(crate) const INPUT_EVENT_TAG_KEY: u8 = 0x00;
/// Wire tag for [`InputEvent::Mouse`].
pub(crate) const INPUT_EVENT_TAG_MOUSE: u8 = 0x01;
/// Wire tag for [`InputEvent::Focus`].
pub(crate) const INPUT_EVENT_TAG_FOCUS: u8 = 0x02;
/// Wire tag for [`InputEvent::Paste`].
pub(crate) const INPUT_EVENT_TAG_PASTE: u8 = 0x03;
}
// 0x04 (`Selection`) was removed by ADR-0030.

// `StateScope` tags (SPEC §5.1).
wire_tags! { StateScope;
/// Wire tag for [`StateScope::Server`].
pub(crate) const STATE_SCOPE_TAG_SERVER: u8 = 0x00;
}

// Wire tags for the `CommandResult` tagged union (SPEC §5).
wire_tags! { CommandResult;
/// Wire tag for [`CommandResult::Ok`].
pub(crate) const COMMAND_RESULT_TAG_OK: u8 = 0x00;
/// Wire tag for [`CommandResult::OkWith`].
pub(crate) const COMMAND_RESULT_TAG_OK_WITH: u8 = 0x01;
/// Wire tag for [`CommandResult::Error`].
pub(crate) const COMMAND_RESULT_TAG_ERROR: u8 = 0x02;
}

// Wire tags for the `CommandValue` tagged union (SPEC §5).
wire_tags! { CommandValue;
/// Wire tag for [`CommandValue::ResourceId`].
pub(crate) const COMMAND_VALUE_TAG_RESOURCE_ID: u8 = 0x00;
/// Wire tag for [`CommandValue::GroupId`].
pub(crate) const COMMAND_VALUE_TAG_GROUP_ID: u8 = 0x01;
/// Wire tag for [`CommandValue::State`].
pub(crate) const COMMAND_VALUE_TAG_STATE: u8 = 0x02;
/// Wire tag for [`CommandValue::Json`].
pub(crate) const COMMAND_VALUE_TAG_JSON: u8 = 0x03;
/// Wire tag for [`CommandValue::Bytes`].
pub(crate) const COMMAND_VALUE_TAG_BYTES: u8 = 0x04;
/// Wire tag for [`CommandValue::FileUpload`].
pub(crate) const COMMAND_VALUE_TAG_FILE_UPLOAD: u8 = 0x05;
}

// Wire tags for the `AttachTarget` tagged union (SPEC §13).
wire_tags! { AttachTarget;
/// Wire tag for [`AttachTarget::Last`].
pub(crate) const ATTACH_TARGET_LAST: u8 = 0;
/// Wire tag for [`AttachTarget::ByName`].
pub(crate) const ATTACH_TARGET_BY_NAME: u8 = 1;
/// Wire tag for [`AttachTarget::ById`].
pub(crate) const ATTACH_TARGET_BY_ID: u8 = 2;
/// Wire tag for [`AttachTarget::CreateIfMissing`].
pub(crate) const ATTACH_TARGET_CREATE_IF_MISSING: u8 = 3;
}

mod codec;
mod command;
mod command_codec;
mod directory;
mod kind;
mod path;
mod payload;
mod role;
mod status;
mod whoami;

pub use command::{
    AgentEvent, ApprovalOutcome, Command, CommandResult, CommandValue, ControlAction,
    FileUploadAck, GET_SCREEN_FORMAT_SELECTOR_MASK, GET_SCREEN_FORMAT_UNWRAP, InputMode,
    KillConditions, KillPrecondition, ListenerTransport, ReportedAgentState, ResourceEventType,
    ResourceLifecycle, StateScope, TerminalSignal,
};
pub use directory::{
    DirectoryEntry, DirectoryErrorCode, DirectoryListing, DirectoryListingError,
    DirectoryListingResult, MAX_DIRECTORY_ENTRIES,
};
pub use kind::FrameKind;
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
mod tests {
    use std::collections::BTreeSet;

    /// Every `u8` wire-tag const here is declared through `wire_tags!`
    /// (E0119 on reuse), and `TYPE_` / `COMMAND_TAG_` consts take their value
    /// from [`FrameType`] / [`CommandTag`] (E0081 on reuse).
    #[test]
    fn wire_tag_consts_are_uniqueness_checked() {
        // Only the const-declaration half of the file is scanned; the test
        // module itself would otherwise match its own patterns.
        let src = include_str!("mod.rs");
        let src = src.split("#[cfg(test)]").next().unwrap_or(src);

        let mut reserved: BTreeSet<&str> = BTreeSet::new();
        let mut rest = src;
        while let Some(start) = rest.find("wire_tags!") {
            let after = &rest[start..];
            let Some(brace) = after.find('{') else {
                panic!("wire_tags! without a body");
            };
            let mut depth = 0_i32;
            let mut end = None;
            for (i, ch) in after[brace..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(brace + i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let end = end.expect("unterminated wire_tags! body");
            for name in after[brace..=end]
                .split(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            {
                if name.contains('_') && name.chars().any(|c| c.is_ascii_uppercase()) {
                    reserved.insert(name);
                }
            }
            rest = &after[end + 1..];
        }
        assert!(
            !reserved.is_empty(),
            "no wire_tags! families found - the compile-time checks are gone"
        );

        for line in src.lines() {
            let line = line.trim();
            let decl = line
                .strip_prefix("pub const ")
                .or_else(|| line.strip_prefix("pub(crate) const "));
            let Some(decl) = decl else { continue };
            let Some((name, expr)) = decl.split_once(": u8 = ") else {
                continue;
            };
            let expr = expr.trim_end_matches(';').trim();
            if name.starts_with("TYPE_") {
                assert!(
                    expr.starts_with("FrameType::"),
                    "{name} must take its value from the FrameType enum so a duplicate \
                     discriminant is a compile error, found `{expr}`"
                );
                assert!(
                    reserved.contains(name),
                    "{name} must be declared through wire_tags! so a reused byte is E0119"
                );
            } else if name.starts_with("COMMAND_TAG_") {
                assert!(
                    expr.starts_with("CommandTag::"),
                    "{name} must take its value from the CommandTag enum so a duplicate \
                     discriminant is a compile error, found `{expr}`"
                );
                assert!(
                    reserved.contains(name),
                    "{name} must be declared through wire_tags! so a reused byte is E0119"
                );
            } else if expr.starts_with("0x") || expr.starts_with(|c: char| c.is_ascii_digit()) {
                assert!(
                    reserved.contains(name),
                    "{name} is a hand-allocated literal wire tag missing from a \
                     wire_tags! family"
                );
            }
        }
    }
}
