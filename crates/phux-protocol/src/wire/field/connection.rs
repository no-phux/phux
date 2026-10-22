//! Handshake, attach, input, stream, and teardown frame bodies.

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
    /// Last agent record committed by the consumer (`u64`); agent/raw only.
    pub const RESUME_AFTER_SEQ: u32 = 9;
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
