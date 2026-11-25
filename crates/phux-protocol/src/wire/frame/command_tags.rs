//! `COMMAND` envelope tags and the tags of the unions it carries.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`frame` re-exports these crate-internal tags"
)]

use super::tag_registry;
#[cfg(doc)]
use super::{Command, CommandResult};

// `Command` tags (`docs/protocol.md#commands-events-resources`). `0x0a`,
// `0x0b`, and `0x2a` (`RESOURCE_ABANDON`) are freed and never reused.
// `0x00`, `0x04`, and `0x06` are reserved
// catalog slots and decode as `UnknownEnumValue`.
wire_tags! { Command;
pub(crate) const COMMAND_TAG_ATTACH_RESOURCE: u8 = 0x01;
pub(crate) const COMMAND_TAG_DETACH_RESOURCE: u8 = 0x02;
pub(crate) const COMMAND_TAG_KILL_RESOURCE: u8 = 0x03;
pub(crate) const COMMAND_TAG_GET_STATE: u8 = 0x05;
pub(crate) const COMMAND_TAG_GET_SCREEN: u8 = 0x07;
pub(crate) const COMMAND_TAG_ROUTE_INPUT: u8 = 0x08;
/// Wire tag for [`Command::KillResources`], reusing the slot the dissolved
/// L2 lifecycle verbs freed (ADR-0019 / ADR-0027).
pub(crate) const COMMAND_TAG_KILL_RESOURCES: u8 = 0x09;
pub(crate) const COMMAND_TAG_GET_TERMINAL_STATE: u8 = 0x0c;
pub(crate) const COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS: u8 = 0x0d;
pub(crate) const COMMAND_TAG_UPGRADE: u8 = 0x0e;
pub(crate) const COMMAND_TAG_ACQUIRE_INPUT: u8 = 0x0f;
pub(crate) const COMMAND_TAG_RELEASE_INPUT: u8 = 0x10;
pub(crate) const COMMAND_TAG_SIGNAL_TERMINAL: u8 = 0x11;
pub(crate) const COMMAND_TAG_REPORT_ASKED: u8 = 0x12;
pub(crate) const COMMAND_TAG_DETACH_CLIENTS: u8 = 0x13;
pub(crate) const COMMAND_TAG_APPLY_INPUT: u8 = 0x14;
}
/// Maximum number of events in one [`Command::ApplyInput`] batch.
pub const MAX_APPLY_INPUT_EVENTS: usize = 256;
/// Maximum encoded bytes in the nested [`Command::ApplyInput`] command body.
pub const MAX_APPLY_INPUT_COMMAND_BODY: usize = 64 * 1024;
wire_tags! { Command;
pub(crate) const COMMAND_TAG_PUT_FILE: u8 = 0x15;
pub(crate) const COMMAND_TAG_GET_FILE: u8 = 0x3b;
pub(crate) const COMMAND_TAG_CHECKOUT: u8 = 0x3c;
pub(crate) const COMMAND_TAG_SERVICE_HTTP_OPEN: u8 = 0x3d;
pub(crate) const COMMAND_TAG_SERVICE_HTTP_READ: u8 = 0x3e;
pub(crate) const COMMAND_TAG_SERVICE_HTTP_CLOSE: u8 = 0x3f;
/// Wire tag for [`Command::Forward`] carrying `ForwardOp::Open`.
pub(crate) const COMMAND_TAG_FORWARD_OPEN: u8 = 0x44;
/// Wire tag for [`Command::Forward`] carrying `ForwardOp::Read`.
pub(crate) const COMMAND_TAG_FORWARD_READ: u8 = 0x45;
/// Wire tag for [`Command::Forward`] carrying `ForwardOp::Write`.
pub(crate) const COMMAND_TAG_FORWARD_WRITE: u8 = 0x46;
/// Wire tag for [`Command::Forward`] carrying `ForwardOp::Close`.
pub(crate) const COMMAND_TAG_FORWARD_CLOSE: u8 = 0x47;
/// Wire tag for [`Command::Forward`] carrying `ForwardOp::AgentOffer`.
pub(crate) const COMMAND_TAG_AGENT_OFFER: u8 = 0x4f;
/// Wire tag for [`Command::Forward`] carrying `ForwardOp::AgentReply`.
pub(crate) const COMMAND_TAG_AGENT_REPLY: u8 = 0x50;
/// Wire tag for [`Command::OpenUrl`] carrying `OpenUrlOp::Request`.
pub(crate) const COMMAND_TAG_OPEN_URL: u8 = 0x51;
/// Wire tag for [`Command::OpenUrl`] carrying `OpenUrlOp::Reply`.
pub(crate) const COMMAND_TAG_OPEN_URL_REPLY: u8 = 0x52;
pub(crate) const COMMAND_TAG_DESCRIBE_API: u8 = 0x40;
pub(crate) const COMMAND_TAG_GET_CREATION: u8 = 0x41;
pub(crate) const COMMAND_TAG_SHUTDOWN: u8 = 0x16;
pub(crate) const COMMAND_TAG_REPORT_AGENT_STATE: u8 = 0x17;
pub(crate) const COMMAND_TAG_GET_PERF: u8 = 0x18;
pub(crate) const COMMAND_TAG_PERF_CONTROL: u8 = 0x39;
/// Wire tag for [`Command::SessionSetProject`]: an organization command gated
/// on `features_ext.SESSION_PROJECT`.
pub(crate) const COMMAND_TAG_SESSION_SET_PROJECT: u8 = 0x3a;
pub(crate) const COMMAND_TAG_TRANSCRIBE: u8 = 0x19;
pub(crate) const COMMAND_TAG_APPEND_RESOURCE_OUTPUT: u8 = 0x1a;
pub(crate) const COMMAND_TAG_PUBLISH_RESOURCE_STATE: u8 = 0x42;
pub(crate) const COMMAND_TAG_SEND_DOCUMENT_INPUT: u8 = 0x48;
pub(crate) const COMMAND_TAG_MIRROR: u8 = 0x4a;
pub(crate) const COMMAND_TAG_MIRROR_PROVIDE: u8 = 0x4b;
pub(crate) const COMMAND_TAG_HOST_CHECKLIST: u8 = 0x43;
pub(crate) const COMMAND_TAG_PRIVACY_STATUS: u8 = 0x4c;
/// Wire tag for [`Command::Credentials`].
pub(crate) const COMMAND_TAG_CREDENTIALS: u8 = 0x4e;
/// Wire tag for [`Command::KillResourceIf`]: a new tag rather than a field on
/// `KILL_RESOURCE`, so a peer without `CONDITIONAL_KILL` fails to decode it
/// instead of killing unconditionally (ADR-0109).
pub(crate) const COMMAND_TAG_KILL_RESOURCE_IF: u8 = 0x1b;
pub(crate) const COMMAND_TAG_OPEN_LISTENER: u8 = 0x1c;
pub(crate) const COMMAND_TAG_CLOSE_TAB_RESOURCES: u8 = 0x1d;
pub(crate) const COMMAND_TAG_NATIVE_AGENT: u8 = 0x38;
}

// Organization commands (`docs/organization.md`), contiguous from `0x1e`;
// `0x2f`..=`0x34` are reserved for the workspace commands
// (`docs/protocol.md#changing-the-wire`).
wire_tags! { Command;
pub(crate) const COMMAND_TAG_GET_ORGANIZATION: u8 = 0x1e;
pub(crate) const COMMAND_TAG_SESSION_CREATE: u8 = 0x1f;
pub(crate) const COMMAND_TAG_SESSION_RENAME: u8 = 0x20;
pub(crate) const COMMAND_TAG_SESSION_SET_LIFETIME: u8 = 0x21;
pub(crate) const COMMAND_TAG_SESSION_DELETE: u8 = 0x22;
pub(crate) const COMMAND_TAG_RESOURCE_CREATE: u8 = 0x23;
pub(crate) const COMMAND_TAG_RESOURCE_MOVE: u8 = 0x24;
pub(crate) const COMMAND_TAG_RESOURCE_MOVE_MANY: u8 = 0x25;
pub(crate) const COMMAND_TAG_RESOURCE_RENAME: u8 = 0x26;
pub(crate) const COMMAND_TAG_RESOURCE_TERMINATE: u8 = 0x27;
pub(crate) const COMMAND_TAG_RESOURCE_REMOVE: u8 = 0x28;
pub(crate) const COMMAND_TAG_RESOURCE_RELAUNCH: u8 = 0x29;
pub(crate) const COMMAND_TAG_INBOX_RESOLVE: u8 = 0x2b;
pub(crate) const COMMAND_TAG_ORGANIZATION_UNDO: u8 = 0x2c;
pub(crate) const COMMAND_TAG_ARRANGEMENT_GET: u8 = 0x2d;
pub(crate) const COMMAND_TAG_ARRANGEMENT_REPLACE: u8 = 0x2e;
}

// Delegated agent credentials (`docs/substrate.md`, ADR-0163), after the
// reserved workspace block. Not organization commands: they are served
// whatever `ORGANIZATION` says, and answer with ordinary results.
wire_tags! { Command;
pub(crate) const COMMAND_TAG_DELEGATE_AGENT: u8 = 0x35;
pub(crate) const COMMAND_TAG_REVOKE_DELEGATION: u8 = 0x36;
}

// `SESSION_KILL` (`docs/organization.md`), an organization command gated on
// `features_ext.SESSION_KILL`, after the delegation pair.
wire_tags! { Command;
pub(crate) const COMMAND_TAG_SESSION_KILL: u8 = 0x37;
}

// `ARRANGEMENT_LOCK` (`docs/organization.md`), an organization command gated
// on `features_ext.TAB_LOCK`, and `SESSION_ENSURE`, gated on
// `features_ext2.SESSION_ENSURE` (after `OPEN_URL`'s 0x51/0x52 pair).
wire_tags! { Command;
pub(crate) const COMMAND_TAG_ARRANGEMENT_LOCK: u8 = 0x4d;
pub(crate) const COMMAND_TAG_SESSION_ENSURE: u8 = 0x53;
}

// `InputEvent` tags (`ROUTE_INPUT`), mirroring the `INPUT_*` frame atoms.
wire_tags! { InputEvent;
pub(crate) const INPUT_EVENT_TAG_KEY: u8 = 0x00;
pub(crate) const INPUT_EVENT_TAG_MOUSE: u8 = 0x01;
pub(crate) const INPUT_EVENT_TAG_FOCUS: u8 = 0x02;
pub(crate) const INPUT_EVENT_TAG_PASTE: u8 = 0x03;
}
// 0x04 (`Selection`) was removed by ADR-0030.

// `StateScope` tags.
wire_tags! { StateScope;
pub(crate) const STATE_SCOPE_TAG_SERVER: u8 = 0x00;
}

// Wire tags for the `CommandResult` tagged union.
wire_tags! { CommandResult;
pub(crate) const COMMAND_RESULT_TAG_OK: u8 = 0x00;
pub(crate) const COMMAND_RESULT_TAG_OK_WITH: u8 = 0x01;
pub(crate) const COMMAND_RESULT_TAG_ERROR: u8 = 0x02;
/// Wire tag for [`CommandResult::Refused`]; sent only in reply to an
/// organization command.
pub(crate) const COMMAND_RESULT_TAG_REFUSED: u8 = 0x03;
/// Wire tag for [`CommandResult::Accepted`]; sent only in reply to an
/// organization command.
pub(crate) const COMMAND_RESULT_TAG_ACCEPTED: u8 = 0x04;
}

// Wire tags for the `CommandValue` tagged union.
wire_tags! { CommandValue;
pub(crate) const COMMAND_VALUE_TAG_RESOURCE_ID: u8 = 0x00;
pub(crate) const COMMAND_VALUE_TAG_GROUP_ID: u8 = 0x01;
pub(crate) const COMMAND_VALUE_TAG_STATE: u8 = 0x02;
pub(crate) const COMMAND_VALUE_TAG_JSON: u8 = 0x03;
pub(crate) const COMMAND_VALUE_TAG_BYTES: u8 = 0x04;
pub(crate) const COMMAND_VALUE_TAG_FILE_UPLOAD: u8 = 0x05;
pub(crate) const COMMAND_VALUE_TAG_ORGANIZATION_PAGE: u8 = 0x06;
pub(crate) const COMMAND_VALUE_TAG_ARRANGEMENT: u8 = 0x07;
pub(crate) const COMMAND_VALUE_TAG_COMMITTED: u8 = 0x08;
pub(crate) const COMMAND_VALUE_TAG_DELEGATION: u8 = 0x09;
pub(crate) const COMMAND_VALUE_TAG_FILE_CHUNK: u8 = 0x0a;
pub(crate) const COMMAND_VALUE_TAG_CHECKOUT: u8 = 0x0b;
pub(crate) const COMMAND_VALUE_TAG_SERVICE_HTTP_HEAD: u8 = 0x0c;
pub(crate) const COMMAND_VALUE_TAG_SERVICE_HTTP_CHUNK: u8 = 0x0d;
/// One-use, redacted credential values.
pub(crate) const COMMAND_VALUE_TAG_CREDENTIAL_SECRETS: u8 = 0x0e;
pub(crate) const COMMAND_VALUE_TAG_FORWARD_OPENED: u8 = 0x0f;
pub(crate) const COMMAND_VALUE_TAG_FORWARD_CHUNK: u8 = 0x10;
}

// Wire tags for the `AttachTarget` tagged union.
wire_tags! { AttachTarget;
pub(crate) const ATTACH_TARGET_LAST: u8 = 0;
pub(crate) const ATTACH_TARGET_BY_NAME: u8 = 1;
pub(crate) const ATTACH_TARGET_BY_ID: u8 = 2;
pub(crate) const ATTACH_TARGET_CREATE_IF_MISSING: u8 = 3;
}
