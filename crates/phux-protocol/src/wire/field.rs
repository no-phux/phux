//! TLV field-ID constants used inside message bodies.
//!
//! Owned by phux-6yl.4. See `docs/spec/proto.md` §7 (message catalog) and
//! `docs/spec/appendix-encoding.md` (field-tagged TLV encoding). Every message
//! body is encoded field-tagged: each top-level field is written as
//! `field_id: varint || wire_type: u8 || length-delimited value`, and decoders
//! match fields by id, skipping any id they do not recognise by its length.
//!
//! # Field-id allocation discipline
//!
//! - Field ids are **per message**: each message's body has its own id space
//!   starting at `1` and running **contiguously** for that message's fields,
//!   in the order the fields are declared. (Two messages may both use id `1`;
//!   ids are scoped to the message, the way the type byte already scopes the
//!   body.)
//! - Field ids are **stable within a major protocol version**: an additive
//!   minor-version change MAY append a new id after the existing ones but MUST
//!   NOT renumber or reuse an existing id. A removed field's id is retired,
//!   not recycled.
//! - An **optional or trailing** field is a simply-absent tagged field: the
//!   encoder writes no field for `None` / an empty trailing value, and the
//!   decoder applies the documented default when the id is absent. This is the
//!   forward-compat mechanism — peers round-trip by id, not by position.
//! - The constants below are grouped one `mod` per message so the per-message
//!   `1, 2, 3, …` allocation is self-evident and a new field appends to the
//!   end of its module.
//!
//! Nested tagged unions and sub-records (e.g. `ResourceId`, `ViewportInfo`,
//! `Command`, `SessionSnapshot`) are encoded *positionally* inside a field's
//! length-delimited value; only the message body itself is field-tagged. Their
//! wire-tag bytes live alongside their definitions in `wire::frame` /
//! `wire::info` / `crate::ids`.

/// `HELLO` body fields (`docs/spec/proto.md` §6.1).
pub mod hello {
    /// Free-form client identifier string.
    pub const CLIENT_NAME: u32 = 1;
    /// Protocol major version (`u16`).
    pub const PROTOCOL_MAJOR: u32 = 2;
    /// Protocol minor version (`u16`).
    pub const PROTOCOL_MINOR: u32 = 3;
    /// Protocol patch version (`u16`).
    pub const PROTOCOL_PATCH: u32 = 4;
    /// `ClientCapabilities` blob (positional sub-record).
    pub const CLIENT_CAPS: u32 = 5;
    /// Frame compressions the client accepts (`u8` bitset), additive.
    ///
    /// A top-level field rather than a member of the `CLIENT_CAPS`
    /// sub-record because protocol 0.8 fixes that sub-record's byte order
    /// exactly (`docs/spec/proto.md` §6.2): appending to it would be a
    /// fleet-wide break, while an unknown top-level id is skipped by
    /// declared length. Absent means "accepts nothing compressed".
    pub const COMPRESSION: u32 = 6;
    // Ids 7 and 8 are the spec-only `phux-workload/v1` fields
    // (`docs/spec/proto.md` §6.1.1); they stay reserved here.
    /// The ssh endpoints `phux stdio-bridge` stamps on a relayed HELLO.
    ///
    /// A positional `SshOrigin` sub-record, additive (`docs/spec/L3.md`
    /// §3.9). The server honors it only from a same-uid Unix-socket peer, and
    /// only to report the route.
    pub const SSH_ORIGIN: u32 = 9;
}

/// `HELLO_OK` body fields (`docs/spec/proto.md` §6.1).
pub mod hello_ok {
    /// Selected protocol major version (`u16`).
    pub const PROTOCOL_MAJOR: u32 = 1;
    /// Selected protocol minor version (`u16`).
    pub const PROTOCOL_MINOR: u32 = 2;
    /// Selected protocol patch version (`u16`).
    pub const PROTOCOL_PATCH: u32 = 3;
    /// `ServerCapabilities` blob (positional sub-record).
    pub const SERVER_CAPS: u32 = 4;
    /// Opaque server identity bytes.
    pub const SERVER_ID: u32 = 5;
    /// Selected explicit `BootstrapProfile` sub-record.
    pub const SELECTED_PROFILE: u32 = 6;
    /// Negotiated maximum `BOOTSTRAP_CHUNK.payload` bytes (`u32`).
    pub const MAX_CHUNK_BYTES: u32 = 7;
    /// Negotiated maximum `HISTORY_PAGE.payload` bytes (`u32`).
    pub const MAX_HISTORY_PAGE_BYTES: u32 = 8;
    /// Selected frame compression (`u8` enum tag), additive. Absent or `0`
    /// means the server compresses nothing.
    pub const COMPRESSION: u32 = 9;
}

/// `PING` / `PONG` body fields (`docs/spec/proto.md` §7.4).
pub mod ping {
    /// Nonce the peer echoes back (`u64`). Shared id for `PING` and `PONG`.
    pub const NONCE: u32 = 1;
}

/// `RESOURCE_OUTPUT` body fields (`docs/spec/L1.md` §8.1, ADR-0013).
pub mod terminal_output {
    /// Target `ResourceId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// Monotonic per-terminal sequence id (`u64`).
    pub const SEQ: u32 = 2;
    /// VT bytes from the PTY.
    pub const BYTES: u32 = 3;
    /// Logical `StreamId` (`u64`, non-zero).
    pub const STREAM_ID: u32 = 4;
    /// Replica `BootstrapId` (`u64`, non-zero).
    pub const BOOTSTRAP_ID: u32 = 5;
}

/// `ATTACH` body fields (`docs/spec/proto.md` §7.1 / §13).
pub mod attach {
    /// `AttachTarget` tagged union (positional).
    pub const TARGET: u32 = 1;
    /// `ViewportInfo` (positional sub-record).
    pub const VIEWPORT: u32 = 2;
    /// `request_scrollback: bool`.
    pub const REQUEST_SCROLLBACK: u32 = 3;
    /// `scrollback_limit_lines: u32`.
    pub const SCROLLBACK_LIMIT_LINES: u32 = 4;
    /// Client-chosen attach correlation id (`u32`).
    pub const ATTACH_ID: u32 = 5;
}

/// `INPUT_KEY` body fields (`docs/spec/input.md` §2).
pub mod input_key {
    /// Target `ResourceId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// `KeyEvent` (positional sub-record).
    pub const EVENT: u32 = 2;
}

/// `INPUT_MOUSE` body fields (`docs/spec/input.md` §3).
pub mod input_mouse {
    /// Target `ResourceId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// `MouseEvent` (positional sub-record).
    pub const EVENT: u32 = 2;
}

/// `INPUT_FOCUS` body fields (`docs/spec/input.md` §4).
pub mod input_focus {
    /// Target `ResourceId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// Focus kind (`u8`: gained=0 / lost=1).
    pub const EVENT: u32 = 2;
}

/// `INPUT_PASTE` body fields (`docs/spec/input.md` §5).
pub mod input_paste {
    /// Target `ResourceId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// `PasteEvent` (positional sub-record: trust byte + bytes).
    pub const EVENT: u32 = 2;
}

/// `INPUT_TERMINAL_REPLY` body fields (`docs/spec/input.md` §6).
pub mod input_terminal_reply {
    /// Attached target `ResourceId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// Opaque terminal-emulator-generated PTY reply bytes.
    pub const BYTES: u32 = 2;
}

/// `FRAME_ACK` body fields (`docs/spec/proto.md` §7.2 / §8.2).
pub mod frame_ack {
    /// Acked `ResourceId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// Acked sequence id (`u64`).
    pub const SEQ: u32 = 2;
    /// Logical `StreamId` (`u64`, non-zero).
    pub const STREAM_ID: u32 = 3;
    /// Replica `BootstrapId` (`u64`, non-zero).
    pub const BOOTSTRAP_ID: u32 = 4;
}

/// `VIEWPORT_RESIZE` body fields (`docs/spec/proto.md` §7.1 / §10.5).
pub mod viewport_resize {
    /// New `ViewportInfo` (positional sub-record).
    pub const VIEWPORT: u32 = 1;
}

/// `ATTACHED` body fields (`docs/spec/L1.md` §8).
pub mod attached {
    /// Full `SessionSnapshot` (positional sub-record).
    pub const SNAPSHOT: u32 = 1;
    /// Server-allocated `ClientId` for this attachment (`u32`).
    pub const INITIAL_CLIENT_ID: u32 = 2;
    /// Client-chosen attach correlation id (`u32`).
    pub const ATTACH_ID: u32 = 3;
}

/// `ATTACH_READY` body fields (`docs/spec/L1.md` §8).
pub mod attach_ready {
    /// Client-chosen attach correlation id (`u32`).
    pub const ATTACH_ID: u32 = 1;
}

/// `HISTORY_REQUEST` body fields (`docs/spec/L1.md` §4.5).
pub mod history_request {
    /// Target `ResourceId`.
    pub const TERMINAL_ID: u32 = 1;
    /// Logical `StreamId`.
    pub const STREAM_ID: u32 = 2;
    /// Replica `BootstrapId`.
    pub const BOOTSTRAP_ID: u32 = 3;
    /// Opaque current cursor.
    pub const CURSOR: u32 = 4;
    /// Requested maximum page bytes (`u32`); zero is retryably rejected.
    pub const MAX_BYTES: u32 = 5;
    /// Requested maximum page rows (`u32`); zero is retryably rejected.
    pub const MAX_ROWS: u32 = 6;
}

/// `BOOTSTRAP_BEGIN` body fields (`docs/spec/L1.md` §4.3).
pub mod bootstrap_begin {
    /// Target `ResourceId`.
    pub const TERMINAL_ID: u32 = 1;
    /// Logical `StreamId`.
    pub const STREAM_ID: u32 = 2;
    /// Replica `BootstrapId`.
    pub const BOOTSTRAP_ID: u32 = 3;
    /// Concrete bootstrap codec.
    pub const CODEC: u32 = 4;
    /// Authoritative columns (`u16`).
    pub const COLS: u32 = 5;
    /// Authoritative rows (`u16`).
    pub const ROWS: u32 = 6;
    /// Live emitter (`OutputMode`).
    pub const OUTPUT_MODE: u32 = 7;
    /// Actor cut sequence (`u64`).
    pub const BASE_SEQ: u32 = 8;
}

/// `BOOTSTRAP_CHUNK` body fields (`docs/spec/L1.md` §4.3).
pub mod bootstrap_chunk {
    /// Target `ResourceId`.
    pub const TERMINAL_ID: u32 = 1;
    /// Logical `StreamId`.
    pub const STREAM_ID: u32 = 2;
    /// Replica `BootstrapId`.
    pub const BOOTSTRAP_ID: u32 = 3;
    /// Zero-based contiguous chunk sequence (`u32`).
    pub const CHUNK_SEQ: u32 = 4;
    /// Opaque checkpoint bytes.
    pub const PAYLOAD: u32 = 5;
}

/// `FRAME_COMPRESSED` body fields (`docs/spec/proto.md` §6.4).
pub mod frame_compressed {
    /// Compression algorithm tag (`u8`).
    pub const ALGORITHM: u32 = 1;
    /// Exact byte length of the inflated inner frame body (`u32`).
    pub const UNCOMPRESSED_LEN: u32 = 2;
    /// Compressed image of one complete inner frame body: its type byte
    /// followed by its payload, i.e. everything after the length prefix.
    pub const PAYLOAD: u32 = 3;
}

/// `BOOTSTRAP_READY` body fields (`docs/spec/L1.md` §4.3).
pub mod bootstrap_ready {
    /// Target `ResourceId`.
    pub const TERMINAL_ID: u32 = 1;
    /// Logical `StreamId`.
    pub const STREAM_ID: u32 = 2;
    /// Replica `BootstrapId`.
    pub const BOOTSTRAP_ID: u32 = 3;
    /// Optional opaque history cursor.
    pub const HISTORY_CURSOR: u32 = 4;
}

/// `HISTORY_PAGE` body fields (`docs/spec/L1.md` §4.5).
pub mod history_page {
    /// Target `ResourceId`.
    pub const TERMINAL_ID: u32 = 1;
    /// Logical `StreamId`.
    pub const STREAM_ID: u32 = 2;
    /// Replica `BootstrapId`.
    pub const BOOTSTRAP_ID: u32 = 3;
    /// Opaque cursor consumed by this page.
    pub const CURSOR: u32 = 4;
    /// Optional cursor for the next older page.
    pub const NEXT_CURSOR: u32 = 5;
    /// Opaque selected-codec page bytes.
    pub const PAYLOAD: u32 = 6;
    /// Non-zero page sequence within one generation-bound cursor lineage.
    pub const PAGE_SEQ: u32 = 7;
    /// Number of native history rows encoded by the payload (`u32`).
    pub const ROWS: u32 = 8;
}

/// `BOOTSTRAP_TOMBSTONE` body fields (`docs/spec/L1.md` §4.6).
pub mod bootstrap_tombstone {
    /// Target `ResourceId`.
    pub const TERMINAL_ID: u32 = 1;
    /// Logical `StreamId`.
    pub const STREAM_ID: u32 = 2;
    /// Replica `BootstrapId`.
    pub const BOOTSTRAP_ID: u32 = 3;
    /// `TombstoneReason` tag (`u8`).
    pub const REASON: u32 = 4;
    /// Highest valid live sequence (`u64`).
    pub const LAST_VALID_SEQ: u32 = 5;
}

/// `HISTORY_TOMBSTONE` body fields (`docs/spec/L1.md` §4.5).
pub mod history_tombstone {
    /// Target `ResourceId`.
    pub const TERMINAL_ID: u32 = 1;
    /// Logical `StreamId`.
    pub const STREAM_ID: u32 = 2;
    /// Replica `BootstrapId`.
    pub const BOOTSTRAP_ID: u32 = 3;
    /// Opaque invalidated history cursor.
    pub const CURSOR: u32 = 4;
    /// `HistoryTombstoneReason` tag (`u8`).
    pub const REASON: u32 = 5;
}

/// `HISTORY_REJECTED` body fields (`docs/spec/L1.md` §4.5).
pub mod history_rejected {
    /// Target `ResourceId`.
    pub const TERMINAL_ID: u32 = 1;
    /// Logical `StreamId`.
    pub const STREAM_ID: u32 = 2;
    /// Replica `BootstrapId`.
    pub const BOOTSTRAP_ID: u32 = 3;
    /// Opaque history cursor that was not advanced.
    pub const CURSOR: u32 = 4;
    /// `HistoryRejectionReason` tag (`u8`).
    pub const REASON: u32 = 5;
    /// Non-zero required retry byte limit (`u32`).
    pub const REQUIRED_BYTES: u32 = 6;
    /// Non-zero required retry row limit (`u32`).
    pub const REQUIRED_ROWS: u32 = 7;
}

/// `DETACHED` body fields (`docs/spec/proto.md` §7.2).
///
/// Both ids are optional-absent: a server that predates `0.7.0-draft.7`
/// encodes an empty `DETACHED` body, and a consumer applies the documented
/// defaults (`reason` unstated, `message` empty) rather than failing.
pub mod detached {
    /// `DetachReason` tag (`u8`). Absent = the peer stated no reason.
    pub const REASON: u32 = 1;
    /// Human-readable UTF-8 detail. Absent = empty.
    pub const MESSAGE: u32 = 2;
}

/// `BELL` body fields (`docs/spec/L1.md` §1.2).
pub mod bell {
    /// Terminal that received the bell character (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
}

/// `ERROR` body fields (`docs/spec/proto.md` §9 / §14).
pub mod error {
    /// Optional correlating `request_id` (absent field = `None`).
    pub const REQUEST_ID: u32 = 1;
    /// Structured `ErrorCode` (`u16`).
    pub const CODE: u32 = 2;
    /// Human-readable UTF-8 message.
    pub const MESSAGE: u32 = 3;
}

/// `GET_METADATA` / `DELETE_METADATA` body fields (`docs/spec/L3.md` §1).
pub mod get_metadata {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// `Scope` tagged union (positional).
    pub const SCOPE: u32 = 2;
    /// Metadata key string.
    pub const KEY: u32 = 3;
}

/// `SET_METADATA` body fields (`docs/spec/L3.md` §1).
pub mod set_metadata {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// `Scope` tagged union (positional).
    pub const SCOPE: u32 = 2;
    /// Metadata key string.
    pub const KEY: u32 = 3;
    /// Metadata value bytes.
    pub const VALUE: u32 = 4;
}

/// `LIST_METADATA` body fields (`docs/spec/L3.md` §1).
pub mod list_metadata {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// `Scope` tagged union (positional).
    pub const SCOPE: u32 = 2;
}

/// `SUBSCRIBE_METADATA` body fields (`docs/spec/L3.md` §1).
pub mod subscribe_metadata {
    /// `Scope` tagged union (positional).
    pub const SCOPE: u32 = 1;
    /// Metadata key string.
    pub const KEY: u32 = 2;
}

/// `METADATA_CHANGED` body fields (`docs/spec/L3.md` §1).
pub mod metadata_changed {
    /// `Scope` tagged union (positional).
    pub const SCOPE: u32 = 1;
    /// Metadata key string.
    pub const KEY: u32 = 2;
    /// Optional new value bytes (absent field = `None` / tombstone).
    pub const VALUE: u32 = 3;
}

/// `METADATA_VALUE` body fields (`docs/spec/L3.md` §1).
pub mod metadata_value {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// Optional value bytes (absent field = key absent).
    pub const VALUE: u32 = 2;
}

/// `METADATA_KEYS` body fields (`docs/spec/L3.md` §1).
pub mod metadata_keys {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// Sorted list of key names (positional `u32` count + strings).
    pub const KEYS: u32 = 2;
}

/// `LIST_DIRECTORY` body fields (`docs/spec/L3.md` §4).
pub mod list_directory {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// Requested path (UTF-8). Empty or `~` = the serving user's home.
    pub const PATH: u32 = 2;
    /// Optional satellite host name (UTF-8): list on that satellite through
    /// the hub instead of on the serving host. Gated on
    /// `ServerFeature::LIST_DIRECTORY_HOST`.
    pub const HOST: u32 = 3;
}

/// `DIRECTORY_LISTING` body fields (`docs/spec/L3.md` §4).
pub mod directory_listing {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// Resolved absolute path (or the attempted path on a refusal).
    pub const PATH: u32 = 2;
    /// Optional lexical parent (absent at the root or on a refusal).
    pub const PARENT: u32 = 3;
    /// Child directories: positional `u32` count + (name str, flags `u8`).
    pub const ENTRIES: u32 = 4;
    /// Optional truncation flag (`u8`, absent = not truncated).
    pub const TRUNCATED: u32 = 5;
    /// Optional `DirectoryErrorCode` (`u8`); present = the listing was refused.
    pub const ERROR: u32 = 6;
    /// Optional diagnostic text accompanying `ERROR`.
    pub const MESSAGE: u32 = 7;
}

/// `SPAWN_RESOURCE` body fields (`docs/spec/L1.md` §10.1).
pub mod spawn_terminal {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// Target `GroupId` (`u32`).
    pub const GROUP: u32 = 2;
    /// Optional command argv (absent field = `None`).
    pub const COMMAND: u32 = 3;
    /// Optional working directory (absent field = `None`).
    pub const CWD: u32 = 4;
    /// Optional environment pairs (absent field = `None`).
    pub const ENV: u32 = 5;
    /// Optional first-class `TERM` for the new Terminal (absent = `None`).
    pub const TERM: u32 = 6;
    /// Optional satellite host to spawn on (absent = `None`, spawn locally).
    /// Federation-hub addressing per `docs/spec/L1.md` §3.1 / §9.1
    /// (phux-v45.6, ADR-0007).
    pub const SATELLITE: u32 = 7;
    /// Optional existing Terminal whose owning window must host the spawn.
    /// Absent preserves the server's legacy placement policy.
    pub const OWNER_TERMINAL: u32 = 8;
    /// Optional opaque `phux.agent-session/v1` record installed atomically on
    /// the new local Terminal.
    pub const AGENT_SESSION: u32 = 9;
    /// Optional initial grid the new Terminal is created at: `cols: u16`
    /// followed by `rows: u16`, both big-endian. Absent (or zero on either
    /// axis) leaves the server's default grid in force.
    pub const INITIAL_SIZE: u32 = 10;
    /// Optional `ResourceKind` tag (`u8`); absent = `Terminal`.
    ///
    /// Every pre-kind body therefore decodes as a Terminal spawn. Decoders
    /// validate the remaining fields per kind: an `AgentSession` spawn
    /// requires `PARENT` and `PROVIDER` and carries none of `COMMAND`,
    /// `CWD`, `ENV`, `TERM`, `OWNER_TERMINAL`, or `INITIAL_SIZE`; a
    /// `Terminal` spawn carries none of fields 12-14.
    pub const KIND: u32 = 11;
    /// Optional parent resource (positional tagged `ResourceId`). Required
    /// for `AgentSession`, which is always bound to a Terminal parent.
    pub const PARENT: u32 = 12;
    /// Optional agent provider name (`str`, e.g. `claude`). Required for
    /// `AgentSession`.
    pub const PROVIDER: u32 = 13;
    /// Optional opaque provider-native session id (`str`).
    pub const NATIVE_ID: u32 = 14;
}

/// `RESOURCE_SPAWNED` body fields (`docs/spec/L1.md` §10.1).
pub mod terminal_spawned {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// `SpawnResult` tagged union (positional).
    pub const RESULT: u32 = 2;
}

/// `MOVE_RESOURCE` body fields (`docs/spec/L1.md` §10.1; ADR-0056).
pub mod move_terminal {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// The Terminal to re-parent (positional tagged union).
    pub const TERMINAL: u32 = 2;
    /// Existing Terminal whose owning window becomes the destination
    /// (positional tagged union). Ownership address only, as in
    /// `SPAWN_RESOURCE.owner_terminal`.
    pub const OWNER_TERMINAL: u32 = 3;
}

/// `RESOURCE_MOVED` body fields (`docs/spec/L1.md` §10.1; ADR-0056).
pub mod terminal_moved {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// `MoveResult` tagged union (positional).
    pub const RESULT: u32 = 2;
}

/// `RESOURCE_CLOSED` body fields (`docs/spec/L1.md` §10.1).
pub mod terminal_closed {
    /// Closed `ResourceId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// Optional exit status (absent field = signal / unknown).
    pub const EXIT_STATUS: u32 = 2;
    /// Optional `CloseReason` tag (`u8`). Absent = the server stated no
    /// reason (`CloseReason::Unknown`), which is what every pre-reason body
    /// decodes as.
    pub const REASON: u32 = 3;
}

/// `RESIZE_TERMINAL` body fields (`docs/spec/L1.md` §10.2).
pub mod terminal_resize {
    /// Target `ResourceId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// New column count (`u16`).
    pub const COLS: u32 = 2;
    /// New row count (`u16`).
    pub const ROWS: u32 = 3;
}

/// `COMMAND` body fields (`docs/spec/L1.md` §5).
pub mod command {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// `Command` tagged union (positional).
    pub const COMMAND: u32 = 2;
}

/// `COMMAND_RESULT` body fields (`docs/spec/L1.md` §5).
pub mod command_result {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// `CommandResult` tagged union (positional).
    pub const RESULT: u32 = 2;
}

/// `SUBSCRIBE_EVENTS` body fields (`docs/spec/L1.md` §7.5).
pub mod subscribe_events {
    /// Optional `ResourceId` scope (absent field = server-scoped `None`).
    pub const TERMINAL: u32 = 1;
}

/// `EVENT` body fields (`docs/spec/L1.md` §7.5).
pub mod event {
    /// Optional `ResourceId` scope (absent field = server-scoped `None`).
    pub const TERMINAL: u32 = 1;
    /// `AgentEvent` tagged union (positional TLV: tag + length-prefixed body).
    pub const EVENT: u32 = 2;
}

/// `AgentEvent::Asked` body fields (`docs/spec/L1.md` §7.5).
///
/// Unlike the other `AgentEvent` bodies (positional), the `ASKED` body is
/// field-tagged TLV: each field is `field_id || wire_type || length-delimited
/// value`, read by `Decoder::read_field` which skips an unrecognised field by
/// its length. This is what lets the optional `elapsed_seconds` and any future
/// field be additive — a present field carries the value, an absent field is
/// the default — while the whole event still skips cleanly to
/// [`crate::wire::frame::AgentEvent::Unknown`] for an older decoder via the
/// event's outer length prefix.
pub mod event_asked {
    /// Stable question id (`str`) the answer correlates against.
    pub const ID: u32 = 1;
    /// The question text (`str`) presented to the human.
    pub const QUESTION: u32 = 2;
    /// One suggested answer (`str`). Repeated once per suggestion, in order;
    /// absent when there are no suggestions.
    pub const SUGGESTION: u32 = 3;
    /// Optional seconds the agent has been waiting (`u64`); an absent field is
    /// `0` / unknown.
    pub const ELAPSED_SECONDS: u32 = 4;
}

/// `AgentEvent::ResourceSpawned` body fields (`docs/spec/L1.md` §7.1).
///
/// Field-tagged TLV like [`event_asked`]: a body that predates the fields is
/// empty and decodes as a root Terminal, and an older decoder ignores the
/// body entirely, so both fields are additive.
pub mod event_pane_spawned {
    /// `ResourceKind` tag (`u8`). Absent = `Terminal`; the encoder writes it
    /// only for another kind.
    pub const KIND: u32 = 1;
    /// Parent resource (positional tagged `ResourceId`). Absent = a root.
    pub const PARENT: u32 = 2;
}

// -----------------------------------------------------------------------------
// `SessionId` tagged union — ADR-0007 §3
// -----------------------------------------------------------------------------

/// `SessionId::Local` tag.
pub const SESSION_ID_TAG_LOCAL: u32 = 0;
/// `SessionId::Satellite` tag (reserved for v0.2+; decoders MUST reject).
pub const SESSION_ID_TAG_SATELLITE: u32 = 1;

// The `ResourceId` wire-side tag bytes (`u8`) live in `crate::ids` alongside the
// [`ResourceId`](crate::ids::ResourceId) definition — ADR-0016 §Decision.
