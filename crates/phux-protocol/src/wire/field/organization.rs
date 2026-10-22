//! Organization bodies and the field-tagged records nested in values.

/// Fields of every organization body (`docs/organization.md`).
///
/// One id space serves commands, `COMMITTED` / `ARRANGEMENT` values, session
/// and resource records, and organization events, so a field means the same
/// thing wherever it appears; each body names which ids it carries.
pub mod org {
    /// `OperationId` (16 non-zero bytes).
    pub const OPERATION: u32 = 1;
    /// `SessionKey` (16 bytes).
    pub const SESSION: u32 = 2;
    /// `ResourceKey` (16 bytes).
    pub const RESOURCE: u32 = 3;
    /// Expected `Revision` of the primary record (`u64`).
    pub const EXPECTED: u32 = 4;
    /// Name (`str`, at most 256 bytes).
    pub const NAME: u32 = 5;
    /// `SessionLifetime` (`u8`).
    pub const LIFETIME: u32 = 6;
    /// One descriptive attribute: positional `str key || str value`, each at
    /// most 256 bytes. Repeated, in order.
    pub const ATTR: u32 = 7;
    /// `LaunchSpec`: nested [`org_launch`](super::org_launch) fields.
    pub const LAUNCH: u32 = 8;
    /// `ResourceKind` tag (`u8`).
    pub const KIND: u32 = 9;
    /// `ExitPolicy` (`u8`).
    pub const ON_EXIT: u32 = 10;
    /// Source `SessionKey` of a membership move (16 bytes).
    pub const FROM: u32 = 11;
    /// Expected or resulting source-session `Revision` (`u64`).
    pub const FROM_REVISION: u32 = 12;
    /// Destination `SessionKey` of a membership move (16 bytes).
    pub const TO: u32 = 13;
    /// Expected or resulting destination-session `Revision` (`u64`).
    pub const TO_REVISION: u32 = 14;
    /// `LiveTarget`, positional: `ResourceId || instance: bytes16 ||
    /// activation: u64`.
    pub const TARGET: u32 = 15;
    /// `TerminateMode` (`u8`).
    pub const MODE: u32 = 16;
    /// `ActivationId` (`u64`).
    pub const ACTIVATION: u32 = 17;
    /// `ArrangementKey` (16 bytes).
    pub const ARRANGEMENT: u32 = 18;
    /// Arrangement document, positional, at most 64 KiB.
    pub const DOCUMENT: u32 = 19;
    /// `OrgFilter`, positional: `tag: u8`, then a 16-byte key for a session
    /// or workspace filter.
    pub const FILTER: u32 = 20;
    /// Requested page size (`u32`); `0` asks for the server default.
    pub const PAGE_SIZE: u32 = 21;
    /// Opaque page cursor (bytes, at most 4 KiB).
    pub const CURSOR: u32 = 22;
    /// The `OperationId` an undo reverses (16 non-zero bytes).
    pub const UNDO: u32 = 23;
    /// One bulk-move item, positional: `resource: bytes16 || from: bytes16 ||
    /// from_expected: u64`. Repeated.
    pub const MOVE_ITEM: u32 = 24;
    /// One record revision, positional: `key: bytes16 || revision: u64`.
    /// Repeated.
    pub const PIN: u32 = 25;
    /// Resulting `Revision` of the primary record (`u64`).
    pub const REVISION: u32 = 26;
    /// Live `SessionId` handle (`u32`).
    pub const LIVE_SESSION: u32 = 27;
    /// Live `ResourceId` handle (positional tagged `ResourceId`).
    pub const LIVE_RESOURCE: u32 = 28;
    /// `ExecState` (`u8`).
    pub const STATE: u32 = 29;
    /// `Outcome`, positional: `tag: u8`, then `i32` for `EXITED` and
    /// `SIGNALED`.
    pub const OUTCOME: u32 = 30;
    /// `LeaseEpoch` (`u64`).
    pub const EPOCH: u32 = 31;
    /// `ActorRef` of a lease holder (positional).
    pub const HOLDER: u32 = 32;
    /// Intake state (`u8`, `0` or `1`): whether the session awaits
    /// organization.
    pub const AWAITING: u32 = 33;
    /// `ServerInstance` (16 bytes).
    pub const INSTANCE: u32 = 34;
    /// Member count (`u32`).
    pub const MEMBERS: u32 = 35;
    /// `WorkspaceKey` (16 bytes).
    pub const WORKSPACE: u32 = 36;
    /// Requested delegated-credential lifetime in seconds (`u32`); `0` asks
    /// for the server default.
    pub const TTL_SECS: u32 = 37;
    /// Delegated credential id (`str`, at most 256 bytes).
    pub const CREDENTIAL: u32 = 38;
    /// Path of a delegated credential's socket (`str`, at most 4 KiB).
    pub const SOCKET_PATH: u32 = 39;
    /// When a delegated credential expires, Unix milliseconds (`u64`).
    pub const EXPIRES_AT: u32 = 40;
    /// `LaunchMode` (`u8`); absent = `IMMEDIATE`.
    pub const LAUNCH_MODE: u32 = 41;
    /// Session project link (`str`, at most 256 bytes); absent on
    /// `SESSION_SET_PROJECT` clears it.
    pub const PROJECT: u32 = 42;
}

/// Fields of a `LaunchSpec` value (`docs/organization.md`).
pub mod org_launch {
    /// One argv element (`str`). Repeated, in order; absent = the default
    /// shell.
    pub const COMMAND: u32 = 1;
    /// Working directory (`str`).
    pub const CWD: u32 = 2;
    /// One environment pair, positional `str name || str value`. Repeated.
    pub const ENV: u32 = 3;
    /// `TERM` override (`str`).
    pub const TERM: u32 = 4;
    /// Kind-specific restore record (bytes), e.g. the agent conversation a
    /// relaunched Terminal resumes.
    pub const RESTORE: u32 = 5;
}

/// Fields of an `ORGANIZATION_PAGE` value (`docs/organization.md`).
pub mod org_page {
    /// `AuthorityId` (16 bytes).
    pub const AUTHORITY: u32 = 1;
    /// The designated Intake `SessionKey` (16 bytes); legacy name, unchanged tag.
    pub const INBOX: u32 = 2;
    /// One session record: nested [`org`](super::org) fields. Repeated.
    pub const SESSION: u32 = 3;
    /// One resource record: nested [`org`](super::org) fields. Repeated.
    pub const RESOURCE: u32 = 4;
    /// Sessions matching the filter, all pages (`u32`).
    pub const TOTAL_SESSIONS: u32 = 5;
    /// Resources matching the filter, all pages (`u32`).
    pub const TOTAL_RESOURCES: u32 = 6;
    /// Sessions awaiting organization among those matching (`u32`).
    pub const TOTAL_INTAKE: u32 = 7;
    /// Organization revision the page was cut at (`u64`).
    pub const SNAPSHOT_REVISION: u32 = 8;
    /// Event-journal `seq` at the cut (`u64`): subscribe after it.
    pub const JOURNAL_SEQ: u32 = 9;
    /// Opaque cursor of the next page (bytes, at most 4 KiB); absent on the
    /// last page.
    pub const NEXT_CURSOR: u32 = 10;
}

/// `AgentEvent::Asked` body fields (`docs/protocol.md#commands-events-resources`); field-tagged,
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

/// `AgentEvent::ResourceSpawned` body fields (`docs/protocol.md#commands-events-resources`);
/// field-tagged, and an empty body is a root Terminal.
pub mod event_pane_spawned {
    /// `ResourceKind` tag (`u8`); absent = `Terminal`.
    pub const KIND: u32 = 1;
    /// Parent resource (positional tagged `ResourceId`). Absent = a root.
    pub const PARENT: u32 = 2;
}
