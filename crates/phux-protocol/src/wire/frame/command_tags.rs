//! `COMMAND` envelope tags and the tags of the unions it carries.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`frame` re-exports these crate-internal tags"
)]

use super::tag_registry;
#[cfg(doc)]
use super::{AttachTarget, Command, CommandResult, CommandValue, StateScope};

/// Wire tag for one [`Command`] variant inside the `COMMAND` envelope
/// (`docs/protocol.md#commands-events-resources`); duplicates are E0081.
/// Registry: `docs/protocol.md#changing-the-wire`.
/// `0x0a`, `0x0b`, and `0x2a` are freed and reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
enum CommandTag {
    AttachResource = 0x01,
    DetachResource = 0x02,
    KillResource = 0x03,
    GetState = 0x05,
    GetScreen = 0x07,
    RouteInput = 0x08,
    KillResources = 0x09,
    GetTerminalState = 0x0c,
    SubscribeResourceEvents = 0x0d,
    Upgrade = 0x0e,
    AcquireInput = 0x0f,
    ReleaseInput = 0x10,
    SignalTerminal = 0x11,
    ReportAsked = 0x12,
    DetachClients = 0x13,
    ApplyInput = 0x14,
    PutFile = 0x15,
    Shutdown = 0x16,
    ReportAgentState = 0x17,
    GetPerf = 0x18,
    Transcribe = 0x19,
    AppendResourceOutput = 0x1a,
    KillResourceIf = 0x1b,
    OpenListener = 0x1c,
    CloseTabResources = 0x1d,
    GetOrganization = 0x1e,
    SessionCreate = 0x1f,
    SessionRename = 0x20,
    SessionSetLifetime = 0x21,
    SessionDelete = 0x22,
    ResourceCreate = 0x23,
    ResourceMove = 0x24,
    ResourceMoveMany = 0x25,
    ResourceRename = 0x26,
    ResourceTerminate = 0x27,
    ResourceRemove = 0x28,
    ResourceRelaunch = 0x29,
    // 0x2a is reserved: it was `RESOURCE_ABANDON` and is never reused.
    InboxResolve = 0x2b,
    OrganizationUndo = 0x2c,
    ArrangementGet = 0x2d,
    ArrangementReplace = 0x2e,
    DelegateAgent = 0x35,
    RevokeDelegation = 0x36,
    SessionKill = 0x37,
    NativeAgent = 0x38,
    PerfControl = 0x39,
    SessionSetProject = 0x3a,
    GetFile = 0x3b,
    Checkout = 0x3c,
    ServiceHttpOpen = 0x3d,
    ServiceHttpRead = 0x3e,
    ServiceHttpClose = 0x3f,
}

// `Command` tags (SPEC §5.1). `0x00`, `0x04`, and `0x06` are reserved catalog
// slots and decode as `UnknownEnumValue`.
wire_tags! { Command;
/// Wire tag for [`Command::AttachResource`].
pub(crate) const COMMAND_TAG_ATTACH_RESOURCE: u8 = CommandTag::AttachResource as u8;
/// Wire tag for [`Command::DetachResource`].
pub(crate) const COMMAND_TAG_DETACH_RESOURCE: u8 = CommandTag::DetachResource as u8;
/// Wire tag for [`Command::KillResource`].
pub(crate) const COMMAND_TAG_KILL_RESOURCE: u8 = CommandTag::KillResource as u8;
/// Wire tag for [`Command::GetState`].
pub(crate) const COMMAND_TAG_GET_STATE: u8 = CommandTag::GetState as u8;
/// Wire tag for [`Command::GetScreen`].
pub(crate) const COMMAND_TAG_GET_SCREEN: u8 = CommandTag::GetScreen as u8;
/// Wire tag for [`Command::RouteInput`].
pub(crate) const COMMAND_TAG_ROUTE_INPUT: u8 = CommandTag::RouteInput as u8;
/// Wire tag for [`Command::KillResources`], reusing the slot the dissolved
/// L2 lifecycle verbs freed (ADR-0019 / ADR-0027).
pub(crate) const COMMAND_TAG_KILL_RESOURCES: u8 = CommandTag::KillResources as u8;
/// Wire tag for [`Command::GetTerminalState`].
pub(crate) const COMMAND_TAG_GET_TERMINAL_STATE: u8 = CommandTag::GetTerminalState as u8;
/// Wire tag for [`Command::SubscribeResourceEvents`].
pub(crate) const COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS: u8 =
    CommandTag::SubscribeResourceEvents as u8;
/// Wire tag for [`Command::Upgrade`].
pub(crate) const COMMAND_TAG_UPGRADE: u8 = CommandTag::Upgrade as u8;
/// Wire tag for [`Command::AcquireInput`].
pub(crate) const COMMAND_TAG_ACQUIRE_INPUT: u8 = CommandTag::AcquireInput as u8;
/// Wire tag for [`Command::ReleaseInput`].
pub(crate) const COMMAND_TAG_RELEASE_INPUT: u8 = CommandTag::ReleaseInput as u8;
/// Wire tag for [`Command::SignalTerminal`].
pub(crate) const COMMAND_TAG_SIGNAL_TERMINAL: u8 = CommandTag::SignalTerminal as u8;
/// Wire tag for [`Command::ReportAsked`].
pub(crate) const COMMAND_TAG_REPORT_ASKED: u8 = CommandTag::ReportAsked as u8;
/// Wire tag for [`Command::DetachClients`].
pub(crate) const COMMAND_TAG_DETACH_CLIENTS: u8 = CommandTag::DetachClients as u8;
/// Wire tag for [`Command::ApplyInput`].
pub(crate) const COMMAND_TAG_APPLY_INPUT: u8 = CommandTag::ApplyInput as u8;
}
/// Maximum number of events in one [`Command::ApplyInput`] batch.
pub const MAX_APPLY_INPUT_EVENTS: usize = 256;
/// Maximum encoded bytes in the nested [`Command::ApplyInput`] command body.
pub const MAX_APPLY_INPUT_COMMAND_BODY: usize = 64 * 1024;
wire_tags! { Command;
/// Wire tag for [`Command::PutFile`].
pub(crate) const COMMAND_TAG_PUT_FILE: u8 = CommandTag::PutFile as u8;
/// Wire tag for [`Command::GetFile`].
pub(crate) const COMMAND_TAG_GET_FILE: u8 = CommandTag::GetFile as u8;
/// Wire tag for [`Command::Checkout`].
pub(crate) const COMMAND_TAG_CHECKOUT: u8 = CommandTag::Checkout as u8;
/// Wire tag for [`Command::ServiceHttpOpen`].
pub(crate) const COMMAND_TAG_SERVICE_HTTP_OPEN: u8 = CommandTag::ServiceHttpOpen as u8;
/// Wire tag for [`Command::ServiceHttpRead`].
pub(crate) const COMMAND_TAG_SERVICE_HTTP_READ: u8 = CommandTag::ServiceHttpRead as u8;
/// Wire tag for [`Command::ServiceHttpClose`].
pub(crate) const COMMAND_TAG_SERVICE_HTTP_CLOSE: u8 = CommandTag::ServiceHttpClose as u8;
/// Wire tag for [`Command::Shutdown`].
pub(crate) const COMMAND_TAG_SHUTDOWN: u8 = CommandTag::Shutdown as u8;
/// Wire tag for [`Command::ReportAgentState`].
pub(crate) const COMMAND_TAG_REPORT_AGENT_STATE: u8 = CommandTag::ReportAgentState as u8;
/// Wire tag for [`Command::GetPerf`].
pub(crate) const COMMAND_TAG_GET_PERF: u8 = CommandTag::GetPerf as u8;
/// Wire tag for [`Command::PerfControl`].
pub(crate) const COMMAND_TAG_PERF_CONTROL: u8 = CommandTag::PerfControl as u8;
/// Wire tag for [`Command::SessionSetProject`]: an organization command gated
/// on `features_ext.SESSION_PROJECT`.
pub(crate) const COMMAND_TAG_SESSION_SET_PROJECT: u8 = CommandTag::SessionSetProject as u8;
/// Wire tag for [`Command::Transcribe`].
pub(crate) const COMMAND_TAG_TRANSCRIBE: u8 = CommandTag::Transcribe as u8;
/// Wire tag for [`Command::AppendResourceOutput`].
pub(crate) const COMMAND_TAG_APPEND_RESOURCE_OUTPUT: u8 = CommandTag::AppendResourceOutput as u8;
/// Wire tag for [`Command::KillResourceIf`]: a new tag rather than a field on
/// `KILL_RESOURCE`, so a peer without `CONDITIONAL_KILL` fails to decode it
/// instead of killing unconditionally (ADR-0109).
pub(crate) const COMMAND_TAG_KILL_RESOURCE_IF: u8 = CommandTag::KillResourceIf as u8;
/// Wire tag for [`Command::OpenListener`].
pub(crate) const COMMAND_TAG_OPEN_LISTENER: u8 = CommandTag::OpenListener as u8;
/// Wire tag for [`Command::CloseTabResources`].
pub(crate) const COMMAND_TAG_CLOSE_TAB_RESOURCES: u8 = CommandTag::CloseTabResources as u8;
/// Wire tag for [`Command::NativeAgent`].
pub(crate) const COMMAND_TAG_NATIVE_AGENT: u8 = CommandTag::NativeAgent as u8;
}

// Organization commands (`docs/organization.md`), contiguous from `0x1e`;
// `0x2f`..=`0x34` are reserved for the workspace commands
// (`docs/protocol.md#changing-the-wire`).
wire_tags! { Command;
/// Wire tag for [`Command::GetOrganization`].
pub(crate) const COMMAND_TAG_GET_ORGANIZATION: u8 = CommandTag::GetOrganization as u8;
/// Wire tag for [`Command::SessionCreate`].
pub(crate) const COMMAND_TAG_SESSION_CREATE: u8 = CommandTag::SessionCreate as u8;
/// Wire tag for [`Command::SessionRename`].
pub(crate) const COMMAND_TAG_SESSION_RENAME: u8 = CommandTag::SessionRename as u8;
/// Wire tag for [`Command::SessionSetLifetime`].
pub(crate) const COMMAND_TAG_SESSION_SET_LIFETIME: u8 = CommandTag::SessionSetLifetime as u8;
/// Wire tag for [`Command::SessionDelete`].
pub(crate) const COMMAND_TAG_SESSION_DELETE: u8 = CommandTag::SessionDelete as u8;
/// Wire tag for [`Command::ResourceCreate`].
pub(crate) const COMMAND_TAG_RESOURCE_CREATE: u8 = CommandTag::ResourceCreate as u8;
/// Wire tag for [`Command::ResourceMove`].
pub(crate) const COMMAND_TAG_RESOURCE_MOVE: u8 = CommandTag::ResourceMove as u8;
/// Wire tag for [`Command::ResourceMoveMany`].
pub(crate) const COMMAND_TAG_RESOURCE_MOVE_MANY: u8 = CommandTag::ResourceMoveMany as u8;
/// Wire tag for [`Command::ResourceRename`].
pub(crate) const COMMAND_TAG_RESOURCE_RENAME: u8 = CommandTag::ResourceRename as u8;
/// Wire tag for [`Command::ResourceTerminate`].
pub(crate) const COMMAND_TAG_RESOURCE_TERMINATE: u8 = CommandTag::ResourceTerminate as u8;
/// Wire tag for [`Command::ResourceRemove`].
pub(crate) const COMMAND_TAG_RESOURCE_REMOVE: u8 = CommandTag::ResourceRemove as u8;
/// Wire tag for [`Command::ResourceRelaunch`].
pub(crate) const COMMAND_TAG_RESOURCE_RELAUNCH: u8 = CommandTag::ResourceRelaunch as u8;
/// Wire tag for [`Command::InboxResolve`].
pub(crate) const COMMAND_TAG_INBOX_RESOLVE: u8 = CommandTag::InboxResolve as u8;
/// Wire tag for [`Command::OrganizationUndo`].
pub(crate) const COMMAND_TAG_ORGANIZATION_UNDO: u8 = CommandTag::OrganizationUndo as u8;
/// Wire tag for [`Command::ArrangementGet`].
pub(crate) const COMMAND_TAG_ARRANGEMENT_GET: u8 = CommandTag::ArrangementGet as u8;
/// Wire tag for [`Command::ArrangementReplace`].
pub(crate) const COMMAND_TAG_ARRANGEMENT_REPLACE: u8 = CommandTag::ArrangementReplace as u8;
}

// Delegated agent credentials (`docs/substrate.md`, ADR-0163), after the
// reserved workspace block. Not organization commands: they are served
// whatever `ORGANIZATION` says, and answer with ordinary results.
wire_tags! { Command;
/// Wire tag for [`Command::DelegateAgent`].
pub(crate) const COMMAND_TAG_DELEGATE_AGENT: u8 = CommandTag::DelegateAgent as u8;
/// Wire tag for [`Command::RevokeDelegation`].
pub(crate) const COMMAND_TAG_REVOKE_DELEGATION: u8 = CommandTag::RevokeDelegation as u8;
}

// `SESSION_KILL` (`docs/organization.md`), an organization command gated on
// `features_ext.SESSION_KILL`, after the delegation pair.
wire_tags! { Command;
/// Wire tag for [`Command::SessionKill`].
pub(crate) const COMMAND_TAG_SESSION_KILL: u8 = CommandTag::SessionKill as u8;
}

// `InputEvent` tags (`ROUTE_INPUT`), mirroring the `INPUT_*` frame atoms.
wire_tags! { InputEvent;
/// Wire tag for [`InputEvent::Key`].
pub(crate) const INPUT_EVENT_TAG_KEY: u8 = 0x00;
/// Wire tag for [`InputEvent::Mouse`].
pub(crate) const INPUT_EVENT_TAG_MOUSE: u8 = 0x01;
/// Wire tag for [`InputEvent::Focus`].
pub(crate) const INPUT_EVENT_TAG_FOCUS: u8 = 0x02;
/// Wire tag for [`InputEvent::Paste`].
pub(crate) const INPUT_EVENT_TAG_PASTE: u8 = 0x03;
}
// 0x04 (`Selection`) was removed by ADR-0030.

// `StateScope` tags (SPEC §5.1).
wire_tags! { StateScope;
/// Wire tag for [`StateScope::Server`].
pub(crate) const STATE_SCOPE_TAG_SERVER: u8 = 0x00;
}

// Wire tags for the `CommandResult` tagged union (SPEC §5).
wire_tags! { CommandResult;
/// Wire tag for [`CommandResult::Ok`].
pub(crate) const COMMAND_RESULT_TAG_OK: u8 = 0x00;
/// Wire tag for [`CommandResult::OkWith`].
pub(crate) const COMMAND_RESULT_TAG_OK_WITH: u8 = 0x01;
/// Wire tag for [`CommandResult::Error`].
pub(crate) const COMMAND_RESULT_TAG_ERROR: u8 = 0x02;
/// Wire tag for [`CommandResult::Refused`]; sent only in reply to an
/// organization command.
pub(crate) const COMMAND_RESULT_TAG_REFUSED: u8 = 0x03;
/// Wire tag for [`CommandResult::Accepted`]; sent only in reply to an
/// organization command.
pub(crate) const COMMAND_RESULT_TAG_ACCEPTED: u8 = 0x04;
}

// Wire tags for the `CommandValue` tagged union (SPEC §5).
wire_tags! { CommandValue;
/// Wire tag for [`CommandValue::ResourceId`].
pub(crate) const COMMAND_VALUE_TAG_RESOURCE_ID: u8 = 0x00;
/// Wire tag for [`CommandValue::GroupId`].
pub(crate) const COMMAND_VALUE_TAG_GROUP_ID: u8 = 0x01;
/// Wire tag for [`CommandValue::State`].
pub(crate) const COMMAND_VALUE_TAG_STATE: u8 = 0x02;
/// Wire tag for [`CommandValue::Json`].
pub(crate) const COMMAND_VALUE_TAG_JSON: u8 = 0x03;
/// Wire tag for [`CommandValue::Bytes`].
pub(crate) const COMMAND_VALUE_TAG_BYTES: u8 = 0x04;
/// Wire tag for [`CommandValue::FileUpload`].
pub(crate) const COMMAND_VALUE_TAG_FILE_UPLOAD: u8 = 0x05;
/// Wire tag for [`CommandValue::OrganizationPage`].
pub(crate) const COMMAND_VALUE_TAG_ORGANIZATION_PAGE: u8 = 0x06;
/// Wire tag for [`CommandValue::Arrangement`].
pub(crate) const COMMAND_VALUE_TAG_ARRANGEMENT: u8 = 0x07;
/// Wire tag for [`CommandValue::Committed`].
pub(crate) const COMMAND_VALUE_TAG_COMMITTED: u8 = 0x08;
/// Wire tag for [`CommandValue::Delegation`].
pub(crate) const COMMAND_VALUE_TAG_DELEGATION: u8 = 0x09;
/// Wire tag for [`CommandValue::FileChunk`].
pub(crate) const COMMAND_VALUE_TAG_FILE_CHUNK: u8 = 0x0a;
/// Wire tag for [`CommandValue::Checkout`].
pub(crate) const COMMAND_VALUE_TAG_CHECKOUT: u8 = 0x0b;
/// Wire tag for [`CommandValue::ServiceHttpHead`].
pub(crate) const COMMAND_VALUE_TAG_SERVICE_HTTP_HEAD: u8 = 0x0c;
/// Wire tag for [`CommandValue::ServiceHttpChunk`].
pub(crate) const COMMAND_VALUE_TAG_SERVICE_HTTP_CHUNK: u8 = 0x0d;
}

// Wire tags for the `AttachTarget` tagged union (SPEC §13).
wire_tags! { AttachTarget;
/// Wire tag for [`AttachTarget::Last`].
pub(crate) const ATTACH_TARGET_LAST: u8 = 0;
/// Wire tag for [`AttachTarget::ByName`].
pub(crate) const ATTACH_TARGET_BY_NAME: u8 = 1;
/// Wire tag for [`AttachTarget::ById`].
pub(crate) const ATTACH_TARGET_BY_ID: u8 = 2;
/// Wire tag for [`AttachTarget::CreateIfMissing`].
pub(crate) const ATTACH_TARGET_CREATE_IF_MISSING: u8 = 3;
}
