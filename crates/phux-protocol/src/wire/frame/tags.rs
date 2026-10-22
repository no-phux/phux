//! Message, spawn/move result, scope, and event wire tags.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`frame` re-exports these crate-internal tags"
)]

#[cfg(doc)]
use super::{AgentEvent, MoveResult, SpawnResult};
use super::{FrameType, tag_registry};

wire_tags! { Message;
/// Discriminant for `METADATA_VALUE` (server to client, `docs/protocol.md`):
/// the `GET_METADATA` reply, `None` when the key is absent.
pub const TYPE_METADATA_VALUE: u8 = FrameType::MetadataValue as u8;

/// Discriminant for `METADATA_KEYS` (server to client, `docs/protocol.md`):
/// the `LIST_METADATA` reply, sorted key names only.
pub const TYPE_METADATA_KEYS: u8 = FrameType::MetadataKeys as u8;

/// Discriminant for `LIST_DIRECTORY` (client to server, `docs/protocol.md`);
/// gated on [`ServerFeature::ListDirectory`](crate::caps::ServerFeature::ListDirectory).
pub const TYPE_LIST_DIRECTORY: u8 = FrameType::ListDirectory as u8;

/// Discriminant for `DIRECTORY_LISTING` (server to client, `docs/protocol.md`):
/// the listing or a typed refusal.
pub const TYPE_DIRECTORY_LISTING: u8 = FrameType::DirectoryListing as u8;

/// `PATH_QUERY` (client to server, L3 §5).
pub const TYPE_PATH_QUERY: u8 = FrameType::PathQuery as u8;
/// `PATH_RESULTS` (server to client, L3 §5).
pub const TYPE_PATH_RESULTS: u8 = FrameType::PathResults as u8;
}

// L1 resource lifecycle discriminants (SPEC §7.2 / §10.1).

wire_tags! { Message;
/// Discriminant for `SPAWN_RESOURCE` (client to server, `docs/protocol.md`);
/// answered by [`TYPE_RESOURCE_SPAWNED`].
pub const TYPE_SPAWN_RESOURCE: u8 = FrameType::SpawnResource as u8;
/// Discriminant for `RESIZE_TERMINAL` (client to server, `docs/protocol.md`):
/// a per-Terminal PTY resize.
pub const TYPE_RESIZE_TERMINAL: u8 = FrameType::ResizeTerminal as u8;

// `0x24..=0x29` and `0xA3..=0xA7` stay unallocated: non-Terminal things are a
// `ResourceKind`, not a parallel frame family.

/// Discriminant for `MOVE_RESOURCE` (client to server, `docs/protocol.md`,
/// ADR-0056): re-parent a live Terminal into the window owning
/// `owner_terminal`, leaving its process and state untouched. Gated on
/// `MOVE_RESOURCE`; answered by [`TYPE_RESOURCE_MOVED`].
pub const TYPE_MOVE_RESOURCE: u8 = FrameType::MoveResource as u8;
/// Discriminant for `RESOURCE_MOVED` (server to client): the
/// `MOVE_RESOURCE` reply carrying a [`MoveResult`].
pub const TYPE_RESOURCE_MOVED: u8 = FrameType::ResourceMoved as u8;

/// Discriminant for `RESOURCE_CLOSED` (server to client, `docs/protocol.md`):
/// a resource ended.
pub const TYPE_RESOURCE_CLOSED: u8 = FrameType::ResourceClosed as u8;
/// Discriminant for `RESOURCE_SPAWNED` (server to client): the
/// `SPAWN_RESOURCE` reply carrying a [`SpawnResult`].
pub const TYPE_RESOURCE_SPAWNED: u8 = FrameType::ResourceSpawned as u8;
}

// `Result`-shaped unions use `Ok = 0`, `Err = 1`, mirroring `Option`'s
// `None = 0`, `Some = 1`.
wire_tags! { SpawnResult;
/// Wire tag for [`SpawnResult::Ok`].
pub(crate) const SPAWN_RESULT_OK: u8 = 0;
/// Wire tag for [`SpawnResult::Err`].
pub(crate) const SPAWN_RESULT_ERR: u8 = 1;
}

// Wire tags for the `SpawnError` tagged union (SPEC §7.2 / §10.1).
wire_tags! { SpawnError;
/// Wire tag for [`SpawnError::GroupNotFound`].
pub(crate) const SPAWN_ERROR_TAG_GROUP_NOT_FOUND: u8 = 0;
/// Wire tag for [`SpawnError::SpawnFailed`].
pub(crate) const SPAWN_ERROR_TAG_SPAWN_FAILED: u8 = 1;
/// Wire tag for [`SpawnError::UnsupportedSatelliteRoute`].
pub(crate) const SPAWN_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE: u8 = 2;
/// Wire tag for [`SpawnError::SatelliteUnreachable`].
pub(crate) const SPAWN_ERROR_TAG_SATELLITE_UNREACHABLE: u8 = 3;
/// Wire tag for [`SpawnError::UnsupportedKind`].
pub(crate) const SPAWN_ERROR_TAG_UNSUPPORTED_KIND: u8 = 4;
/// Wire tag for [`SpawnError::ParentNotFound`].
pub(crate) const SPAWN_ERROR_TAG_PARENT_NOT_FOUND: u8 = 5;
/// Wire tag for [`SpawnError::ParentKindMismatch`].
pub(crate) const SPAWN_ERROR_TAG_PARENT_KIND_MISMATCH: u8 = 6;
/// Wire tag for [`SpawnError::IdempotencyConflict`] (ADR-0126).
pub(crate) const SPAWN_ERROR_TAG_IDEMPOTENCY_CONFLICT: u8 = 7;
}

// `MoveResult` / `MoveError` tags (ADR-0056).
wire_tags! { MoveResult;
/// Wire tag for [`MoveResult::Ok`].
pub(crate) const MOVE_RESULT_OK: u8 = 0;
/// Wire tag for [`MoveResult::Err`].
pub(crate) const MOVE_RESULT_ERR: u8 = 1;
}
wire_tags! { MoveError;
/// Wire tag for [`MoveError::MoveFailed`].
pub(crate) const MOVE_ERROR_TAG_MOVE_FAILED: u8 = 0;
/// Wire tag for [`MoveError::UnsupportedSatelliteRoute`].
pub(crate) const MOVE_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE: u8 = 1;
}

// Wire tags for the `Scope` tagged union (SPEC §7.4 / §11.L3).
wire_tags! { Scope;
/// Wire tag for [`Scope::Resource`].
pub(crate) const SCOPE_TAG_RESOURCE: u8 = 0;
/// Wire tag for [`Scope::Group`].
pub(crate) const SCOPE_TAG_GROUP: u8 = 1;
/// Wire tag for [`Scope::Global`].
pub(crate) const SCOPE_TAG_GLOBAL: u8 = 2;
}

// Control-plane command envelope (SPEC §5, ADR-0021): typed `Command`s
// correlated by `request_id` instead of per-verb frames.

wire_tags! { Message;
/// Discriminant for `COMMAND` (client to server, `docs/protocol.md#commands-events-resources`).
pub const TYPE_COMMAND: u8 = FrameType::Command as u8;
/// Discriminant for `COMMAND_RESULT` (server to client, `docs/protocol.md`).
pub const TYPE_COMMAND_RESULT: u8 = FrameType::CommandResult as u8;
}

// Agent-event push stream (SPEC §7.5, ADR-0022).

wire_tags! { Message;
/// Discriminant for `SUBSCRIBE_EVENTS` (client to server, `docs/protocol.md`).
pub const TYPE_SUBSCRIBE_EVENTS: u8 = FrameType::SubscribeEvents as u8;
/// Discriminant for `EVENT` (server to client, `docs/protocol.md#commands-events-resources`).
pub const TYPE_EVENT: u8 = FrameType::Event as u8;
}

// `AgentEvent` tags (SPEC §7.5 / §10.3). Each event is `tag: u8` + a
// length-prefixed body, so an unknown tag skips to [`AgentEvent::Unknown`].
wire_tags! { Event;
/// Wire tag for [`AgentEvent::CommandStarted`].
pub(crate) const EVENT_TAG_COMMAND_STARTED: u8 = 0x00;
/// Wire tag for [`AgentEvent::CommandFinished`].
pub(crate) const EVENT_TAG_COMMAND_FINISHED: u8 = 0x01;
/// Wire tag for [`AgentEvent::TitleChanged`].
pub(crate) const EVENT_TAG_TITLE_CHANGED: u8 = 0x02;
/// Wire tag for [`AgentEvent::Bell`].
pub(crate) const EVENT_TAG_BELL: u8 = 0x03;
/// Wire tag for [`AgentEvent::ResourceSpawned`].
pub(crate) const EVENT_TAG_RESOURCE_SPAWNED: u8 = 0x04;
/// Wire tag for [`AgentEvent::ResourceClosed`].
pub(crate) const EVENT_TAG_RESOURCE_CLOSED: u8 = 0x05;
/// Wire tag for [`AgentEvent::Dirty`].
pub(crate) const EVENT_TAG_DIRTY: u8 = 0x06;
/// Wire tag for [`AgentEvent::Idle`].
pub(crate) const EVENT_TAG_IDLE: u8 = 0x07;
/// Wire tag for [`AgentEvent::TerminalControl`]. The supervisory broadcast
/// (ADR-0033): emitted to every subscriber whenever a Terminal's input lease
/// or process lifecycle changes — who holds the wheel, and `Running` /
/// `Frozen` / `Exited`. The live-dashboard signal and the seed of the
/// recorded audit trail.
pub(crate) const EVENT_TAG_TERMINAL_CONTROL: u8 = 0x08;
/// Wire tag for [`AgentEvent::Asked`]. Appended after `TERMINAL_CONTROL`'s
/// `0x08`; `ASKED` is an additive agent-surface event (phux-2sl6) that carries
/// an agent's pending human-answerable question so a projection consumer can
/// render the waiting prompt without re-deriving it from the grid. Its body
/// is field-tagged TLV (not positional) so the suggestion list and the
/// optional elapsed counter are additive and an older decoder skips the whole
/// event by its length prefix as [`AgentEvent::Unknown`].
pub(crate) const EVENT_TAG_ASKED: u8 = 0x09;
/// Wire tag for [`AgentEvent::CwdChanged`]. Appended after `ASKED`'s `0x09`
/// (phux-foz.4): the scoped Terminal's working directory changed. Sourced
/// server-side from the kernel cwd of the PTY child (the same query the
/// spawn-inheritance path uses), polled at OSC-133 prompt boundaries and
/// output-idle and coalesced on change. Backs the `cwd` status widget.
pub(crate) const EVENT_TAG_CWD_CHANGED: u8 = 0x0a;
/// Wire tag for [`AgentEvent::JournalGap`] (ADR-0123): the subscription
/// missed a range of journaled events and the consumer re-reads level state.
/// A per-subscription notice, never journaled itself.
pub(crate) const EVENT_TAG_JOURNAL_GAP: u8 = 0x0b;
/// Wire tag for [`AgentEvent::SourceGap`] (ADR-0123): the scoped resource
/// produced events the server dropped before it could journal them.
pub(crate) const EVENT_TAG_SOURCE_GAP: u8 = 0x0c;
/// Wire tag for [`AgentEvent::ApprovalRequested`] (ADR-0128): a `SIGNAL`
/// action was held for approval.
pub(crate) const EVENT_TAG_APPROVAL_REQUESTED: u8 = 0x0d;
/// Wire tag for [`AgentEvent::ApprovalDecided`] (ADR-0128): a held action
/// was approved, denied, expired, or withdrawn.
pub(crate) const EVENT_TAG_APPROVAL_DECIDED: u8 = 0x0e;
/// Wire tag for [`AgentEvent::InputActivity`]. A live, unjournaled notice
/// that someone just sent input to the scoped Terminal. The body is one
/// positional length-prefixed origin label. No stamp, so no credential id
/// crosses, and an older decoder skips the tag by its length prefix.
pub(crate) const EVENT_TAG_INPUT_ACTIVITY: u8 = 0x0f;
/// Wire tag for [`AgentEvent::SessionCreated`] (`docs/organization.md`).
pub(crate) const EVENT_TAG_SESSION_CREATED: u8 = 0x10;
/// Wire tag for [`AgentEvent::SessionRenamed`] (`docs/organization.md`).
pub(crate) const EVENT_TAG_SESSION_RENAMED: u8 = 0x11;
/// Wire tag for [`AgentEvent::SessionDeleted`] (`docs/organization.md`).
pub(crate) const EVENT_TAG_SESSION_DELETED: u8 = 0x12;
/// Wire tag for [`AgentEvent::SessionLifetimeChanged`] (`docs/organization.md`).
pub(crate) const EVENT_TAG_SESSION_LIFETIME_CHANGED: u8 = 0x13;
/// Wire tag for [`AgentEvent::MembershipChanged`] (`docs/organization.md`).
pub(crate) const EVENT_TAG_MEMBERSHIP_CHANGED: u8 = 0x14;
/// Wire tag for [`AgentEvent::ResourceCreated`] (`docs/organization.md`).
pub(crate) const EVENT_TAG_RESOURCE_CREATED: u8 = 0x15;
/// Wire tag for [`AgentEvent::ResourceRemoved`] (`docs/organization.md`).
pub(crate) const EVENT_TAG_RESOURCE_REMOVED: u8 = 0x16;
/// Wire tag for [`AgentEvent::ActivationChanged`] (`docs/organization.md`).
pub(crate) const EVENT_TAG_ACTIVATION_CHANGED: u8 = 0x17;
/// Wire tag for [`AgentEvent::ArrangementChanged`] (`docs/organization.md`).
pub(crate) const EVENT_TAG_ARRANGEMENT_CHANGED: u8 = 0x18;
/// Wire tag for [`AgentEvent::LeaseChanged`] (`docs/organization.md`).
pub(crate) const EVENT_TAG_LEASE_CHANGED: u8 = 0x19;
/// Wire tag for [`AgentEvent::IntakeChanged`] (`docs/organization.md`).
pub(crate) const EVENT_TAG_INTAKE_CHANGED: u8 = 0x1a;
/// Wire tag for [`AgentEvent::ResourceRenamed`] (`docs/organization.md`).
pub(crate) const EVENT_TAG_RESOURCE_RENAMED: u8 = 0x1b;
/// Wire tag for [`AgentEvent::NativeAgentResult`].
pub(crate) const EVENT_TAG_NATIVE_AGENT_RESULT: u8 = 0x1c;
/// Wire tag for [`AgentEvent::SessionProjectSet`] (`docs/organization.md`),
/// after `NATIVE_AGENT_RESULT`.
pub(crate) const EVENT_TAG_SESSION_PROJECT_SET: u8 = 0x1d;
}
