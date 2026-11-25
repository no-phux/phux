//! Handshake, attach, input, stream, and teardown frame bodies.

use crate::ids::{BootstrapId, ClientId, ResourceId, StreamId};
use crate::input::focus::FocusEvent;
use crate::input::key::KeyEvent;
use crate::input::mouse::MouseEvent;
use crate::input::paste::{ClipboardData, PasteEvent};
use crate::wire::frame::{AttachTarget, ErrorCode, RolePolicy, ViewportInfo};
use crate::wire::info::SessionSnapshot;

tlv_body! {
    /// `HELLO` body fields (`docs/protocol.md#handshake`).
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
        /// Top-level because the `CLIENT_CAPS` byte order is fixed.
        pub const COMPRESSION: u32 = 6;
        // Ids 7 and 8 are retired-unshipped (ADR-0116) and stay reserved.
        /// Positional `SshOrigin` stamped by `phux stdio-bridge`
        /// (`docs/reference/cli.md#phux-whoami`); honored only from a same-uid Unix peer.
        pub const SSH_ORIGIN: u32 = 9;
        /// Client can demultiplex QUIC stream-per-Terminal; absent = false.
        pub const QUIC_STREAMS: u32 = 10;
        /// Resource kinds the client decodes (`u64`, bit `1 << kind tag`);
        /// absent = `ResourceKindSet::LEGACY`.
        pub const RESOURCE_KINDS: u32 = 11;
        /// The device the client says it runs on: 1 to 16 bytes of `[a-z0-9-]`
        /// (`ClientPlatform`); absent or anything else = unreported.
        pub const PLATFORM: u32 = 12;
        /// Client reviews credential request details (`u8` boolean); absent = false.
        pub const CREDENTIAL_APPROVALS: u32 = 13;
        /// Client prompts for and answers `open_url` events (`u8` boolean);
        /// absent = false.
        pub const OPEN_URL: u32 = 14;
    }
}

tlv_body! {
    /// `HELLO_OK` body fields (`docs/protocol.md#handshake`).
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
}

tlv_body! {
    /// `PING` / `PONG` body fields (`docs/protocol.md#handshake`).
    pub mod ping {
        /// Nonce the peer echoes back (`u64`). Shared id for `PING` and `PONG`.
        pub const NONCE: u32 = 1;
    }
}

tlv_body! {
    /// `RESOURCE_OUTPUT` body fields (`docs/protocol.md#attach-and-output`, ADR-0013).
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
}

tlv_body! {
    /// `ATTACH` body fields (`docs/protocol.md`).
    pub mod attach for Attach => decode_attach {
        /// `AttachTarget` tagged union (positional).
        pub const TARGET: u32 = 1 => required target: AttachTarget;
        /// `ViewportInfo` (positional sub-record).
        pub const VIEWPORT: u32 = 2 => required viewport: ViewportInfo;
        /// `request_scrollback: bool`.
        pub const REQUEST_SCROLLBACK: u32 = 3 => default request_scrollback: bool;
        /// `scrollback_limit_lines: u32`.
        pub const SCROLLBACK_LIMIT_LINES: u32 = 4 => default scrollback_limit_lines: u32;
        /// Client-chosen attach correlation id (`u32`).
        pub const ATTACH_ID: u32 = 5 => required attach_id: u32;
        /// `role_policy: u8` (ADR-0127); absent = `{ PRIMARY, NEVER }`.
        pub const ROLE_POLICY: u32 = 6 => optional role_policy: RolePolicy;
    }
}

tlv_body! {
    /// `INPUT_KEY` body fields (`docs/protocol.md#input`).
    pub mod input_key for InputKey => decode_input_key {
        /// Target `ResourceId` (positional tagged union).
        pub const TERMINAL_ID: u32 = 1 => required terminal_id: ResourceId;
        /// `KeyEvent` (positional sub-record).
        pub const EVENT: u32 = 2 => required event: KeyEvent;
    }
}

tlv_body! {
    /// `INPUT_MOUSE` body fields (`docs/protocol.md#input`).
    pub mod input_mouse for InputMouse => decode_input_mouse {
        /// Target `ResourceId` (positional tagged union).
        pub const TERMINAL_ID: u32 = 1 => required terminal_id: ResourceId;
        /// `MouseEvent` (positional sub-record).
        pub const EVENT: u32 = 2 => required event: MouseEvent;
    }
}

tlv_body! {
    /// `INPUT_FOCUS` body fields (`docs/protocol.md#input`).
    pub mod input_focus for InputFocus => decode_input_focus {
        /// Target `ResourceId` (positional tagged union).
        pub const TERMINAL_ID: u32 = 1 => required terminal_id: ResourceId;
        /// Focus kind (`u8`: gained=0 / lost=1).
        pub const EVENT: u32 = 2 => required event: FocusEvent;
    }
}

tlv_body! {
    /// `INPUT_PASTE` body fields (`docs/protocol.md#input`).
    pub mod input_paste for InputPaste => decode_input_paste {
        /// Target `ResourceId` (positional tagged union).
        pub const TERMINAL_ID: u32 = 1 => required terminal_id: ResourceId;
        /// `PasteEvent` (positional sub-record: trust byte + bytes).
        pub const EVENT: u32 = 2 => required event: PasteEvent;
    }
}

tlv_body! {
    /// `INPUT_TERMINAL_REPLY` body fields (`docs/protocol.md#terminal-queries`).
    pub mod input_terminal_reply {
        /// Attached target `ResourceId` (positional tagged union).
        pub const TERMINAL_ID: u32 = 1;
        /// Opaque terminal-emulator-generated PTY reply bytes.
        pub const BYTES: u32 = 2;
    }
}

tlv_body! {
    /// `CLIPBOARD_REPLY` body fields (`docs/program-sequences.md#clipboard-osc-52`).
    pub mod clipboard_reply for ClipboardReply => decode_clipboard_reply {
        /// Attached target `ResourceId` (positional tagged union).
        pub const TERMINAL_ID: u32 = 1 => required terminal_id: ResourceId;
        /// OSC 52 selection byte (`u8`): `c` or `p`.
        pub const SELECTION: u32 = 2 => required selection: u8;
        /// UTF-8 clipboard text, at most `MAX_CLIPBOARD_TEXT` bytes.
        pub const TEXT: u32 = 3 => required text: ClipboardData;
    }
}

tlv_body! {
    /// `FRAME_ACK` body fields (`docs/protocol.md#attach-and-output`).
    pub mod frame_ack for FrameAck => decode_frame_ack {
        /// Acked `ResourceId` (positional tagged union).
        pub const TERMINAL_ID: u32 = 1 => required terminal_id: ResourceId;
        /// Acked sequence id (`u64`).
        pub const SEQ: u32 = 2 => required seq: u64;
        /// Logical `StreamId` (`u64`, non-zero).
        pub const STREAM_ID: u32 = 3 => required stream_id: StreamId;
        /// Replica `BootstrapId` (`u64`, non-zero).
        pub const BOOTSTRAP_ID: u32 = 4 => required bootstrap_id: BootstrapId;
    }
}

tlv_body! {
    /// `VIEWPORT_RESIZE` body fields (`docs/protocol.md`).
    pub mod viewport_resize for ViewportResize => decode_viewport_resize {
        /// New `ViewportInfo` (positional sub-record).
        pub const VIEWPORT: u32 = 1 => required viewport: ViewportInfo;
    }
}

tlv_body! {
    /// `VIEWPORT_COLORS` body fields (`docs/protocol.md`).
    pub mod viewport_colors for ViewportColors => decode_viewport_colors {
        /// Foreground then background, three RGB bytes each.
        pub const COLORS: u32 = 1 => required colors: crate::caps::TerminalDefaultColors;
    }
}

tlv_body! {
    /// `ATTACHED` body fields (`docs/protocol.md#attach-and-output`).
    pub mod attached for Attached => decode_attached {
        /// Full `SessionSnapshot` (positional sub-record).
        pub const SNAPSHOT: u32 = 1 => required snapshot: SessionSnapshot;
        /// Server-allocated `ClientId` for this attachment (`u32`).
        pub const INITIAL_CLIENT_ID: u32 = 2 => required initial_client_id: ClientId;
        /// Client-chosen attach correlation id (`u32`).
        pub const ATTACH_ID: u32 = 3 => required attach_id: u32;
    }
}

tlv_body! {
    /// `ATTACH_READY` body fields (`docs/protocol.md#attach-and-output`).
    pub mod attach_ready for AttachReady => decode_attach_ready {
        /// Client-chosen attach correlation id (`u32`).
        pub const ATTACH_ID: u32 = 1 => required attach_id: u32;
    }
}

tlv_body! {
    /// `HISTORY_REQUEST` body fields (`docs/protocol.md#generations-publication-and-history`).
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
}

tlv_body! {
    /// `BOOTSTRAP_BEGIN` body fields (`docs/protocol.md#attach-and-output`).
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
}

tlv_body! {
    /// `BOOTSTRAP_CHUNK` body fields (`docs/protocol.md#attach-and-output`).
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
}

tlv_body! {
    /// `FRAME_COMPRESSED` body fields (`docs/protocol.md#framing`).
    pub mod frame_compressed {
        /// Compression algorithm tag (`u8`).
        pub const ALGORITHM: u32 = 1;
        /// Exact byte length of the inflated inner frame body (`u32`).
        pub const UNCOMPRESSED_LEN: u32 = 2;
        /// Compressed inner frame body (type byte + payload, no length prefix).
        pub const PAYLOAD: u32 = 3;
    }
}

tlv_body! {
    /// `BOOTSTRAP_READY` body fields (`docs/protocol.md#attach-and-output`).
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
}

tlv_body! {
    /// `HISTORY_PAGE` body fields (`docs/protocol.md#generations-publication-and-history`).
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
}

tlv_body! {
    /// `BOOTSTRAP_TOMBSTONE` body fields (`docs/protocol.md#generations-publication-and-history`).
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
}

tlv_body! {
    /// `HISTORY_TOMBSTONE` body fields (`docs/protocol.md#generations-publication-and-history`).
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
}

tlv_body! {
    /// `HISTORY_REJECTED` body fields (`docs/protocol.md#generations-publication-and-history`).
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
}

tlv_body! {
    /// `DETACHED` body fields (`docs/protocol.md#attach-and-output`); both optional, so an
    /// empty body is valid.
    pub mod detached {
        /// `DetachReason` tag (`u8`). Absent = the peer stated no reason.
        pub const REASON: u32 = 1;
        /// Human-readable UTF-8 detail. Absent = empty.
        pub const MESSAGE: u32 = 2;
    }
}

tlv_body! {
    /// `BELL` body fields (`docs/protocol.md#commands-events-resources`).
    pub mod bell for Bell => decode_bell {
        /// Terminal that received the bell character (positional tagged union).
        pub const TERMINAL_ID: u32 = 1 => required terminal_id: ResourceId;
    }
}

tlv_body! {
    /// `ERROR` body fields (`docs/protocol.md`).
    pub mod error for Error => decode_error {
        /// Optional correlating `request_id` (absent field = `None`).
        pub const REQUEST_ID: u32 = 1 => optional request_id: u32;
        /// Structured `ErrorCode` (`u16`).
        pub const CODE: u32 = 2 => required code: ErrorCode;
        /// Human-readable UTF-8 message.
        pub const MESSAGE: u32 = 3 => required message: String;
    }
}
