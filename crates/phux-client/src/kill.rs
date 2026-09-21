//! `phux kill` — wire primitives and the shared verb orchestration.
//!
//! Covers `SHUTDOWN`, `KILL_RESOURCES` (a whole session in one round trip),
//! `KILL_RESOURCE` (one Terminal), and the keep-empty-session clear that
//! lets the server remove an emptied session (ADR-0105).
//!
//! [`selected`] is the selector resolution and whole-session / empty-session
//! / per-pane choice both `phux kill` and MCP `phux_kill` call, so those
//! surfaces cannot drift.

use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, SESSION_KEEP_EMPTY_KEY, Scope,
    encode_session_keep_empty,
};
use phux_protocol::wire::info::SessionSnapshot;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::selector::{self, Selector};
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

/// A successful [`selected`] kill.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selected {
    /// Interleaved notices from the kill command(s) themselves (a hub's
    /// per-satellite unreachability on the kill round trip).
    pub interleaved: Vec<String>,
}

/// Why [`selected`] did not kill the target.
#[derive(Debug, thiserror::Error)]
pub enum KillError {
    /// The server does not advertise `KEYED_SIGNAL`, so a keyed retry would
    /// run again. Nothing was sent.
    #[error(
        "the server does not advertise KEYED_SIGNAL, so it would run a keyed retry again; \
         nothing was sent"
    )]
    UnsupportedKeyedSignal,
    /// The selector matched nothing against a complete view, or a named
    /// session is absent (hub-local, so a partial fleet cannot hide it).
    #[error("no such target: {target}")]
    NoSuchTarget {
        /// The selector as the caller typed it.
        target: String,
    },
    /// The selector matched nothing against a partial fleet view: a miss
    /// here does not mean the target is gone.
    #[error(
        "could not resolve '{target}': this server's view of the fleet is incomplete, so a \
         miss here does not mean the target is gone"
    )]
    Unresolved {
        /// The selector as the caller typed it.
        target: String,
        /// What the snapshot could not see.
        degradation: Degradation,
    },
    /// The server refused a whole-target kill.
    #[error("kill refused for {label}: {message}")]
    Refused {
        /// The target label used in the refusal (`session "work"`, `"@1"`).
        label: String,
        /// The server's message.
        message: String,
        /// Interleaved notices from that round trip.
        interleaved: Vec<String>,
    },
    /// The server answered a whole-target kill with an unexpected shape.
    #[error("{label}: {message}")]
    Unexpected {
        /// The target label used in the sentence.
        label: String,
        /// The unexpected-reply sentence from [`crate::explain`].
        message: String,
        /// Interleaved notices from that round trip.
        interleaved: Vec<String>,
    },
    /// One or more per-pane kills were refused; others may have landed.
    #[error("{}", messages.join("\n"))]
    PaneRefusals {
        /// One sentence per refused pane, without a `phux:` prefix.
        messages: Vec<String>,
        /// Interleaved notices collected across the loop.
        interleaved: Vec<String>,
    },
    /// An empty-session clear ran but the server still listed the session.
    #[error("kill refused for session {session:?}: the server kept it")]
    SessionKept {
        /// The session that should have gone.
        session: String,
    },
    /// Transport or decode failure.
    #[error(transparent)]
    Attach(#[from] AttachError),
}

impl KillError {
    /// Interleaved notices from a kill command that still produced an error.
    #[must_use]
    pub fn interleaved(&self) -> &[String] {
        match self {
            Self::Refused { interleaved, .. }
            | Self::Unexpected { interleaved, .. }
            | Self::PaneRefusals { interleaved, .. } => interleaved,
            _ => &[],
        }
    }
}

/// Resolve `selector` against a fresh snapshot on `conn` and kill what it names.
///
/// A whole-session target rides one atomic `KILL_RESOURCES`; an empty
/// session (ADR-0105) clears its keep-empty mark; a window, pane, `@id`, or
/// `#tag` target kills each resolved Terminal with `KILL_RESOURCE`. A clean
/// disconnect after a kill is the server self-exiting once its last session
/// was reaped: success.
///
/// With `key`, the kill is one keyed command (`KILL_RESOURCE` for one
/// Terminal, `KILL_RESOURCES` for several). An `@N` / `host/@N` target is
/// sent as written, without a snapshot lookup, so a retry after the first
/// attempt removed the pane still reaches the server.
///
/// Snapshot partial-view notices for a *hit* are appended to `notices`
/// before the kill runs, so a caller can warn even when the kill then
/// fails. A miss does not append: the [`KillError::Unresolved`] /
/// [`KillError::NoSuchTarget`] is the whole answer.
///
/// # Errors
///
/// [`KillError`] — see its variants.
pub async fn selected(
    conn: &mut Connection,
    selector: &Selector,
    target: &str,
    key: Option<IdempotencyKey>,
    notices: &mut Vec<String>,
) -> Result<Selected, KillError> {
    if key.is_some() && !keyed_signal_supported(conn) {
        return Err(KillError::UnsupportedKeyedSignal);
    }
    if let (Some(key), Some(id)) = (key, explicit_id(selector)) {
        return kill_keyed(conn, target, vec![id], key).await;
    }
    let (snapshot, degradation) = crate::state::get_state_on(conn).await?.into_parts();
    if let Some(session) = selector::whole_session_name(selector, &snapshot) {
        let ids = selector::resolve(selector, &snapshot);
        if ids.is_empty() && session_is_empty(&snapshot, &session) {
            return kill_empty_session(conn, &session).await;
        }
        if ids.is_empty() {
            return Err(KillError::NoSuchTarget {
                target: target.to_owned(),
            });
        }
        notices.extend(degradation.notices().iter().cloned());
        return kill_whole_session(conn, &session, ids, key).await;
    }
    let terminals = resolve_terminals(conn, selector, &snapshot).await;
    if terminals.is_empty() {
        return Err(target_miss(target, degradation));
    }
    notices.extend(degradation.notices().iter().cloned());
    match key {
        Some(key) => kill_keyed(conn, target, terminals, key).await,
        None => kill_each_terminal(conn, terminals).await,
    }
}

/// The one id an explicit `@N` / `host/@N` target names.
fn explicit_id(selector: &Selector) -> Option<ResourceId> {
    match selector {
        Selector::ResourceId(id) => Some(ResourceId::local(*id)),
        Selector::SatelliteResourceId { host, id } => {
            Some(ResourceId::satellite(host.as_str(), *id))
        }
        _ => None,
    }
}

/// A miss, told apart so a partial fleet view never claims the target is
/// gone: `#tag` and `@id` search the pane list a hub aggregates.
fn target_miss(target: &str, degradation: Degradation) -> KillError {
    if degradation.is_complete() {
        KillError::NoSuchTarget {
            target: target.to_owned(),
        }
    } else {
        KillError::Unresolved {
            target: target.to_owned(),
            degradation,
        }
    }
}

/// The Terminals a non-session selector names. A `#tag` selector resolves
/// against L3 tag metadata fetched on this same connection; every other form
/// is pure snapshot resolution.
async fn resolve_terminals(
    conn: &mut Connection,
    selector: &Selector,
    snapshot: &SessionSnapshot,
) -> Vec<ResourceId> {
    if matches!(selector, Selector::Tag(_)) {
        let index = crate::state::fetch_tag_index(conn, snapshot).await;
        return selector::resolve_with_tags(selector, snapshot, &index);
    }
    selector::resolve(selector, snapshot)
}

/// Whether the session named `name` holds no windows (ADR-0105).
fn session_is_empty(snapshot: &SessionSnapshot, name: &str) -> bool {
    snapshot
        .sessions
        .iter()
        .any(|session| session.name == name && session.is_empty())
}

/// Kill an empty session (ADR-0105): clear its keep-empty mark, then confirm
/// with a `GET_STATE` on the same ordered connection that it is gone. A
/// disconnect in its place is the server self-exiting after its last
/// session went, which is success.
async fn kill_empty_session(conn: &mut Connection, session: &str) -> Result<Selected, KillError> {
    clear_session_keep_empty(conn, 1, session).await?;
    match crate::state::get_state_on(conn).await {
        Ok(view) if view.snapshot().sessions.iter().any(|s| s.name == session) => {
            Err(KillError::SessionKept {
                session: session.to_owned(),
            })
        }
        Ok(_) | Err(AttachError::Disconnected) => Ok(Selected::default()),
        Err(err) => Err(err.into()),
    }
}

/// One `KILL_RESOURCES` for a whole session, keyed when `key` is set.
async fn kill_whole_session(
    conn: &mut Connection,
    session: &str,
    ids: Vec<ResourceId>,
    key: Option<IdempotencyKey>,
) -> Result<Selected, KillError> {
    let reply = match key {
        Some(key) => kill_resources_keyed(conn, 1, ids, key).await,
        None => kill_resources(conn, 1, ids).await.map_err(KeyedError::from),
    };
    batch_outcome(reply, &format!("session {session:?}"))
}

/// Kill `terminals` as one keyed command: `KILL_RESOURCE` for one,
/// `KILL_RESOURCES` for several, so the key names the whole operation.
async fn kill_keyed(
    conn: &mut Connection,
    target: &str,
    mut terminals: Vec<ResourceId>,
    key: IdempotencyKey,
) -> Result<Selected, KillError> {
    let reply = if terminals.len() == 1 {
        let terminal = terminals.remove(0);
        kill_resource_keyed(conn, 1, terminal, key).await
    } else {
        kill_resources_keyed(conn, 1, terminals, key).await
    };
    batch_outcome(reply, &format!("{target:?}"))
}

/// The result of one kill round trip that covered a whole target. A
/// disconnect is the server self-exiting after its last session was reaped,
/// which is success.
fn batch_outcome(
    reply: Result<(KillOutcome, Degradation), KeyedError>,
    label: &str,
) -> Result<Selected, KillError> {
    match reply {
        Ok((KillOutcome::Killed, degradation)) => Ok(Selected {
            interleaved: degradation.notices().to_vec(),
        }),
        Err(KeyedError::Attach(AttachError::Disconnected)) => Ok(Selected::default()),
        Err(KeyedError::Unsupported) => Err(KillError::UnsupportedKeyedSignal),
        Ok((KillOutcome::Refused(message), degradation)) => Err(KillError::Refused {
            label: label.to_owned(),
            message,
            interleaved: degradation.notices().to_vec(),
        }),
        Ok((KillOutcome::Unexpected(other), degradation)) => Err(KillError::Unexpected {
            label: label.to_owned(),
            message: crate::explain::explain_unexpected("kill", &other),
            interleaved: degradation.notices().to_vec(),
        }),
        Err(KeyedError::Attach(err)) => Err(err.into()),
    }
}

/// Kill each Terminal in turn: every refusal is collected rather than
/// stopping the loop, and a disconnect means the remaining targets are
/// already gone.
async fn kill_each_terminal(
    conn: &mut Connection,
    terminals: Vec<ResourceId>,
) -> Result<Selected, KillError> {
    let mut refusals = Vec::new();
    let mut interleaved = Vec::new();
    for (i, terminal) in terminals.into_iter().enumerate() {
        let request_id = u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1);
        match kill_one(conn, request_id, terminal).await {
            KillStep::Killed(notices) => interleaved.extend(notices),
            KillStep::Refused { message, notices } => {
                interleaved.extend(notices);
                refusals.push(message);
            }
            KillStep::ServerGone => break,
        }
    }
    if refusals.is_empty() {
        Ok(Selected { interleaved })
    } else {
        Err(KillError::PaneRefusals {
            messages: refusals,
            interleaved,
        })
    }
}

/// How one `KILL_RESOURCE` ended.
enum KillStep {
    Killed(Vec<String>),
    Refused {
        message: String,
        notices: Vec<String>,
    },
    ServerGone,
}

async fn kill_one(conn: &mut Connection, request_id: u32, terminal: ResourceId) -> KillStep {
    let label = selector::format_terminal_id(&terminal);
    match kill_resource(conn, request_id, terminal).await {
        Ok((KillOutcome::Killed, degradation)) => KillStep::Killed(degradation.notices().to_vec()),
        Ok((KillOutcome::Refused(message), degradation)) => KillStep::Refused {
            message: format!("kill refused for {label}: {message}"),
            notices: degradation.notices().to_vec(),
        },
        Ok((KillOutcome::Unexpected(other), degradation)) => KillStep::Refused {
            message: format!(
                "{label}: {}",
                crate::explain::explain_unexpected("kill", &other)
            ),
            notices: degradation.notices().to_vec(),
        },
        Err(AttachError::Disconnected) => KillStep::ServerGone,
        Err(err) => KillStep::Refused {
            message: format!("kill failed for {label}: {err}"),
            notices: Vec::new(),
        },
    }
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

    fn pane_state() -> phux_protocol::wire::info::SessionSnapshot {
        use phux_protocol::ids::{SessionId, WindowId};
        use phux_protocol::wire::info::{ResourceInfo, SessionInfo, WindowInfo};
        let session = SessionId::new(1);
        let window = WindowId::new(10);
        SessionSnapshot::new(session, window, ResourceId::local(1))
            .with_sessions(vec![SessionInfo::new(session, "work").with_window_count(1)])
            .with_windows(vec![WindowInfo::new(window, session, "shell")])
            .with_resources(vec![
                ResourceInfo::new(ResourceId::local(1), window, 80, 24),
                ResourceInfo::new(ResourceId::local(2), window, 80, 24),
            ])
    }

    async fn run_selected(
        spec: crate::testkit::ScriptSpec,
        target: &str,
    ) -> (Result<Selected, KillError>, Vec<String>, Vec<FrameKind>) {
        use crate::testkit::ScriptedServer;
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let selector = crate::selector::parse(target).expect("selector");
        let mut notices = Vec::new();
        let result = selected(&mut conn, &selector, target, None, &mut notices).await;
        drop(conn);
        (result, notices, server.await.expect("scripted server task"))
    }

    /// A pane target rides `KILL_RESOURCE`; a whole-session target rides
    /// one `KILL_RESOURCES` for every pane in the session.
    #[tokio::test]
    async fn selected_kills_a_pane_or_a_whole_session() {
        use crate::testkit::ScriptSpec;
        let (result, notices, seen) =
            run_selected(ScriptSpec::new().state(pane_state()), "@2").await;
        assert!(result.is_ok(), "{result:?}");
        assert!(notices.is_empty());
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::KillResource { terminal_id, .. },
                    ..
                } if *terminal_id == ResourceId::local(2)
            )),
            "expected KILL_RESOURCE @2; sent {seen:?}"
        );

        let (result, notices, seen) =
            run_selected(ScriptSpec::new().state(pane_state()), "work").await;
        assert!(result.is_ok(), "{result:?}");
        assert!(notices.is_empty());
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::KillResources { ids, .. },
                    ..
                } if ids.len() == 2
            )),
            "expected KILL_RESOURCES for the session; sent {seen:?}"
        );
    }

    /// A miss against a complete view is absence; a miss against a partial
    /// fleet is unresolved. A hit under degradation still kills, and the
    /// snapshot notices are what the CLI/MCP warning prints.
    #[tokio::test]
    async fn selected_splits_a_complete_miss_from_a_partial_view() {
        use crate::testkit::ScriptSpec;
        const NOTICE: &str = "satellite build-box is unreachable: link is down";

        let (result, notices, _) = run_selected(ScriptSpec::new().state(pane_state()), "@9").await;
        assert!(
            matches!(result, Err(KillError::NoSuchTarget { ref target }) if target == "@9"),
            "{result:?}"
        );
        assert!(notices.is_empty());

        let (result, notices, _) = run_selected(
            ScriptSpec::new()
                .state(pane_state())
                .degradation_notice(NOTICE),
            "@9",
        )
        .await;
        assert!(
            matches!(
                result,
                Err(KillError::Unresolved { ref target, ref degradation })
                    if target == "@9" && degradation.notices() == [NOTICE]
            ),
            "{result:?}"
        );
        assert!(
            notices.is_empty(),
            "a miss does not warn as a hit: {notices:?}"
        );

        let (result, notices, seen) = run_selected(
            ScriptSpec::new()
                .state(pane_state())
                .degradation_notice(NOTICE),
            "@2",
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(notices, [NOTICE]);
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::KillResource { terminal_id, .. },
                    ..
                } if *terminal_id == ResourceId::local(2)
            )),
            "a partial-view hit still kills; sent {seen:?}"
        );
    }
}
