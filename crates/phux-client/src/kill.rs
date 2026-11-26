//! `phux kill` — wire primitives and the shared verb orchestration.
//!
//! Covers `SHUTDOWN`, `KILL_RESOURCE` (one Terminal), and killing a whole
//! session with one `SESSION_KILL` ([`kill_session`]): the server ends every
//! member and deletes the session.
//!
//! [`selected`] is the selector resolution and whole-session / per-pane
//! choice both `phux kill` and MCP `phux_kill` call, so those surfaces
//! cannot drift.

use phux_protocol::ResourceId;
use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::Degradation;

mod selection;
mod session;

pub use selection::{KillError, KillIdentity, Selected, selected};
pub use session::kill_session;

/// What a `SHUTDOWN` request answered.
#[derive(Debug, Clone, PartialEq)]
pub enum ShutdownOutcome {
    /// The server acknowledged the stop.
    Ok,
    /// The server refused.
    Refused {
        /// The wire error code.
        code: ErrorCode,
        /// The server's message.
        message: String,
    },
    /// An unexpected reply shape.
    Unexpected(CommandResult),
}

impl ShutdownOutcome {
    /// Classify a `SHUTDOWN` `COMMAND_RESULT`.
    #[must_use]
    pub fn from_result(result: CommandResult) -> Self {
        match result {
            CommandResult::Ok => Self::Ok,
            CommandResult::Error { code, message } => Self::Refused { code, message },
            other => Self::Unexpected(other),
        }
    }
}

/// Send `SHUTDOWN` and classify the reply, with any interleaved notices as
/// the [`Degradation`].
///
/// A disconnect right after the request is the server stopping having
/// acknowledged; callers treat [`AttachError::Disconnected`] as success.
///
/// # Errors
///
/// Transport and decode failures from [`Connection::request`].
pub async fn shutdown(
    conn: &mut Connection,
    request_id: u32,
) -> Result<(ShutdownOutcome, Degradation), AttachError> {
    let (result, interleaved) = conn
        .request(request_id, Command::Shutdown)
        .await?
        .into_parts();
    Ok((
        ShutdownOutcome::from_result(result),
        Degradation::from_interleaved(&interleaved),
    ))
}

/// What a `KILL_RESOURCE` / `KILL_RESOURCES` request answered.
#[derive(Debug, Clone, PartialEq)]
pub enum KillOutcome {
    /// The server accepted close intent; this does not observe process exit.
    Killed,
    /// The server refused.
    Refused(String),
    /// Original per-host result, including successful, absent and failed targets.
    Partial(serde_json::Value),
    /// An unexpected reply shape.
    Unexpected(CommandResult),
}

impl KillOutcome {
    /// Classify a `KILL_RESOURCE`/`KILL_RESOURCES` `COMMAND_RESULT`.
    ///
    /// A federated result retains each host's exact result rather than
    /// reducing a mixed outcome to one refusal.
    #[must_use]
    pub fn from_result(result: CommandResult) -> Self {
        match result {
            CommandResult::Ok => Self::Killed,
            CommandResult::Error { message, .. } => Self::Refused(message),
            CommandResult::OkWith(CommandValue::Json(document)) => {
                match merged_kill_result(&document) {
                    Some((value, false)) => Self::Partial(value),
                    Some((_, true)) => Self::Killed,
                    None => Self::Unexpected(CommandResult::OkWith(CommandValue::Json(document))),
                }
            }
            other => Self::Unexpected(other),
        }
    }
}

/// Validate the federated result while retaining the original document.
fn merged_kill_result(document: &str) -> Option<(serde_json::Value, bool)> {
    let value: serde_json::Value = serde_json::from_str(document).ok()?;
    if value.get("schema_version")?.as_u64()? != 1 {
        return None;
    }
    value.get("killed")?.as_array()?;
    value.get("not_found")?.as_array()?;
    let complete = value.get("failed")?.as_array()?.is_empty();
    Some((value, complete))
}

/// Request a batch kill (for example a whole-session target). The server
/// closes each host's subset separately. Interleaved notices supply the
/// [`Degradation`]. A keyed request reports per-id outcomes.
///
/// # Errors
///
/// Transport and decode failures from [`Connection::request`].
pub async fn kill_resources(
    conn: &mut Connection,
    request_id: u32,
    ids: Vec<ResourceId>,
) -> Result<(KillOutcome, Degradation), AttachError> {
    request_kill(
        conn,
        request_id,
        Command::KillResources {
            ids,
            operation_id: None,
        },
    )
    .await
}

/// [`kill_resources`] under an idempotency key (`phux kill --idempotency-key`).
///
/// A repeat with the same key and ids answers the first result and kills
/// nothing, and the reply names an outcome per id (`docs/protocol.md`).
///
/// # Errors
///
/// Transport and decode failures from [`Connection::request`].
pub async fn kill_resources_keyed(
    conn: &mut Connection,
    request_id: u32,
    ids: Vec<ResourceId>,
    operation_id: IdempotencyKey,
) -> Result<(KillOutcome, Degradation), AttachError> {
    let command = Command::KillResources {
        ids,
        operation_id: Some(operation_id),
    };
    request_kill(conn, request_id, command).await
}

/// Send one kill command and classify its reply.
async fn request_kill(
    conn: &mut Connection,
    request_id: u32,
    command: Command,
) -> Result<(KillOutcome, Degradation), AttachError> {
    let (result, interleaved) = conn.request(request_id, command).await?.into_parts();
    Ok((
        KillOutcome::from_result(result),
        Degradation::from_interleaved(&interleaved),
    ))
}

/// Kill exactly one Terminal, with any interleaved notices as the
/// [`Degradation`].
///
/// # Errors
///
/// Transport and decode failures from [`Connection::request`].
pub async fn kill_resource(
    conn: &mut Connection,
    request_id: u32,
    terminal_id: ResourceId,
) -> Result<(KillOutcome, Degradation), AttachError> {
    request_kill(
        conn,
        request_id,
        Command::KillResource {
            terminal_id,
            operation_id: None,
        },
    )
    .await
}

/// [`kill_resource`] under an idempotency key (`phux kill --idempotency-key`).
///
/// A repeat with the same key and target answers the first result and kills
/// nothing (`docs/protocol.md#receipts-retries-and-backpressure`).
///
/// # Errors
///
/// Transport and decode failures from [`Connection::request`].
pub async fn kill_resource_keyed(
    conn: &mut Connection,
    request_id: u32,
    terminal_id: ResourceId,
    operation_id: IdempotencyKey,
) -> Result<(KillOutcome, Degradation), AttachError> {
    let command = Command::KillResource {
        terminal_id,
        operation_id: Some(operation_id),
    };
    request_kill(conn, request_id, command).await
}

#[cfg(test)]
mod tests;
