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
/// Creation placement (tagged record); requires `CREATE_PLACEMENT`.
pub const PLACEMENT: u32 = 43;
/// Stable entry created with a resource (`bytes16`).
pub const ENTRY: u32 = 44;
/// Whether an organization operation replayed its retained receipt (`bool`).
pub const REPLAYED: u32 = 45;
/// Durable authority of a creation receipt (`bytes16`).
pub const AUTHORITY: u32 = 46;
/// Bounded JSON launch diagnostic (stage and errno), optional.
pub const LAUNCH_DIAGNOSTIC: u32 = 47;
/// Executor evidence awaits persistence; last committed state is shown.
pub const PERSISTENCE_PENDING: u32 = 48;
