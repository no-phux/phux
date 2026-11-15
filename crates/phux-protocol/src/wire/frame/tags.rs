//! Message, spawn/move result, scope, and event wire tags.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`frame` re-exports these crate-internal tags"
)]

use super::tag_registry;
#[cfg(doc)]
use super::{MoveResult, SpawnResult};

wire_tags! { Message;
/// Discriminant for `METADATA_VALUE` (server to client, `docs/protocol.md`):
/// the `GET_METADATA` reply, `None` when the key is absent.
pub const TYPE_METADATA_VALUE: u8 = 0xD1;

/// Discriminant for `METADATA_KEYS` (server to client, `docs/protocol.md`):
/// the `LIST_METADATA` reply, sorted key names only.
pub const TYPE_METADATA_KEYS: u8 = 0xD2;

/// Discriminant for `LIST_DIRECTORY` (client to server, `docs/protocol.md`);
/// gated on [`ServerFeature::ListDirectory`](crate::caps::ServerFeature::ListDirectory).
pub const TYPE_LIST_DIRECTORY: u8 = 0x55;

/// Discriminant for `DIRECTORY_LISTING` (server to client, `docs/protocol.md`):
/// the listing or a typed refusal.
pub const TYPE_DIRECTORY_LISTING: u8 = 0xD3;

/// `PATH_QUERY` (client to server, L3 §5).
pub const TYPE_PATH_QUERY: u8 = 0x56;
/// `PATH_RESULTS` (server to client, L3 §5).
pub const TYPE_PATH_RESULTS: u8 = 0xD4;
}

// L1 resource lifecycle discriminants (SPEC §7.2 / §10.1).

wire_tags! { Message;
/// Discriminant for `SPAWN_RESOURCE` (client to server, `docs/protocol.md`);
/// answered by [`TYPE_RESOURCE_SPAWNED`].
pub const TYPE_SPAWN_RESOURCE: u8 = 0x22;
/// Discriminant for `RESIZE_TERMINAL` (client to server, `docs/protocol.md`):
/// a per-Terminal PTY resize.
pub const TYPE_RESIZE_TERMINAL: u8 = 0x23;

// `0x24..=0x29` and `0xA3..=0xA7` stay unallocated: non-Terminal things are a
// `ResourceKind`, not a parallel frame family.

/// Discriminant for `MOVE_RESOURCE` (client to server, `docs/protocol.md`,
/// ADR-0056): re-parent a live Terminal into the window owning
/// `owner_terminal`, leaving its process and state untouched. Gated on
/// `MOVE_RESOURCE`; answered by [`TYPE_RESOURCE_MOVED`].
pub const TYPE_MOVE_RESOURCE: u8 = 0x2A;
/// Discriminant for `RESOURCE_MOVED` (server to client): the
/// `MOVE_RESOURCE` reply carrying a [`MoveResult`].
pub const TYPE_RESOURCE_MOVED: u8 = 0xA8;

/// Discriminant for `RESOURCE_CLOSED` (server to client, `docs/protocol.md`):
/// a resource ended.
pub const TYPE_RESOURCE_CLOSED: u8 = 0xA1;
/// Discriminant for `RESOURCE_SPAWNED` (server to client): the
/// `SPAWN_RESOURCE` reply carrying a [`SpawnResult`].
pub const TYPE_RESOURCE_SPAWNED: u8 = 0xA2;
}

// `Result`-shaped unions use `Ok = 0`, `Err = 1`, mirroring `Option`'s
// `None = 0`, `Some = 1`.
wire_tags! { SpawnResult;
pub(crate) const SPAWN_RESULT_OK: u8 = 0;
pub(crate) const SPAWN_RESULT_ERR: u8 = 1;
}

// Wire tags for the `SpawnError` tagged union (SPEC §7.2 / §10.1).
wire_tags! { SpawnError;
pub(crate) const SPAWN_ERROR_TAG_GROUP_NOT_FOUND: u8 = 0;
pub(crate) const SPAWN_ERROR_TAG_SPAWN_FAILED: u8 = 1;
pub(crate) const SPAWN_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE: u8 = 2;
pub(crate) const SPAWN_ERROR_TAG_SATELLITE_UNREACHABLE: u8 = 3;
pub(crate) const SPAWN_ERROR_TAG_UNSUPPORTED_KIND: u8 = 4;
pub(crate) const SPAWN_ERROR_TAG_PARENT_NOT_FOUND: u8 = 5;
pub(crate) const SPAWN_ERROR_TAG_PARENT_KIND_MISMATCH: u8 = 6;
pub(crate) const SPAWN_ERROR_TAG_IDEMPOTENCY_CONFLICT: u8 = 7;
}

// `MoveResult` / `MoveError` tags (ADR-0056).
wire_tags! { MoveResult;
pub(crate) const MOVE_RESULT_OK: u8 = 0;
pub(crate) const MOVE_RESULT_ERR: u8 = 1;
}
wire_tags! { MoveError;
pub(crate) const MOVE_ERROR_TAG_MOVE_FAILED: u8 = 0;
pub(crate) const MOVE_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE: u8 = 1;
}

// Wire tags for the `Scope` tagged union (SPEC §7.4 / §11.L3).
wire_tags! { Scope;
pub(crate) const SCOPE_TAG_RESOURCE: u8 = 0;
pub(crate) const SCOPE_TAG_GROUP: u8 = 1;
pub(crate) const SCOPE_TAG_GLOBAL: u8 = 2;
}

// Control-plane command envelope (SPEC §5, ADR-0021): typed `Command`s
// correlated by `request_id` instead of per-verb frames.

wire_tags! { Message;
/// Discriminant for `COMMAND` (client to server, `docs/protocol.md#commands-events-resources`).
pub const TYPE_COMMAND: u8 = 0x31;
/// Discriminant for `COMMAND_RESULT` (server to client, `docs/protocol.md`).
pub const TYPE_COMMAND_RESULT: u8 = 0xC2;
}

// Agent-event push stream (SPEC §7.5, ADR-0022).

wire_tags! { Message;
/// Discriminant for `SUBSCRIBE_EVENTS` (client to server, `docs/protocol.md`).
pub const TYPE_SUBSCRIBE_EVENTS: u8 = 0x41;
/// Discriminant for `EVENT` (server to client, `docs/protocol.md#commands-events-resources`).
pub const TYPE_EVENT: u8 = 0xB3;
}

// `AgentEvent` tags (SPEC §7.5 / §10.3). Each event is `tag: u8` + a
// length-prefixed body, so an unknown tag skips to `AgentEvent::Unknown`.
wire_tags! { Event;
pub(crate) const EVENT_TAG_COMMAND_STARTED: u8 = 0x00;
pub(crate) const EVENT_TAG_COMMAND_FINISHED: u8 = 0x01;
pub(crate) const EVENT_TAG_TITLE_CHANGED: u8 = 0x02;
pub(crate) const EVENT_TAG_BELL: u8 = 0x03;
pub(crate) const EVENT_TAG_RESOURCE_SPAWNED: u8 = 0x04;
pub(crate) const EVENT_TAG_RESOURCE_CLOSED: u8 = 0x05;
pub(crate) const EVENT_TAG_DIRTY: u8 = 0x06;
pub(crate) const EVENT_TAG_IDLE: u8 = 0x07;
pub(crate) const EVENT_TAG_TERMINAL_CONTROL: u8 = 0x08;
pub(crate) const EVENT_TAG_ASKED: u8 = 0x09;
pub(crate) const EVENT_TAG_CWD_CHANGED: u8 = 0x0a;
pub(crate) const EVENT_TAG_JOURNAL_GAP: u8 = 0x0b;
pub(crate) const EVENT_TAG_SOURCE_GAP: u8 = 0x0c;
pub(crate) const EVENT_TAG_APPROVAL_REQUESTED: u8 = 0x0d;
pub(crate) const EVENT_TAG_APPROVAL_DECIDED: u8 = 0x0e;
pub(crate) const EVENT_TAG_INPUT_ACTIVITY: u8 = 0x0f;
// The organization events (`docs/organization.md`).
pub(crate) const EVENT_TAG_SESSION_CREATED: u8 = 0x10;
pub(crate) const EVENT_TAG_SESSION_RENAMED: u8 = 0x11;
pub(crate) const EVENT_TAG_SESSION_DELETED: u8 = 0x12;
pub(crate) const EVENT_TAG_SESSION_LIFETIME_CHANGED: u8 = 0x13;
pub(crate) const EVENT_TAG_MEMBERSHIP_CHANGED: u8 = 0x14;
pub(crate) const EVENT_TAG_RESOURCE_CREATED: u8 = 0x15;
pub(crate) const EVENT_TAG_RESOURCE_REMOVED: u8 = 0x16;
pub(crate) const EVENT_TAG_ACTIVATION_CHANGED: u8 = 0x17;
pub(crate) const EVENT_TAG_ARRANGEMENT_CHANGED: u8 = 0x18;
pub(crate) const EVENT_TAG_LEASE_CHANGED: u8 = 0x19;
pub(crate) const EVENT_TAG_INTAKE_CHANGED: u8 = 0x1a;
pub(crate) const EVENT_TAG_RESOURCE_RENAMED: u8 = 0x1b;
pub(crate) const EVENT_TAG_NATIVE_AGENT_RESULT: u8 = 0x1c;
pub(crate) const EVENT_TAG_SESSION_PROJECT_SET: u8 = 0x1d;
// `0x1e` and `0x1f` are left free for concurrent work.
pub(crate) const EVENT_TAG_DOCUMENT_INPUT: u8 = 0x20;
pub(crate) const EVENT_TAG_AGENT_REQUEST: u8 = 0x21;
}
