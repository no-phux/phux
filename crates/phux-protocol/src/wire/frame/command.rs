//! Control-plane command, result, and agent-event types — SPEC §5
//! (ADR-0021) and SPEC §7.5 (ADR-0022).

use crate::ids::{
    ArrangementKey, FileUploadId, GroupId, InputOperationId, OperationId, ResourceId, ResourceKey,
    ResourceKind, Revision, SessionKey,
};
use crate::input::InputEvent;
use crate::wire::info::SessionSnapshot;

use super::ErrorCode;
#[cfg(doc)]
use super::FrameKind;
use super::org::{
    ArrangementDoc, ArrangementValue, Committed, ExitPolicy, LaunchMode, LaunchSpec, LiveTarget,
    MoveItem, OrgFilter, OrganizationPage, RecordRevision, Refusal, SessionLifetime, TerminateMode,
};

mod args;
mod event;
#[macro_use]
mod organization;
#[macro_use]
mod supervisory;

pub use args::{
    FileChunk, FileUploadAck, GET_SCREEN_FORMAT_SELECTOR_MASK, GET_SCREEN_FORMAT_UNWRAP, InputMode,
    KillConditions, KillPrecondition, ListenerTransport, PerfOp, ReportedAgentState,
    ResourceEventType, StateScope, TerminalSignal,
};
pub use event::{AgentEvent, ApprovalOutcome, ControlAction, ResourceLifecycle};

/// Declares [`Command`] around the variant list the family macros build.
macro_rules! command_enum {
    ($($variants:tt)*) => {
/// A typed control-plane command carried by
/// [`FrameKind::Command`](super::FrameKind::Command) (SPEC §5.1).
///
/// Unknown tags decode as [`DecodeError::UnknownEnumValue`](crate::wire::error::DecodeError::UnknownEnumValue).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Command {
    $($variants)*
}
    };
}

// One file per variant family keeps each source file small; each family
// macro appends its variants, in declaration order, and hands the list on.
supervisory_commands!(organization_commands!(command_enum!()));

impl Command {
    /// Clear an imported session's Intake mark with the existing `0x2b` wire tag.
    #[must_use]
    pub const fn intake_resolve(
        operation: OperationId,
        session: SessionKey,
        expected: Revision,
    ) -> Self {
        Self::InboxResolve {
            operation,
            session,
            expected,
        }
    }

    /// Subscribe to a Terminal content stream (SPEC §5.1).
    ///
    /// Carries no agent cursor. Send a non-default `role_policy` only to a
    /// server advertising `ATTACH_ROLES`. `None` is `{ PRIMARY, NEVER }` and
    /// writes no role byte.
    #[must_use]
    pub const fn attach_terminal(
        resource_id: ResourceId,
        role_policy: Option<super::RolePolicy>,
    ) -> Self {
        Self::AttachResource {
            terminal_id: resource_id,
            role_policy,
            agent_after_seq: None,
        }
    }

    /// Subscribe to an `AgentSession` record stream (SPEC §5.1, ADR-0159).
    ///
    /// `after_seq` is the last committed record, or `None` for a full
    /// bootstrap. Send a cursor only when the server advertises
    /// `AgentStreamResume`; `None` keeps the no-cursor bytes. A Terminal
    /// rejects a cursor.
    #[must_use]
    pub const fn attach_agent_session(
        resource_id: ResourceId,
        role_policy: Option<super::RolePolicy>,
        after_seq: Option<u64>,
    ) -> Self {
        Self::AttachResource {
            terminal_id: resource_id,
            role_policy,
            agent_after_seq: after_seq,
        }
    }

    /// The operation id of a keyed supervisory command
    /// (`docs/spec/L1.md` §5.1.1) or of a mutating organization command
    /// (§5.7), or `None`.
    #[must_use]
    pub const fn idempotency_key(&self) -> Option<&crate::ids::IdempotencyKey> {
        match self {
            Self::KillResource { operation_id, .. }
            | Self::KillResourceIf { operation_id, .. }
            | Self::KillResources { operation_id, .. }
            | Self::SignalTerminal { operation_id, .. } => operation_id.as_ref(),
            Self::SessionCreate { operation, .. }
            | Self::SessionRename { operation, .. }
            | Self::SessionSetProject { operation, .. }
            | Self::SessionSetLifetime { operation, .. }
            | Self::SessionDelete { operation, .. }
            | Self::SessionKill { operation, .. }
            | Self::ResourceCreate { operation, .. }
            | Self::ResourceMove { operation, .. }
            | Self::ResourceMoveMany { operation, .. }
            | Self::ResourceRename { operation, .. }
            | Self::ResourceTerminate { operation, .. }
            | Self::ResourceRemove { operation, .. }
            | Self::ResourceRelaunch { operation, .. }
            | Self::InboxResolve { operation, .. }
            | Self::OrganizationUndo { operation, .. }
            | Self::ArrangementReplace { operation, .. } => Some(operation),
            _ => None,
        }
    }

    /// Whether this is an organization command (`docs/spec/L1.md` §5.7):
    /// one a server serves only while it advertises
    /// [`ServerFeatureExt::Organization`](crate::caps::ServerFeatureExt::Organization),
    /// and the only kind a reply may answer with [`CommandResult::Refused`]
    /// or [`CommandResult::Accepted`].
    #[must_use]
    pub const fn is_organization(&self) -> bool {
        matches!(
            self,
            Self::GetOrganization { .. }
                | Self::SessionCreate { .. }
                | Self::SessionRename { .. }
                | Self::SessionSetProject { .. }
                | Self::SessionSetLifetime { .. }
                | Self::SessionDelete { .. }
                | Self::SessionKill { .. }
                | Self::ResourceCreate { .. }
                | Self::ResourceMove { .. }
                | Self::ResourceMoveMany { .. }
                | Self::ResourceRename { .. }
                | Self::ResourceTerminate { .. }
                | Self::ResourceRemove { .. }
                | Self::ResourceRelaunch { .. }
                | Self::InboxResolve { .. }
                | Self::OrganizationUndo { .. }
                | Self::ArrangementGet { .. }
                | Self::ArrangementReplace { .. }
        )
    }
}

/// A successful command's payload (SPEC §5, `CommandValue`).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum CommandValue {
    /// A Terminal identifier (e.g. the result of a spawn).
    ResourceId(ResourceId),
    /// A Group identifier (opaque grouping key).
    GroupId(GroupId),
    /// A server-state snapshot (reply to `GET_STATE`). Reuses the
    /// `ATTACHED` snapshot shape — see the wire-bytes note in SPEC §7.
    State(SessionSnapshot),
    /// A structured JSON return, for commands whose result is open-shaped.
    Json(String),
    /// Opaque bytes (e.g. an L3 metadata value).
    Bytes(Vec<u8>),
    /// Acknowledgement for a sandboxed file-upload chunk.
    FileUpload(FileUploadAck),
    /// One chunk of a host file (reply to `GET_FILE`).
    FileChunk(FileChunk),
    /// A checkout answer (reply to `CHECKOUT`).
    Checkout(Box<crate::checkout::CheckoutAnswer>),
    /// A host service's response head (reply to `SERVICE_HTTP_OPEN`).
    ServiceHttpHead(crate::service_http::ServiceHttpHead),
    /// Response body bytes (reply to `SERVICE_HTTP_READ`).
    ServiceHttpChunk(crate::service_http::ServiceHttpChunk),
    /// One page of the organization (reply to `GET_ORGANIZATION`).
    OrganizationPage(Box<OrganizationPage>),
    /// An arrangement and its revision (reply to `ARRANGEMENT_GET`).
    Arrangement(Box<ArrangementValue>),
    /// A committed organization edit and its receipt (reply to a mutating
    /// organization command).
    Committed(Box<Committed>),
    /// A minted delegated agent credential (reply to `DELEGATE_AGENT`).
    Delegation(Box<super::Delegation>),
}

/// The outcome of a [`Command`], carried by
/// [`FrameKind::CommandResult`](super::FrameKind::CommandResult) (SPEC §5).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum CommandResult {
    /// The command succeeded and returned no value.
    Ok,
    /// The command succeeded and returned a [`CommandValue`].
    OkWith(CommandValue),
    /// The command failed; carries a structured [`ErrorCode`] and a
    /// human-readable UTF-8 diagnostic.
    Error {
        /// Structured failure code.
        code: ErrorCode,
        /// Human-readable diagnostic (UTF-8; unconstrained otherwise).
        message: String,
    },
    /// An organization command was refused and had no effect. Sent only in
    /// reply to an organization command ([`Command::is_organization`]): an
    /// older decoder fails the whole frame on this tag, so a legacy command,
    /// or a legacy write the server translates, never receives it.
    Refused(Refusal),
    /// An organization command was accepted; its execution effects follow as
    /// events stamped with `operation`. Sent only in reply to an
    /// organization command, like [`Self::Refused`].
    Accepted {
        /// The accepted operation.
        operation: OperationId,
    },
}
