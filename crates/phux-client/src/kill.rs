//! `phux kill` — wire primitives and the shared verb orchestration.
//!
//! Covers `SHUTDOWN`, `KILL_RESOURCE` (one Terminal), and killing a whole
//! session. Against a server that advertises `SESSION_KILL`, killing a
//! session is that one command ([`kill_session`]): the server ends every
//! member and deletes the session. Against an older server it is one atomic
//! `KILL_RESOURCES` of the session's panes.
//!
//! [`selected`] is the selector resolution and whole-session / per-pane
//! choice both `phux kill` and MCP `phux_kill` call, so those surfaces
//! cannot drift.

use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::Degradation;

mod selection;
mod session;

pub use selection::{KillError, Selected, selected};
pub use session::{kill_session, session_kill_supported};

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
    /// The server killed the target(s).
    Killed,
    /// The server refused.
    Refused(String),
    /// An unexpected reply shape.
    Unexpected(CommandResult),
}

impl KillOutcome {
    /// Classify a `KILL_RESOURCE`/`KILL_RESOURCES` `COMMAND_RESULT`.
    ///
    /// A `KILL_RESOURCES` that named a satellite id is answered with one
    /// outcome per id (`docs/protocol.md#commands-events-resources`): every id killed is
    /// [`Self::Killed`], and any id a host refused or could not be reached
    /// for makes it [`Self::Refused`] naming each such id.
    #[must_use]
    pub fn from_result(result: CommandResult) -> Self {
        match result {
            CommandResult::Ok => Self::Killed,
            CommandResult::Error { message, .. } => Self::Refused(message),
            CommandResult::OkWith(CommandValue::Json(document)) => {
                match merged_kill_failures(&document) {
                    Some(failures) if failures.is_empty() => Self::Killed,
                    Some(failures) => Self::Refused(format!("not killed: {}", failures.join("; "))),
                    None => Self::Unexpected(CommandResult::OkWith(CommandValue::Json(document))),
                }
            }
            other => Self::Unexpected(other),
        }
    }
}

/// The failures a per-id `KILL_RESOURCES` outcome lists, one `"id: reason"`
/// each; `None` when `document` is not such an outcome.
fn merged_kill_failures(document: &str) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(document).ok()?;
    value.get("schema_version")?;
    let failed = value.get("failed")?.as_array()?;
    Some(
        failed
            .iter()
            .map(|entry| {
                let id = entry
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?");
                let message = entry
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("refused");
                format!("{id}: {message}")
            })
            .collect(),
    )
}

/// Why a keyed kill or signal was not answered.
#[derive(Debug, thiserror::Error)]
pub enum KeyedError {
    /// The server does not advertise `KEYED_SIGNAL`, so it would ignore the
    /// key and run a retry again. Nothing was sent.
    #[error(
        "the server does not advertise KEYED_SIGNAL, so it would run a keyed retry again; \
         nothing was sent"
    )]
    Unsupported,
    /// Transport and decode failures from [`Connection::request`].
    #[error(transparent)]
    Attach(#[from] AttachError),
}

/// Whether the server behind `conn` honors a keyed kill or signal.
///
/// That is `KEYED_SIGNAL` (`docs/protocol.md`). An older server ignores
/// the key and runs a retry again, so a caller refuses a keyed request to it
/// rather than send one.
#[must_use]
pub fn keyed_signal_supported(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features
            .contains(ServerFeature::KeyedSignal)
    })
}

/// Kill every Terminal in `ids` in one atomic round trip (a whole-session
/// target), with any interleaved notices as the [`Degradation`].
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
/// [`KeyedError::Unsupported`], with nothing sent, when the server does not
/// advertise `KEYED_SIGNAL`; transport and decode failures otherwise.
pub async fn kill_resources_keyed(
    conn: &mut Connection,
    request_id: u32,
    ids: Vec<ResourceId>,
    operation_id: IdempotencyKey,
) -> Result<(KillOutcome, Degradation), KeyedError> {
    require_keyed_signal(conn)?;
    let command = Command::KillResources {
        ids,
        operation_id: Some(operation_id),
    };
    Ok(request_kill(conn, request_id, command).await?)
}

/// Refuse a keyed request to a server that would run its retry again.
fn require_keyed_signal(conn: &Connection) -> Result<(), KeyedError> {
    if keyed_signal_supported(conn) {
        Ok(())
    } else {
        Err(KeyedError::Unsupported)
    }
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
/// [`KeyedError::Unsupported`], with nothing sent, when the server does not
/// advertise `KEYED_SIGNAL`; transport and decode failures otherwise.
pub async fn kill_resource_keyed(
    conn: &mut Connection,
    request_id: u32,
    terminal_id: ResourceId,
    operation_id: IdempotencyKey,
) -> Result<(KillOutcome, Degradation), KeyedError> {
    require_keyed_signal(conn)?;
    let command = Command::KillResource {
        terminal_id,
        operation_id: Some(operation_id),
    };
    Ok(request_kill(conn, request_id, command).await?)
}

#[cfg(test)]
mod tests;
