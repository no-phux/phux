//! The handshake, attach, input, bootstrap, and history [`FrameKind`]
//! variants (`docs/protocol.md#handshake`, `docs/protocol.md#attach-and-output`).

#[cfg(doc)]
use super::FrameKind;

/// Appends the handshake, attach, input, and stream [`FrameKind`] variants to the list being
/// built and hands it to `$next`; see `frame_kind_enum!` in `kind.rs`.
macro_rules! connection_frames {
    ($next:ident!($($rest:tt)*) $($done:tt)*) => {
        $next!($($rest)* $($done)*
    /// `HELLO` — client to server handshake (`docs/protocol.md#handshake`). A
    /// missing or truncated capability record is malformed.
    Hello {
        /// Free-form client identifier (e.g. `"phux-client 0.1.0"`).
        client_name: String,
        /// Highest protocol major version the client supports.
        protocol_major: u16,
        /// Highest protocol minor version the client supports.
        protocol_minor: u16,
        /// Highest protocol patch version the client supports.
        protocol_patch: u16,
        /// Client capability advertisement (SPEC §6.2).
        client_caps: ClientCapabilities,
    },
    /// `HELLO_OK` — server handshake acknowledgement (`docs/protocol.md#handshake`).
    /// Selects one [`BootstrapProfile`] for the connection's lifetime.
    HelloOk {
        /// Selected major version (wire-breaking axis pre-1.0).
        protocol_major: u16,
        /// Selected minor version.
        protocol_minor: u16,
        /// Selected patch version.
        protocol_patch: u16,
        /// The conformance tiers the server mounts; intersect with the
        /// client's `layers` for the negotiated tier set.
        server_caps: ServerCapabilities,
        /// Opaque server identity bytes (SPEC §6.1).
        server_id: Vec<u8>,
        /// Explicit synchronization profile selected for this connection.
        selected_profile: BootstrapProfile,
        /// Negotiated bootstrap/history payload bounds.
        bootstrap_limits: BootstrapLimits,
    },

    /// `PING` — liveness probe (`docs/protocol.md#handshake`), echoed in `PONG`.
    Ping {
        /// Opaque nonce echoed by the peer in `PONG`.
        nonce: u64,
    },
    /// `PONG` — liveness response (`docs/protocol.md#handshake`).
    Pong {
        /// Nonce echoed from the corresponding `PING`.
        nonce: u64,
    },

    /// `RESOURCE_OUTPUT` — live terminal content (`docs/protocol.md#attach-and-output`).
    ///
    /// `seq` is contiguous and non-wrapping within one `(stream_id,
    /// bootstrap_id)`. Under `NativeState`, `bytes` are the exact PTY bytes
    /// and MUST NOT be rewritten.
    ResourceOutput {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription receiving this output.
        stream_id: StreamId,
        /// Published replica generation this output extends.
        bootstrap_id: BootstrapId,
        /// Monotonic stream sequence.
        seq: u64,
        /// Opaque VT bytes.
        bytes: bytes::Bytes,
    },

    /// `ATTACH` — client requests to attach to a session (`docs/protocol.md`).
    Attach {
        /// Client-chosen correlation id echoed by `ATTACHED` and `ATTACH_READY`.
        attach_id: u32,
        /// Which session to attach to.
        target: AttachTarget,
        /// Client viewport dimensions at attach time.
        viewport: ViewportInfo,
        /// Whether to send scrollback as part of the attach sequence.
        request_scrollback: bool,
        /// Upper bound on scrollback lines the client will accept.
        scrollback_limit_lines: u32,
        /// Attach intent for every Terminal the attach subscribes (field 6,
        /// ADR-0127). `None` writes no field and means `{ PRIMARY, NEVER }`.
        /// Send `Some` only to a server advertising `ATTACH_ROLES`.
        role_policy: Option<super::RolePolicy>,
    },

    /// `DETACH` — client signals clean departure (`docs/protocol.md#attach-and-output`).
    Detach,

    /// `INPUT_KEY` — client forwards a structured key event (`docs/protocol.md#input`).
    InputKey {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Structured key event; libghostty atoms inside.
        event: KeyEvent,
    },

    /// `INPUT_MOUSE` — client forwards a mouse event (`docs/protocol.md#input`).
    InputMouse {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Structured mouse event; coordinates are terminal-local pixels.
        event: MouseEvent,
    },

    /// `INPUT_FOCUS` — client reports focus change on its host window
    /// (`docs/protocol.md#input`).
    InputFocus {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Whether the client window gained or lost focus.
        event: FocusEvent,
    },

    /// `INPUT_PASTE` — client forwards a paste payload (`docs/protocol.md#input`).
    InputPaste {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Paste payload plus trust classification.
        event: PasteEvent,
    },

    /// `INPUT_TERMINAL_REPLY` — opaque bytes the client's terminal emulator
    /// generated in response to output (`docs/protocol.md#terminal-queries`), written
    /// byte-identically to the terminal's input lane.
    InputTerminalReply {
        /// Attached target terminal.
        terminal_id: ResourceId,
        /// Non-empty opaque PTY reply bytes. NUL and non-UTF-8 are valid.
        bytes: bytes::Bytes,
    },

    /// `FRAME_ACK` — cumulative acknowledgement (`docs/protocol.md`),
    /// valid only for the `SynthesizedVtStateSync` profile.
    FrameAck {
        /// Acked terminal.
        terminal_id: ResourceId,
        /// Logical subscription whose `StateSync` reference advances.
        stream_id: StreamId,
        /// Replica generation whose reference advances.
        bootstrap_id: BootstrapId,
        /// Highest contiguous `RESOURCE_OUTPUT.seq` applied.
        seq: u64,
    },

    /// `VIEWPORT_RESIZE` — the attached client's outer terminal changed
    /// size (`docs/protocol.md`). The connection identifies
    /// the client.
    ViewportResize {
        /// New outer-terminal metrics.
        viewport: ViewportInfo,
    },

    /// `ATTACHED` — metadata inventory for an accepted attach.
    ///
    /// Terminal content follows separately through bootstrap streams. This
    /// frame does not mean those streams are renderable; `ATTACH_READY` marks
    /// the aggregate boundary after every pane is READY or closed.
    Attached {
        /// Client-chosen correlation id from `ATTACH`.
        attach_id: u32,
        /// Full graph of sessions/windows/panes plus initial focus.
        snapshot: SessionSnapshot,
        /// Server-allocated client identifier for this attachment.
        initial_client_id: ClientId,
    },
    /// `ATTACH_READY` — every stream created by one `ATTACH` is READY or closed.
    AttachReady {
        /// Client-chosen correlation id from `ATTACH`.
        attach_id: u32,
    },

    /// `DETACHED` — server confirms detach and closes the transport
    /// (`docs/protocol.md#attach-and-output`). With that close, the only ending a
    /// consumer may act on; an `ERROR` never ends an attach (§9).
    Detached {
        /// Why the attach ended, or `None` when unstated or unrecognised.
        /// A consumer MUST NOT infer [`DetachReason::Requested`] from absence.
        reason: Option<DetachReason>,
        /// Diagnostic text, not a contract; empty when none was sent.
        message: String,
    },

    /// `BOOTSTRAP_BEGIN` — declares one replica generation. Agent streams
    /// either replace the retained log or append a cursor-anchored suffix.
    BootstrapBegin {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// New generation for this stream.
        bootstrap_id: BootstrapId,
        /// Concrete stream profile: codec, output mode, and optional agent
        /// resume anchor. Native `StateSync` is not permitted.
        profile: BootstrapStreamProfile,
        /// Authoritative PTY width at the actor cut.
        cols: u16,
        /// Authoritative PTY height at the actor cut.
        rows: u16,
        /// Actor cut sequence; first live output is `base_seq + 1`.
        base_seq: u64,
    },
    /// `BOOTSTRAP_CHUNK` — one bounded opaque checkpoint fragment.
    BootstrapChunk {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation.
        bootstrap_id: BootstrapId,
        /// Zero-based contiguous chunk sequence.
        chunk_seq: u32,
        /// Opaque engine/compatibility bytes.
        payload: bytes::Bytes,
    },
    /// `BOOTSTRAP_READY` — prior chunks reach the selected codec's READY boundary.
    BootstrapReady {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation now safe to publish.
        bootstrap_id: BootstrapId,
        /// Opaque newest-to-oldest history cursor, if retained history exists.
        history_cursor: Option<bytes::Bytes>,
    },
    /// `HISTORY_REQUEST` — request the next bounded history suffix page.
    HistoryRequest {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation that issued the cursor.
        bootstrap_id: BootstrapId,
        /// Opaque cursor returned by READY or a previous page.
        cursor: bytes::Bytes,
        /// Requested response byte budget; zero receives `HISTORY_REJECTED`.
        max_bytes: u32,
        /// Requested row budget; zero receives `HISTORY_REJECTED`.
        max_rows: u32,
    },
    /// `HISTORY_PAGE` — one independently decodable opaque history page.
    HistoryPage {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation that issued the cursor.
        bootstrap_id: BootstrapId,
        /// Non-zero page sequence within this generation-bound cursor lineage.
        page_seq: u64,
        /// Opaque cursor consumed by this response.
        cursor: bytes::Bytes,
        /// Cursor for the next older page; absence means this payload ends in FINISH.
        next_cursor: Option<bytes::Bytes>,
        /// Opaque selected-codec page bytes.
        payload: bytes::Bytes,
        /// Number of history rows encoded by this payload.
        rows: u32,
    },
    /// `BOOTSTRAP_TOMBSTONE` — permanently invalidates one generation.
    BootstrapTombstone {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Invalidated generation.
        bootstrap_id: BootstrapId,
        /// Why continuity could not be preserved.
        reason: TombstoneReason,
        /// Highest live sequence known valid for this generation.
        last_valid_seq: u64,
    },
    /// `HISTORY_TOMBSTONE` — invalidates one progressive history cursor only.
    HistoryTombstone {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation that issued the cursor.
        bootstrap_id: BootstrapId,
        /// Opaque invalidated history cursor.
        cursor: bytes::Bytes,
        /// Why progressive history for the cursor ended.
        reason: HistoryTombstoneReason,
    },
    /// `HISTORY_REJECTED` — retryable refusal that preserves cursor continuity.
    HistoryRejected {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation that issued the cursor.
        bootstrap_id: BootstrapId,
        /// Opaque history cursor that was not advanced.
        cursor: bytes::Bytes,
        /// Why the request did not begin.
        reason: HistoryRejectionReason,
        /// Non-zero byte limit required for a retry.
        required_bytes: u32,
        /// Non-zero row limit required for a retry.
        required_rows: u32,
    },

    /// `BELL` — terminal received a bell character (`docs/protocol.md#commands-events-resources`).
    Bell {
        /// Terminal that bell'd.
        terminal_id: ResourceId,
    },

    /// `ERROR` — structured error (`docs/protocol.md`). A fatal error
    /// MUST be followed by `DETACHED { PROTOCOL_ERROR }` and transport close.
    Error {
        /// The `COMMAND` this error answers, or `None` when uncorrelated.
        request_id: Option<u32>,
        /// Structured error code.
        code: ErrorCode,
        /// Short human-readable message.
        message: String,
    },
        );
    };
}
