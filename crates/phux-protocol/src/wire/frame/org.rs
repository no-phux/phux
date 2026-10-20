//! Organization commands, results, and events (`docs/spec/L1.md` §5.7).
//!
//! Sessions and resources are persistent records named by 16-byte keys;
//! live handles (`SessionId`, `ResourceId`) name current execution. Every
//! organization body is field-tagged TLV in one id space
//! ([`field::org`]), so a field means the same thing in a command, a value,
//! and an event, and every body is additive. Gated on
//! [`ServerFeatureExt::Organization`](crate::caps::ServerFeatureExt::Organization).

use bytes::BytesMut;

use crate::ids::{
    ActivationId, ArrangementKey, AuthorityId, LeaseEpoch, OperationId, ResourceId, ResourceKey,
    ResourceKind, Revision, ServerInstance, SessionId, SessionKey, WorkspaceKey,
};
use crate::kinds::Verb;
use crate::wire::decode::{Decoder, utf8_value};
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::field;

use super::codec::{
    decode_actor_ref, decode_server_instance, decode_terminal_id, encode_actor_ref,
    encode_server_instance, encode_terminal_id,
};
use super::{
    ActorRef, AgentEvent, Command, EVENT_TAG_ACTIVATION_CHANGED, EVENT_TAG_ARRANGEMENT_CHANGED,
    EVENT_TAG_INTAKE_CHANGED, EVENT_TAG_LEASE_CHANGED, EVENT_TAG_MEMBERSHIP_CHANGED,
    EVENT_TAG_RESOURCE_CREATED, EVENT_TAG_RESOURCE_REMOVED, EVENT_TAG_RESOURCE_RENAMED,
    EVENT_TAG_SESSION_CREATED, EVENT_TAG_SESSION_DELETED, EVENT_TAG_SESSION_LIFETIME_CHANGED,
    EVENT_TAG_SESSION_RENAMED, MAX_HISTORY_CURSOR_BYTES,
};

mod accumulator;
mod arrangement;
mod command_decode;
mod command_encode;
mod events;
mod field_values;
mod model;
mod records;
mod refusal;
mod values;

use accumulator::{
    Fields, write_actor, write_name, write_operation, write_project, write_resource, write_session,
};
pub(in crate::wire) use arrangement::decode_arrangement_doc;
use arrangement::write_document;
pub(in crate::wire) use command_decode::decode_org_command;
pub(in crate::wire) use command_encode::encode_org_command;
pub(in crate::wire) use events::{decode_org_event, encode_org_event};
use field_values::{
    bounded_dec_str, bounded_str, decode_filter, decode_launch, decode_live_target,
    decode_move_item, decode_outcome, decode_pair, decode_pin, encode_filter, encode_launch,
    encode_live_target, encode_move_item, encode_outcome, encode_pair, encode_pin, enum_byte,
    key_bytes, push_bounded, read_key, read_operation, read_u8, read_u32, read_u64, req, write_key,
    write_u8, write_u32, write_u64,
};
pub use model::{
    ArrangementDoc, ArrangementGroup, ArrangementNode, ArrangementOwner, EntryRef, EntryTarget,
    LaunchSpec, LiveTarget, MoveItem, OrgFilter, Outcome, RecordRevision,
};
use model::{
    FILTER_ALL, FILTER_INTAKE, FILTER_SESSION, FILTER_WORKSPACE, MAX_ENTRY_HOST_BYTES,
    OUTCOME_EXITED, OUTCOME_SIGNALED, OUTCOME_UNOBSERVED, TARGET_MEMBER, TARGET_SATELLITE,
};
pub use records::{
    ActivationInfo, ArrangementValue, Committed, Delegation, MAX_CREDENTIAL_ID_BYTES,
    MAX_SOCKET_PATH_BYTES, OrganizationPage, ResourceRecord, SessionRecord,
};
pub use refusal::Refusal;
pub(in crate::wire) use refusal::{decode_refusal, encode_refusal};
use values::write_owner;
pub(in crate::wire) use values::{
    decode_arrangement_value, decode_committed, decode_delegation, decode_organization_page,
    encode_arrangement_value, encode_committed, encode_delegation, encode_organization_page,
};

/// Most bytes in an organization name (a session, resource, or group name).
pub const MAX_ORG_NAME_BYTES: usize = 256;
/// Most bytes in an encoded arrangement document.
pub const MAX_ARRANGEMENT_BYTES: usize = 64 * 1024;
/// Most items in one bulk move, and most revision pins in one undo.
pub const MAX_ORG_BATCH: usize = 256;
/// Most descriptive attributes on one session.
pub const MAX_ORG_ATTRS: usize = 64;
/// Most argv elements or environment pairs in one `LaunchSpec`.
pub const MAX_LAUNCH_LIST: usize = 256;
/// Most bytes in a `LaunchSpec` restore record.
pub const MAX_LAUNCH_RESTORE_BYTES: usize = 16 * 1024;
/// Most bytes in an organization page cursor.
pub const MAX_ORG_CURSOR_BYTES: usize = MAX_HISTORY_CURSOR_BYTES;
/// Deepest split nesting an arrangement group may hold.
pub const MAX_ARRANGEMENT_DEPTH: usize = crate::wire::info::MAX_LAYOUT_DEPTH;

wire_enum! { to_u8 / from_u8;
/// Whether a session outlives its last resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SessionLifetime {
    /// Survives empty; the default for a named session.
    #[default]
    Persistent = 0,
    /// Deleted in the same transaction that removes its last resource; never
    /// empty after a commit.
    Ephemeral = 1,
}
}

wire_enum! { to_u8 / from_u8;
/// How `RESOURCE_TERMINATE` ends an activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TerminateMode {
    /// Ask the process to exit (SIGTERM for a Terminal).
    #[default]
    Graceful = 0,
    /// End it now (SIGKILL for a Terminal).
    Force = 1,
}
}

wire_enum! { to_u8 / from_u8;
/// What happens to a resource record when its execution ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ExitPolicy {
    /// Remove the record: an interactive shell closes when it exits.
    #[default]
    Remove = 0,
    /// Keep the record with its outcome, for inspection or relaunch.
    Retain = 1,
}
}

wire_enum! { to_u8 / from_u8;
/// Whether a created resource starts executing in the same transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LaunchMode {
    /// Commit the launch intent with the record: the first activation starts.
    #[default]
    Immediate = 0,
    /// Store the launch spec without an activation. The record stays dormant
    /// until a `RESOURCE_RELAUNCH` starts it.
    Deferred = 1,
}
}

wire_enum! { to_u8 / from_u8;
/// Where one activation is in its execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecState {
    /// Launch intent committed; the executor has not reported running.
    Starting = 0,
    /// Executing.
    Running = 1,
    /// A termination was requested and has not completed.
    Stopping = 2,
    /// Ended; the outcome says how.
    Completed = 3,
    /// Could not start.
    Failed = 4,
    /// Recorded live under a previous server instance, and its process may
    /// still run: remove and relaunch refuse until it is resolved.
    Lost = 5,
    /// A lost activation the owner released from management.
    Abandoned = 6,
}
}

wire_enum! { to_u8 / from_u8;
/// Axis of an arrangement split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SplitDirection {
    /// Side by side.
    Horizontal = 0,
    /// Stacked.
    Vertical = 1,
}
}

// ---------------------------------------------------------------------------
// Field helpers.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Arrangement documents.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The shared field accumulator.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Results and values.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------
