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
//! Nested tagged unions and sub-records (e.g. `TerminalId`, `ViewportInfo`,
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
}

/// `PING` / `PONG` body fields (`docs/spec/proto.md` §7.4).
pub mod ping {
    /// Nonce the peer echoes back (`u64`). Shared id for `PING` and `PONG`.
    pub const NONCE: u32 = 1;
}

/// `TERMINAL_OUTPUT` body fields (`docs/spec/L1.md` §8.1, ADR-0013).
pub mod terminal_output {
    /// Target `TerminalId` (positional tagged union).
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
    /// Target `TerminalId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// `KeyEvent` (positional sub-record).
    pub const EVENT: u32 = 2;
}

/// `INPUT_MOUSE` body fields (`docs/spec/input.md` §3).
pub mod input_mouse {
    /// Target `TerminalId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// `MouseEvent` (positional sub-record).
    pub const EVENT: u32 = 2;
}

/// `INPUT_FOCUS` body fields (`docs/spec/input.md` §4).
pub mod input_focus {
    /// Target `TerminalId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// Focus kind (`u8`: gained=0 / lost=1).
    pub const EVENT: u32 = 2;
}

/// `INPUT_PASTE` body fields (`docs/spec/input.md` §5).
pub mod input_paste {
    /// Target `TerminalId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// `PasteEvent` (positional sub-record: trust byte + bytes).
    pub const EVENT: u32 = 2;
}

/// `FRAME_ACK` body fields (`docs/spec/proto.md` §7.2 / §8.2).
pub mod frame_ack {
    /// Acked `TerminalId` (positional tagged union).
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
    /// Target `TerminalId`.
    pub const TERMINAL_ID: u32 = 1;
    /// Logical `StreamId`.
    pub const STREAM_ID: u32 = 2;
    /// Replica `BootstrapId`.
    pub const BOOTSTRAP_ID: u32 = 3;
    /// Opaque current cursor.
    pub const CURSOR: u32 = 4;
    /// Non-zero requested maximum page bytes (`u32`).
    pub const MAX_BYTES: u32 = 5;
}

/// `BOOTSTRAP_BEGIN` body fields (`docs/spec/L1.md` §4.3).
pub mod bootstrap_begin {
    /// Target `TerminalId`.
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
    /// Target `TerminalId`.
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

/// `BOOTSTRAP_READY` body fields (`docs/spec/L1.md` §4.3).
pub mod bootstrap_ready {
    /// Target `TerminalId`.
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
    /// Target `TerminalId`.
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
}

/// `BOOTSTRAP_TOMBSTONE` body fields (`docs/spec/L1.md` §4.6).
pub mod bootstrap_tombstone {
    /// Target `TerminalId`.
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

/// `SPAWN_TERMINAL` body fields (`docs/spec/L1.md` §10.1).
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
}

/// `TERMINAL_SPAWNED` body fields (`docs/spec/L1.md` §10.1).
pub mod terminal_spawned {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// `SpawnResult` tagged union (positional).
    pub const RESULT: u32 = 2;
}

/// `TERMINAL_CLOSED` body fields (`docs/spec/L1.md` §10.1).
pub mod terminal_closed {
    /// Closed `TerminalId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// Optional exit status (absent field = signal / unknown).
    pub const EXIT_STATUS: u32 = 2;
}

/// `TERMINAL_RESIZE` body fields (`docs/spec/L1.md` §10.2).
pub mod terminal_resize {
    /// Target `TerminalId` (positional tagged union).
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
    /// Optional `TerminalId` scope (absent field = server-scoped `None`).
    pub const TERMINAL: u32 = 1;
}

/// `EVENT` body fields (`docs/spec/L1.md` §7.5).
pub mod event {
    /// Optional `TerminalId` scope (absent field = server-scoped `None`).
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

// -----------------------------------------------------------------------------
// `SessionId` tagged union — ADR-0007 §3
// -----------------------------------------------------------------------------

/// `SessionId::Local` tag.
pub const SESSION_ID_TAG_LOCAL: u32 = 0;
/// `SessionId::Satellite` tag (reserved for v0.2+; decoders MUST reject).
pub const SESSION_ID_TAG_SATELLITE: u32 = 1;

// The `TerminalId` wire-side tag bytes (`u8`) live in `crate::ids` alongside the
// [`TerminalId`](crate::ids::TerminalId) definition — ADR-0016 §Decision.
