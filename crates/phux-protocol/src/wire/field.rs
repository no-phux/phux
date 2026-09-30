//! TLV field ids inside message bodies (`docs/spec/appendix-encoding.md`).
//!
//! Each top-level field is `field_id: varint || wire_type: u8 || value`;
//! decoders skip unknown ids by length. Ids are per message, start at `1`,
//! and are stable within a major version: new fields append, removed ids are
//! retired, never reused. An optional field is simply absent. Nested unions
//! and sub-records stay positional inside a field's value.

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
    /// Frame compressions the client accepts (`u8` bitset); absent = none.
    /// Top-level because the `CLIENT_CAPS` byte order is fixed (§6.2).
    pub const COMPRESSION: u32 = 6;
    // Ids 7 and 8 are retired-unshipped (ADR-0116) and stay reserved.
    /// Positional `SshOrigin` stamped by `phux stdio-bridge`
    /// (`docs/spec/L3.md` §3.9); honored only from a same-uid Unix peer.
    pub const SSH_ORIGIN: u32 = 9;
    /// Client can demultiplex QUIC stream-per-Terminal; absent = false.
    pub const QUIC_STREAMS: u32 = 10;
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
    /// Selected frame compression (`u8` tag); absent or `0` = none.
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
    /// `role_policy: u8` (ADR-0127); absent = `{ PRIMARY, NEVER }`.
    pub const ROLE_POLICY: u32 = 6;
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
    /// Compressed inner frame body (type byte + payload, no length prefix).
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

/// `DETACHED` body fields (`docs/spec/proto.md` §7.2); both optional, so an
/// empty body is valid.
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
    /// Optional `ActorRef` of the connection whose write caused the change
    /// (positional; ADR-0123, gated on `ServerFeature::EventJournal`).
    pub const ACTOR: u32 = 4;
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
    /// Optional satellite host to list on via the hub (`LIST_DIRECTORY_HOST`).
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

/// `PATH_QUERY` body fields (`docs/spec/L3.md` §5).
pub mod path_query {
    /// Correlation id (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// Starting directory (`str`).
    pub const ROOT: u32 = 2;
    /// Browse filter or fuzzy term (`str`).
    pub const QUERY: u32 = 3;
    /// `u8`: 0 = one level, 1 = recursive search.
    pub const RECURSIVE: u32 = 4;
    /// Optional satellite host (`str`).
    pub const HOST: u32 = 5;
}

/// `PATH_RESULTS` body fields (`docs/spec/L3.md` §5).
pub mod path_results {
    /// Correlation id (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// Absolute lexical root (`str`), or attempted root on refusal.
    pub const ROOT: u32 = 2;
    /// Lexical parent (`str`), absent at filesystem root.
    pub const PARENT: u32 = 3;
    /// Positional `u32` count then `(path: str, kind: u8)` rows.
    pub const ROWS: u32 = 4;
    /// `PathStatus` (`u8`), required on success.
    pub const STATUS: u32 = 5;
    /// `PathErrorCode` (`u8`), present on refusal only.
    pub const ERROR: u32 = 6;
    /// Human-readable diagnostic, present on refusal only.
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
    /// Optional satellite host to spawn on (`docs/spec/L1.md` §3.1, ADR-0007).
    pub const SATELLITE: u32 = 7;
    /// Optional Terminal whose owning window hosts the spawn.
    pub const OWNER_TERMINAL: u32 = 8;
    /// Optional opaque `phux.agent-session/v1` record installed atomically.
    pub const AGENT_SESSION: u32 = 9;
    /// Optional initial grid, `cols: u16` then `rows: u16`; absent or zero
    /// keeps the server default.
    pub const INITIAL_SIZE: u32 = 10;
    /// Optional `ResourceKind` tag (`u8`); absent = `Terminal`.
    ///
    /// `AgentSession` requires `PARENT` and `PROVIDER` and forbids fields 3-6,
    /// 8, and 10; `Terminal` forbids fields 12-14.
    pub const KIND: u32 = 11;
    /// Optional parent resource (positional `ResourceId`).
    pub const PARENT: u32 = 12;
    /// Optional agent provider name (`str`).
    pub const PROVIDER: u32 = 13;
    /// Optional opaque provider-native session id (`str`).
    pub const NATIVE_ID: u32 = 14;
    /// Optional `u8` flag: answer with the instance token (ADR-0109).
    pub const BIND_INSTANCE: u32 = 15;
    /// Optional `u32` post-exit retention seconds, `0` = server default
    /// (ADR-0124). Terminal only.
    pub const RETAIN_SECS: u32 = 16;
    /// Optional 16-byte non-zero idempotency key (ADR-0126).
    pub const IDEMPOTENCY_KEY: u32 = 17;
}

/// `RESOURCE_SPAWNED` body fields (`docs/spec/L1.md` §10.1).
pub mod terminal_spawned {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// `SpawnResult` tagged union (positional).
    pub const RESULT: u32 = 2;
    /// Optional 16-byte instance token, only when the spawn set
    /// `BIND_INSTANCE` (ADR-0109).
    pub const INSTANCE: u32 = 3;
    /// Optional `u8` replay flag, only beside `Ok` (ADR-0126).
    pub const REPLAYED: u32 = 4;
}

/// `MOVE_RESOURCE` body fields (`docs/spec/L1.md` §10.1; ADR-0056).
pub mod move_terminal {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// The Terminal to re-parent (positional tagged union).
    pub const TERMINAL: u32 = 2;
    /// Terminal whose owning window becomes the destination (positional).
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
    /// Optional `CloseReason` tag (`u8`); absent = `Unknown`.
    pub const REASON: u32 = 3;
    /// Optional terminating signal (`i32` as two's-complement `u32`).
    pub const SIGNAL: u32 = 4;
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
    /// Optional `u64` journal cursor to replay after (ADR-0123).
    pub const AFTER_SEQ: u32 = 2;
}

/// `EVENT` body fields (`docs/spec/L1.md` §7.5); 3-6 are the journal stamp
/// (ADR-0123).
pub mod event {
    /// Optional `ResourceId` scope (absent field = server-scoped `None`).
    pub const TERMINAL: u32 = 1;
    /// `AgentEvent` tagged union (positional TLV: tag + length-prefixed body).
    pub const EVENT: u32 = 2;
    /// Server-wide journal sequence (`u64`, starts at 1, never wraps).
    pub const SEQ: u32 = 3;
    /// Journal wall-clock time, Unix milliseconds (`u64`).
    pub const TS_MS: u32 = 4;
    /// `ActorRef` of the causing connection (positional).
    pub const ACTOR: u32 = 5;
    /// Idempotency key of the causing operation (16 bytes).
    pub const OPERATION_ID: u32 = 6;
}

/// `AgentEvent::JournalGap` body fields (`docs/spec/L1.md` §7.1).
pub mod event_journal_gap {
    /// First missing journal sequence (`u64`, inclusive).
    pub const FIRST_MISSING: u32 = 1;
    /// Last missing journal sequence (`u64`, inclusive).
    pub const LAST_MISSING: u32 = 2;
}

/// `AgentEvent::SourceGap` body fields (`docs/spec/L1.md` §7.1).
pub mod event_source_gap {
    /// Events dropped before journaling (`u64`).
    pub const DROPPED: u32 = 1;
}

/// Fields of the field-tagged `SessionSnapshot` extension block
/// (`docs/spec/L1.md` §9.1), the fifth trailing element.
pub mod snapshot_extension {
    /// One resource with non-default state: positional `ResourceId`, then
    /// [`resource_state`](super::resource_state) fields. Repeated.
    pub const RESOURCE_STATE: u32 = 1;
    /// Newest event-journal `seq` at the cut (`u64`, `docs/spec/L1.md` §7.3).
    pub const JOURNAL_HEAD: u32 = 2;
}

/// Fields of one `RESOURCE_STATE` value, after its positional `ResourceId`.
pub mod resource_state {
    /// `ResourceLifecycle` (`u8`); absent = `RUNNING`.
    pub const LIFECYCLE: u32 = 1;
    /// `ExitFacet` (positional); present iff exited and retained (ADR-0124).
    pub const EXIT: u32 = 2;
    /// `ClientId` (`u32`) of the input-lease holder; absent = open.
    pub const INPUT_HOLDER: u32 = 3;
    /// `ClientId` (`u32`) of one `VIEWER` (ADR-0127); repeated.
    pub const VIEWER: u32 = 4;
}

/// `AgentEvent::Asked` body fields (`docs/spec/L1.md` §7.5); field-tagged,
/// unlike the positional `AgentEvent` bodies, so fields stay additive.
pub mod event_asked {
    /// Stable question id (`str`).
    pub const ID: u32 = 1;
    /// Question text (`str`).
    pub const QUESTION: u32 = 2;
    /// One suggested answer (`str`), repeated in order.
    pub const SUGGESTION: u32 = 3;
    /// Optional seconds waiting (`u64`); absent = unknown.
    pub const ELAPSED_SECONDS: u32 = 4;
}

/// `AgentEvent::ResourceSpawned` body fields (`docs/spec/L1.md` §7.1);
/// field-tagged, and an empty body is a root Terminal.
pub mod event_pane_spawned {
    /// `ResourceKind` tag (`u8`); absent = `Terminal`.
    pub const KIND: u32 = 1;
    /// Parent resource (positional tagged `ResourceId`). Absent = a root.
    pub const PARENT: u32 = 2;
}
