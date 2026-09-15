//! Wire primitives for `phux kill`.
//!
//! Covers `SHUTDOWN`, `KILL_RESOURCES` (a whole session in one round trip),
//! `KILL_RESOURCE` (one Terminal), and the keep-empty-session clear that
//! lets the server remove an emptied session (ADR-0105).
//!
//! Selector resolution (which Terminals a target names) and the choice
//! between the whole-session and per-pane paths stay client-side — see
//! `crates/phux/src/commands/kill.rs`.

use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, SESSION_KEEP_EMPTY_KEY, Scope,
    encode_session_keep_empty,
};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::Degradation;

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

/// Send `SHUTDOWN` and classify the reply.
///
/// A clean disconnect right after the request is the server tearing itself
/// down having already acknowledged: it surfaces as
/// [`AttachError::Disconnected`], which callers treat the same as
/// [`ShutdownOutcome::Ok`] (see `phux kill --server`).
///
/// The paired [`Degradation`] carries any uncorrelated `ERROR` the server
/// interleaved ahead of the reply (a hub's per-satellite unreachability
/// notice) — the caller prints it exactly as
/// `crate::commands::command_on` used to, via [`Degradation::notices`].
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
    #[must_use]
    pub fn from_result(result: CommandResult) -> Self {
        match result {
            CommandResult::Ok => Self::Killed,
            CommandResult::Error { message, .. } => Self::Refused(message),
            other => Self::Unexpected(other),
        }
    }
}

/// Kill every Terminal in `ids` in one round trip — the atomic multi-terminal
/// op a whole-session target rides (the v0.3.0 "Option B" re-tier's
/// replacement for the dissolved `KILL_COLLECTION` verb).
///
/// The paired [`Degradation`] is any uncorrelated `ERROR` interleaved ahead
/// of the reply; print its notices exactly as `command_on` used to.
///
/// # Errors
///
/// Transport and decode failures from [`Connection::request`].
pub async fn kill_resources(
    conn: &mut Connection,
    request_id: u32,
    ids: Vec<ResourceId>,
) -> Result<(KillOutcome, Degradation), AttachError> {
    let (result, interleaved) = conn
        .request(request_id, Command::KillResources { ids })
        .await?
        .into_parts();
    Ok((
        KillOutcome::from_result(result),
        Degradation::from_interleaved(&interleaved),
    ))
}

/// Kill exactly one Terminal.
///
/// The paired [`Degradation`] is any uncorrelated `ERROR` interleaved ahead
/// of the reply; print its notices exactly as `command_on` used to.
///
/// # Errors
///
/// Transport and decode failures from [`Connection::request`].
pub async fn kill_resource(
    conn: &mut Connection,
    request_id: u32,
    terminal_id: ResourceId,
) -> Result<(KillOutcome, Degradation), AttachError> {
    let (result, interleaved) = conn
        .request(request_id, Command::KillResource { terminal_id })
        .await?
        .into_parts();
    Ok((
        KillOutcome::from_result(result),
        Degradation::from_interleaved(&interleaved),
    ))
}

/// Clear a session's keep-empty mark (ADR-0105).
///
/// An empty session has no pane for `KILL_RESOURCES` to name, so `phux kill`
/// on one clears the mark instead, which is what makes the server remove a
/// session holding no windows. Fire-and-forget like every `SET_METADATA`
/// write: the caller confirms with a following `GET_STATE` on the same
/// ordered connection.
///
/// # Errors
///
/// Transport failures from [`Connection::send`].
pub async fn clear_session_keep_empty(
    conn: &mut Connection,
    request_id: u32,
    session_name: &str,
) -> Result<(), AttachError> {
    conn.send(&FrameKind::SetMetadata {
        request_id,
        scope: Scope::Global,
        key: SESSION_KEEP_EMPTY_KEY.to_owned(),
        value: encode_session_keep_empty(session_name, false),
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_outcome_carries_the_code_on_refusal() {
        assert_eq!(
            ShutdownOutcome::from_result(CommandResult::Ok),
            ShutdownOutcome::Ok
        );
        assert_eq!(
            ShutdownOutcome::from_result(CommandResult::Error {
                code: ErrorCode::PermissionDenied,
                message: "no".to_owned(),
            }),
            ShutdownOutcome::Refused {
                code: ErrorCode::PermissionDenied,
                message: "no".to_owned(),
            }
        );
    }

    #[test]
    fn kill_outcome_discards_the_code_on_refusal() {
        assert_eq!(
            KillOutcome::from_result(CommandResult::Ok),
            KillOutcome::Killed
        );
        assert_eq!(
            KillOutcome::from_result(CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                message: "gone".to_owned(),
            }),
            KillOutcome::Refused("gone".to_owned())
        );
        assert!(matches!(
            KillOutcome::from_result(CommandResult::OkWith(
                phux_protocol::wire::frame::CommandValue::Json("x".to_owned())
            )),
            KillOutcome::Unexpected(_)
        ));
    }

    #[tokio::test]
    async fn kill_resources_kill_resource_and_the_keep_empty_clear_send_the_expected_frames() {
        use crate::attach::connection::Connection;
        use crate::testkit::{ScriptSpec, ScriptedServer};
        use phux_protocol::wire::frame::{FrameKind, Scope};

        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
        let server =
            tokio::spawn(async move { ScriptedServer::accept(&listener, ScriptSpec::new()).await });

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let ids = vec![ResourceId::local(1), ResourceId::local(2)];
        let (outcome, degradation) = kill_resources(&mut conn, 1, ids.clone())
            .await
            .expect("scripted server");
        assert_eq!(outcome, KillOutcome::Killed);
        assert!(degradation.is_complete());
        let (outcome, degradation) = kill_resource(&mut conn, 2, ResourceId::local(3))
            .await
            .expect("scripted server");
        assert_eq!(outcome, KillOutcome::Killed);
        assert!(degradation.is_complete());
        clear_session_keep_empty(&mut conn, 3, "work")
            .await
            .expect("fire-and-forget send");
        drop(conn);
        let seen = server.await.expect("scripted server task");

        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::KillResources { ids: seen_ids },
                    ..
                } if *seen_ids == ids
            )),
            "expected KILL_RESOURCES{{ids}}; sent {seen:?}"
        );
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::KillResource { terminal_id },
                    ..
                } if *terminal_id == ResourceId::local(3)
            )),
            "expected KILL_RESOURCE{{terminal_id: @3}}; sent {seen:?}"
        );
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::SetMetadata { scope: Scope::Global, key, .. }
                    if key == SESSION_KEEP_EMPTY_KEY
            )),
            "expected the keep-empty SET_METADATA; sent {seen:?}"
        );
    }

    /// An uncorrelated `ERROR` interleaved ahead of the reply — a hub's
    /// per-satellite unreachability notice — must still reach the caller
    /// through the returned `Degradation`, for every verb in this module.
    /// This is what lets the CLI print `phux: warning: partial results —
    /// {message}` exactly as `command_on` used to, even though these
    /// functions no longer route through it.
    #[tokio::test]
    async fn an_interleaved_degradation_notice_survives_every_verb() {
        use crate::attach::connection::Connection;
        use crate::testkit::{ScriptSpec, ScriptedServer};

        const NOTICE: &str = "satellite build-box is unreachable: link is down";

        async fn scripted() -> (
            Connection,
            tokio::task::JoinHandle<Vec<FrameKind>>,
            tempfile::TempDir,
        ) {
            let dir = tempfile::tempdir().expect("temp dir");
            let socket = dir.path().join("phux.sock");
            let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
            listener.set_nonblocking(true).expect("nonblocking");
            let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
            let server = tokio::spawn(async move {
                ScriptedServer::accept(&listener, ScriptSpec::new().degradation_notice(NOTICE))
                    .await
            });
            let conn = Connection::connect(&socket).await.expect("connect");
            (conn, server, dir)
        }

        let (mut conn, server, _dir) = scripted().await;
        let (outcome, degradation) = shutdown(&mut conn, 1).await.expect("scripted server");
        assert_eq!(outcome, ShutdownOutcome::Ok);
        assert_eq!(degradation.notices(), [NOTICE.to_owned()]);
        drop(conn);
        server.await.expect("scripted server task");

        let (mut conn, server, _dir) = scripted().await;
        let (outcome, degradation) = kill_resources(&mut conn, 1, vec![ResourceId::local(1)])
            .await
            .expect("scripted server");
        assert_eq!(outcome, KillOutcome::Killed);
        assert_eq!(degradation.notices(), [NOTICE.to_owned()]);
        drop(conn);
        server.await.expect("scripted server task");

        let (mut conn, server, _dir) = scripted().await;
        let (outcome, degradation) = kill_resource(&mut conn, 1, ResourceId::local(1))
            .await
            .expect("scripted server");
        assert_eq!(outcome, KillOutcome::Killed);
        assert_eq!(degradation.notices(), [NOTICE.to_owned()]);
        drop(conn);
        server.await.expect("scripted server task");
    }
}
