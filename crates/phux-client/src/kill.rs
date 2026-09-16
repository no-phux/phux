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
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, SESSION_KEEP_EMPTY_KEY, Scope,
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
    ///
    /// A `KILL_RESOURCES` that named a satellite id is answered with one
    /// outcome per id (`docs/spec/L1.md` §5.2): every id killed is
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
/// That is `KEYED_SIGNAL` (`docs/spec/L1.md` §5.1.1). An older server ignores
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
/// nothing, and the reply names an outcome per id (`docs/spec/L1.md` §5.1.1,
/// §5.2).
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
/// nothing (`docs/spec/L1.md` §5.1.1).
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

    /// A per-id `KILL_RESOURCES` outcome is `Killed` only when no id failed,
    /// and a refusal names each id that did.
    #[test]
    fn a_merged_kill_outcome_names_every_id_that_was_not_killed() {
        let all = r#"{"schema_version":1,"killed":["@1","devbox/@2"],"not_found":[],"failed":[]}"#;
        assert_eq!(
            KillOutcome::from_result(CommandResult::OkWith(CommandValue::Json(all.to_owned()))),
            KillOutcome::Killed
        );
        let partial = r#"{"schema_version":1,"killed":["@1"],"not_found":[],
            "failed":[{"id":"devbox/@2","code":107,"message":"link is down"}]}"#;
        assert_eq!(
            KillOutcome::from_result(CommandResult::OkWith(CommandValue::Json(
                partial.to_owned()
            ))),
            KillOutcome::Refused("not killed: devbox/@2: link is down".to_owned())
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
                    command: Command::KillResources { ids: seen_ids, .. },
                    ..
                } if *seen_ids == ids
            )),
            "expected KILL_RESOURCES{{ids}}; sent {seen:?}"
        );
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::KillResource { terminal_id, .. },
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

    /// A keyed kill is refused, with nothing sent, by a server that does not
    /// advertise `KEYED_SIGNAL`; one that does receives the key.
    #[tokio::test]
    async fn a_keyed_kill_needs_keyed_signal_and_carries_its_key() {
        use crate::attach::connection::Connection;
        use crate::testkit::{ScriptSpec, ScriptedServer};
        use phux_protocol::caps::ServerFeatureSet;

        async fn run(spec: ScriptSpec) -> (Result<(), KeyedError>, Vec<FrameKind>) {
            let dir = tempfile::tempdir().expect("temp dir");
            let socket = dir.path().join("phux.sock");
            let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
            listener.set_nonblocking(true).expect("nonblocking");
            let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
            let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
            let mut conn = Connection::connect(&socket).await.expect("connect");
            let key = IdempotencyKey::new([7; 16]).expect("non-zero");
            let sent = kill_resource_keyed(&mut conn, 1, ResourceId::local(3), key)
                .await
                .map(|_| ());
            drop(conn);
            (sent, server.await.expect("scripted server task"))
        }

        let (sent, seen) = run(ScriptSpec::new()).await;
        assert!(matches!(sent, Err(KeyedError::Unsupported)));
        assert!(
            !seen
                .iter()
                .any(|frame| matches!(frame, FrameKind::Command { .. })),
            "nothing was sent to a server without the bit: {seen:?}"
        );

        let (sent, seen) = run(ScriptSpec::new()
            .server_features(ServerFeatureSet::with(&[ServerFeature::KeyedSignal])))
        .await;
        assert!(sent.is_ok());
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::KillResource {
                        operation_id: Some(_),
                        ..
                    },
                    ..
                }
            )),
            "the keyed kill carries its key: {seen:?}"
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
