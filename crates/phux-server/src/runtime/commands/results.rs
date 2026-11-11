//! Command results and attribution every app and the host share.

use phux_protocol::wire::frame::{CommandResult, ErrorCode};
use tokio::sync::oneshot;

use crate::resource::WrongResourceKind;
use crate::state::ClientId;

/// The command-result shape of a Terminal-only request aimed at a resource
/// of another kind.
pub(crate) fn wrong_resource_kind(error: WrongResourceKind) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::WrongResourceKind,
        message: error.to_string(),
    }
}

/// The `TERMINAL_NOT_FOUND` refusal for `terminal_id`.
pub(in crate::runtime) fn terminal_not_found(
    terminal_id: &phux_protocol::ids::ResourceId,
) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::TerminalNotFound,
        message: format!("no such terminal: {terminal_id}"),
    }
}

/// The command result of a pane actor's `Result<(), String>` reply; a
/// refusal carries `refusal_code`.
pub(in crate::runtime) fn actor_reply(
    reply: Result<Result<(), String>, oneshot::error::RecvError>,
    refusal_code: ErrorCode,
    verb: &str,
) -> CommandResult {
    match reply {
        Ok(Ok(())) => CommandResult::Ok,
        Ok(Err(message)) => CommandResult::Error {
            code: refusal_code,
            message,
        },
        Err(_) => CommandResult::Error {
            code: ErrorCode::InternalError,
            message: format!("pane actor dropped {verb} reply"),
        },
    }
}

/// Why an `ATTACH_RESOURCE` stage failed; `AttachResourceSession::failed`
/// rolls the partial generation back exactly once.
#[derive(Debug)]
pub(crate) struct AttachResourceFailure {
    /// Wire error code the caller receives.
    pub(crate) code: ErrorCode,
    /// Human-readable explanation attached to that code.
    pub(crate) message: String,
}

impl AttachResourceFailure {
    /// An internal fault with a fixed explanation — the dominant shape.
    pub(in crate::runtime) fn internal(message: &str) -> Self {
        Self {
            code: ErrorCode::InternalError,
            message: message.to_owned(),
        }
    }
}

/// A `SessionSnapshot` describing a server with no sessions: empty lists,
/// sentinel focus ids. Used by `GET_STATE` when the registry is empty.
pub(crate) const fn empty_session_snapshot() -> phux_protocol::wire::info::SessionSnapshot {
    use phux_protocol::ids::{ResourceId, SessionId, WindowId};
    phux_protocol::wire::info::SessionSnapshot::new(
        SessionId::new(0),
        WindowId::new(0),
        ResourceId::local(0),
    )
}

/// Who and what caused a pane's spawn, for its `pane_spawned` stamp
/// (ADR-0123). `Default` is a server-driven spawn, such as a seed pane.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SpawnAttribution {
    /// The connection that asked for the pane.
    pub(crate) actor: Option<ClientId>,
    /// The spawn's idempotency key (ADR-0126).
    pub(crate) operation_id: Option<phux_protocol::ids::IdempotencyKey>,
    /// Seconds the pane is retained after its process exits (ADR-0124);
    /// `None` falls back to `defaults.retain-on-exit`.
    pub(crate) retain_secs: Option<u32>,
}

/// Where a session's seed pane came from: its agent-session provenance and
/// who asked for it (ADR-0123). `Default` is a server-driven seed.
#[derive(Debug, Default)]
pub(crate) struct SeedOrigin {
    /// Opaque native agent-session provenance to install on the pane.
    pub(crate) agent_session: Option<Vec<u8>>,
    /// The seed pane's `pane_spawned` attribution.
    pub(crate) attribution: SpawnAttribution,
    /// The new session's lifetime; a named create is persistent.
    pub(crate) lifetime: phux_core::SessionLifetime,
}
