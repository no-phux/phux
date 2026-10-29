//! L1 resource lifecycle, command, and event frame bodies.

/// `SPAWN_RESOURCE` body fields (`docs/protocol.md#commands-events-resources`).
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
    /// Optional satellite host to spawn on (`docs/protocol.md`, ADR-0007).
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

/// `RESOURCE_SPAWNED` body fields (`docs/protocol.md#commands-events-resources`).
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

/// `MOVE_RESOURCE` body fields (`docs/protocol.md#commands-events-resources`; ADR-0056).
pub mod move_terminal {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// The Terminal to re-parent (positional tagged union).
    pub const TERMINAL: u32 = 2;
    /// Terminal whose owning window becomes the destination (positional).
    pub const OWNER_TERMINAL: u32 = 3;
}

/// `RESOURCE_MOVED` body fields (`docs/protocol.md#commands-events-resources`; ADR-0056).
pub mod terminal_moved {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// `MoveResult` tagged union (positional).
    pub const RESULT: u32 = 2;
}

/// `RESOURCE_CLOSED` body fields (`docs/protocol.md#commands-events-resources`).
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

/// `RESIZE_TERMINAL` body fields (`docs/protocol.md#commands-events-resources`).
pub mod terminal_resize {
    /// Target `ResourceId` (positional tagged union).
    pub const TERMINAL_ID: u32 = 1;
    /// New column count (`u16`).
    pub const COLS: u32 = 2;
    /// New row count (`u16`).
    pub const ROWS: u32 = 3;
    /// Optional cell width in pixels (`u16`), sent together with
    /// [`CELL_HEIGHT_PX`]; absent keeps the Terminal's last cell size
    /// (ADR-0145).
    pub const CELL_WIDTH_PX: u32 = 4;
    /// Optional cell height in pixels (`u16`); see [`CELL_WIDTH_PX`].
    pub const CELL_HEIGHT_PX: u32 = 5;
}

/// `COMMAND` body fields (`docs/protocol.md#commands-events-resources`).
pub mod command {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// `Command` tagged union (positional).
    pub const COMMAND: u32 = 2;
}

/// `COMMAND_RESULT` body fields (`docs/protocol.md#commands-events-resources`).
pub mod command_result {
    /// Correlating `request_id` (`u32`).
    pub const REQUEST_ID: u32 = 1;
    /// `CommandResult` tagged union (positional).
    pub const RESULT: u32 = 2;
}

/// `SUBSCRIBE_EVENTS` body fields (`docs/protocol.md#commands-events-resources`).
pub mod subscribe_events {
    /// Optional `ResourceId` scope (absent field = server-scoped `None`).
    pub const TERMINAL: u32 = 1;
    /// Optional `u64` journal cursor to replay after (ADR-0123).
    pub const AFTER_SEQ: u32 = 2;
}

/// `EVENT` body fields (`docs/protocol.md#commands-events-resources`); 3-6 are the journal stamp
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

/// `AgentEvent::JournalGap` body fields (`docs/protocol.md#commands-events-resources`).
pub mod event_journal_gap {
    /// First missing journal sequence (`u64`, inclusive).
    pub const FIRST_MISSING: u32 = 1;
    /// Last missing journal sequence (`u64`, inclusive).
    pub const LAST_MISSING: u32 = 2;
}

/// `AgentEvent::SourceGap` body fields (`docs/protocol.md#commands-events-resources`).
pub mod event_source_gap {
    /// Events dropped before journaling (`u64`).
    pub const DROPPED: u32 = 1;
}

/// Fields of the field-tagged `SessionSnapshot` extension block
/// (`docs/protocol.md`), the fifth trailing element.
pub mod snapshot_extension {
    /// One resource with non-default state: positional `ResourceId`, then
    /// [`resource_state`](super::resource_state) fields. Repeated.
    pub const RESOURCE_STATE: u32 = 1;
    /// Newest event-journal `seq` at the cut (`u64`, `docs/protocol.md`).
    pub const JOURNAL_HEAD: u32 = 2;
    /// One resource's session: positional `ResourceId`, then the owning
    /// `SessionId` (`u32`). Repeated; absent = no session reported.
    pub const RESOURCE_SESSION: u32 = 3;
    /// One session's organization state: `SessionId` (`u32`), then
    /// [`session_state`](super::session_state) fields. Repeated.
    pub const SESSION_STATE: u32 = 4;
}

/// Fields of one `SESSION_STATE` value, after its positional `SessionId`.
pub mod session_state {
    /// `SessionLifetime` (`u8`); absent = not reported.
    pub const LIFETIME: u32 = 1;
    /// `SessionKey` (16 bytes); absent = not reported.
    pub const KEY: u32 = 2;
    /// One descriptive attribute, `str key || str value`; repeated.
    pub const ATTR: u32 = 3;
    /// The session's most recent input target (`ResourceId`, R14); absent =
    /// none reported.
    pub const RECENT_INPUT: u32 = 4;
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
