//! `phux kill` — wire primitives and the shared verb orchestration.
//!
//! Covers `SHUTDOWN`, `KILL_RESOURCE` (one Terminal), and killing a whole
//! session. Against a server that advertises `ORGANIZATION`, killing a
//! session is the workflow of CONTRACTS §04 ([`kill_session`]): read the
//! session's members and revision, `RESOURCE_TERMINATE` each executing
//! member, wait until each is inactive, `RESOURCE_REMOVE` each, then
//! `SESSION_DELETE` naming the revision the workflow last committed against,
//! with a result per target. Work added meanwhile survives and the delete
//! refuses `NOT_EMPTY`. Against an older server it is one atomic
//! `KILL_RESOURCES`.
//!
//! [`selected`] is the selector resolution and whole-session / per-pane
//! choice both `phux kill` and MCP `phux_kill` call, so those surfaces
//! cannot drift.

use phux_protocol::ResourceId;
use phux_protocol::caps::{ServerFeature, ServerFeatureExt};
use phux_protocol::ids::{IdempotencyKey, SessionId};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, OrgFilter};
use phux_protocol::wire::info::SessionSnapshot;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::org::{self, KillReport, OrgError, SessionOutcome, TargetOutcome};
use crate::org_ops::KillWorkflow;
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

/// A successful [`selected`] kill.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selected {
    /// Interleaved notices from the kill command(s) themselves (a hub's
    /// per-satellite unreachability on the kill round trip).
    pub interleaved: Vec<String>,
    /// The per-target result of a whole-session kill workflow, when the
    /// server advertises `ORGANIZATION`.
    pub report: Option<KillReport>,
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
    /// A whole-session kill workflow ended without deleting the session;
    /// the report says what happened to each target.
    #[error("{}", workflow_sentence(label, report))]
    Workflow {
        /// The target label (`session "work"`).
        label: String,
        /// The per-target result.
        report: Box<KillReport>,
        /// Interleaved notices from the workflow's round trips.
        interleaved: Vec<String>,
    },
    /// The session has no members and the server predates `ORGANIZATION`,
    /// which is what deletes an empty session.
    #[error(
        "session {session:?} is empty and this server predates ORGANIZATION, so it cannot \
         delete it"
    )]
    EmptySessionUnsupported {
        /// The session.
        session: String,
    },
    /// An organization request failed outright.
    #[error(transparent)]
    Org(OrgError),
    /// `%name` did not resolve to one agent (ADR-0075 point 3); nothing
    /// was killed.
    #[error(transparent)]
    Agent(#[from] crate::selector::AgentResolveError),
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
            | Self::PaneRefusals { interleaved, .. }
            | Self::Workflow { interleaved, .. } => interleaved,
            _ => &[],
        }
    }
}

/// The sentence for a workflow that did not delete its session: every
/// target that is not gone, and the session's own outcome.
fn workflow_sentence(label: &str, report: &KillReport) -> String {
    let names = report_key_names(report);
    let mut parts: Vec<String> = report
        .targets
        .iter()
        .filter(|(_, outcome)| !outcome.is_gone())
        .map(|(key, outcome)| {
            format!(
                "{}: {}",
                names.display(*key.as_bytes()),
                target_sentence(outcome)
            )
        })
        .collect();
    match &report.session_outcome {
        SessionOutcome::Deleted | SessionOutcome::AlreadyGone => {}
        SessionOutcome::Kept => parts.push("the session was kept".to_owned()),
        SessionOutcome::Refused(refusal) => {
            parts.push(format!(
                "delete refused: {}",
                org::refusal_sentence(refusal)
            ));
        }
        SessionOutcome::Unknown(reason) => parts.push(format!("delete outcome unknown: {reason}")),
    }
    format!("kill incomplete for {label}: {}", parts.join("; "))
}

/// Display names unique among the keys a kill report lists: the session
/// and every member it targeted.
#[must_use]
pub fn report_key_names(report: &KillReport) -> org::KeyNames {
    org::KeyNames::new(
        std::iter::once(*report.session.as_bytes())
            .chain(report.targets.iter().map(|(key, _)| *key.as_bytes())),
    )
}

fn target_sentence(outcome: &TargetOutcome) -> String {
    match outcome {
        TargetOutcome::Removed | TargetOutcome::AlreadyGone => "removed".to_owned(),
        TargetOutcome::Refused(refusal) => org::refusal_sentence(refusal),
        TargetOutcome::Unknown(reason) => format!("outcome unknown ({reason})"),
        TargetOutcome::TimedOut => "did not stop in time".to_owned(),
        TargetOutcome::NoLiveTarget => "runs on another server instance".to_owned(),
    }
}

/// Resolve `selector` against a fresh snapshot on `conn` and kill what it names.
///
/// A whole-session target runs [`kill_session`] against a server that
/// advertises `ORGANIZATION` (an empty session is simply deleted), and one
/// atomic `KILL_RESOURCES` against an older one; a group, pane, `@id`, or
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
    if let (Some(key), Some(id)) = (key, selector.explicit_id()) {
        return kill_keyed(conn, target, vec![id], key).await;
    }
    let (snapshot, degradation) = crate::state::get_state_on(conn).await?.into_parts();
    if let Some(session) = selector::whole_session_name(selector, &snapshot) {
        if org::supported(conn) {
            let Some(id) = session_id_named(&snapshot, &session) else {
                return Err(KillError::NoSuchTarget {
                    target: target.to_owned(),
                });
            };
            notices.extend(degradation.notices().iter().cloned());
            return kill_session(conn, id, &session).await;
        }
        let ids = selector::resolve(selector, &snapshot);
        if ids.is_empty() && session_is_empty(&snapshot, &session) {
            return Err(KillError::EmptySessionUnsupported { session });
        }
        if ids.is_empty() {
            return Err(KillError::NoSuchTarget {
                target: target.to_owned(),
            });
        }
        notices.extend(degradation.notices().iter().cloned());
        let killed = kill_whole_session(conn, &session, ids.clone(), key).await?;
        await_reaped(conn, &ids).await;
        return Ok(killed);
    }
    let terminals = crate::state::resolve_targets_on(conn, selector, &snapshot).await?;
    if terminals.is_empty() {
        return Err(target_miss(target, degradation));
    }
    notices.extend(degradation.notices().iter().cloned());
    let killed = match key {
        Some(key) => kill_keyed(conn, target, terminals.clone(), key).await,
        None => kill_each_terminal(conn, terminals.clone()).await,
    }?;
    await_reaped(conn, &terminals).await;
    Ok(killed)
}

/// The longest a successful kill waits, against an older server, for its
/// panes to leave the server's state: past the pane-kill grace (`SIGHUP`,
/// then `SIGKILL` after 500ms) with room for a loaded host.
const REAP_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Poll cadence while [`await_reaped`] waits.
const REAP_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// Whether the server's kill reply already follows the whole visible
/// teardown (`features_ext.COMMITTED_TEARDOWN`, `docs/spec/L1.md` §5.2), so
/// the next `GET_STATE` omits the killed resources.
fn kill_reply_is_the_teardown(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features_ext
            .contains(ServerFeatureExt::CommittedTeardown)
    })
}

/// Against a server that predates `COMMITTED_TEARDOWN`, wait until none of
/// the local `ids` is still listed by `GET_STATE`; a current server's reply
/// already guarantees it, so this returns at once.
///
/// An older server commits a kill before it replies, but keeps each pane
/// (and so its session) listed until the pane's process is reaped. Without
/// this wait `phux kill work && phux new work` fails on a name that is about
/// to be free, and `phux ls` right after a kill still lists the killed panes
/// as running. Best-effort and bounded: a disconnect is the server exiting
/// after its last session, and a pane that outlives [`REAP_WAIT`] was still
/// killed. Satellite ids are not waited on; their reap is the satellite's
/// business.
async fn await_reaped(conn: &mut Connection, ids: &[ResourceId]) {
    if kill_reply_is_the_teardown(conn) {
        return;
    }
    let local: Vec<&ResourceId> = ids
        .iter()
        .filter(|id| matches!(id, ResourceId::Local { .. }))
        .collect();
    if local.is_empty() {
        return;
    }
    let deadline = tokio::time::Instant::now() + REAP_WAIT;
    loop {
        let Ok(view) = crate::state::get_state_on(conn).await else {
            return;
        };
        let (snapshot, _) = view.into_parts();
        let listed = snapshot
            .resources
            .iter()
            .any(|resource| local.contains(&&resource.id));
        if !listed || tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(REAP_POLL).await;
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

/// Whether the session named `name` holds no windows (ADR-0105).
fn session_is_empty(snapshot: &SessionSnapshot, name: &str) -> bool {
    snapshot
        .sessions
        .iter()
        .any(|session| session.name == name && session.is_empty())
}

fn session_id_named(snapshot: &SessionSnapshot, name: &str) -> Option<SessionId> {
    snapshot
        .sessions
        .iter()
        .find(|session| session.name == name)
        .map(|session| session.id)
}

/// The longest the workflow waits for terminated members to end before it
/// reports them [`TargetOutcome::TimedOut`]; a graceful terminate escalates
/// to a forced one halfway.
const KILL_WAIT: std::time::Duration = std::time::Duration::from_secs(4);

/// Poll cadence while members are ending.
const KILL_POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// Kill the whole session `session` (live handle) by the organization
/// workflow (CONTRACTS §04). The target set is the session's members when
/// the workflow starts; the result names each.
///
/// # Errors
///
/// [`KillError::NoSuchTarget`] when the session has no record,
/// [`KillError::Workflow`] when it was not deleted (with the per-target
/// report), [`KillError::Org`] when its records cannot be read.
pub async fn kill_session(
    conn: &mut Connection,
    session: SessionId,
    label: &str,
) -> Result<Selected, KillError> {
    let label = format!("session {label:?}");
    let inventory = org::fetch(conn, OrgFilter::All, 1)
        .await
        .map_err(KillError::Org)?;
    let mut interleaved = inventory.degradation.notices().to_vec();
    let record =
        inventory
            .session_by_live(session)
            .cloned()
            .ok_or_else(|| KillError::NoSuchTarget {
                target: label.clone(),
            })?;
    let mut members: Vec<_> = inventory.members(&record.key).cloned().collect();
    let mut workflow = KillWorkflow::plan(&record, &members);
    let start = tokio::time::Instant::now();
    let mut escalated = false;
    let mut mint = org::mint_operation;
    loop {
        for (operation, command) in workflow.next_commands(&mut mint) {
            match org::request(conn, 2, command).await {
                Ok((outcome, degradation)) => {
                    interleaved.extend(degradation.notices().iter().cloned());
                    workflow.on_result(&operation, &outcome);
                }
                Err(err) => workflow.on_unknown(&operation, &err.to_string()),
            }
        }
        if workflow.is_done() {
            break;
        }
        if workflow.awaiting().next().is_none() && !workflow.has_in_flight() {
            // Nothing to wait for and nothing to send: refresh once more so
            // the planner can move, else stop.
            let before = workflow.report();
            refresh(conn, &mut workflow, &record.key, &mut members).await;
            if workflow.report() == before && workflow.awaiting().next().is_none() {
                workflow.time_out();
            }
            continue;
        }
        let elapsed = start.elapsed();
        if elapsed >= KILL_WAIT {
            workflow.time_out();
            continue;
        }
        if !escalated && elapsed >= KILL_WAIT / 2 {
            escalated = true;
            workflow.escalate(&members);
            continue;
        }
        tokio::time::sleep(KILL_POLL).await;
        refresh(conn, &mut workflow, &record.key, &mut members).await;
    }
    let report = workflow.report();
    if report.complete() {
        Ok(Selected {
            interleaved,
            report: Some(report),
        })
    } else {
        Err(KillError::Workflow {
            label,
            report: Box::new(report),
            interleaved,
        })
    }
}

/// Re-read the session's records and feed them to the planner. A failed
/// read changes nothing; the wait deadline still bounds the workflow.
async fn refresh(
    conn: &mut Connection,
    workflow: &mut KillWorkflow,
    session: &phux_protocol::ids::SessionKey,
    members: &mut Vec<phux_protocol::wire::frame::ResourceRecord>,
) {
    if let Ok(view) = org::fetch(conn, OrgFilter::Session(*session), 3).await {
        *members = view.members(session).cloned().collect();
        workflow.on_records(view.session(session), &view.resources);
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
            report: None,
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
        Ok(Selected {
            interleaved,
            report: None,
        })
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
    use phux_protocol::caps::{ServerFeatureExtSet, ServerFeatureSet};
    use phux_protocol::ids::WindowId;
    use phux_protocol::wire::frame::{ExecState, FrameKind, Refusal, Scope, SessionLifetime};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, WindowInfo};

    use super::*;
    use crate::testkit::{self, OrgFixture, ScriptSpec};

    const NOTICE: &str = "satellite build-box is unreachable: link is down";

    /// A per-id `KILL_RESOURCES` outcome is `Killed` only when no id failed,
    /// and a refusal names each id that did.
    #[test]
    fn a_merged_kill_outcome_names_every_id_that_was_not_killed() {
        let json = |doc: &str| CommandResult::OkWith(CommandValue::Json(doc.to_owned()));
        let all = r#"{"schema_version":1,"killed":["@1","devbox/@2"],"not_found":[],"failed":[]}"#;
        assert_eq!(KillOutcome::from_result(json(all)), KillOutcome::Killed);
        let partial = r#"{"schema_version":1,"killed":["@1"],"not_found":[],
            "failed":[{"id":"devbox/@2","code":107,"message":"link is down"}]}"#;
        assert_eq!(
            KillOutcome::from_result(json(partial)),
            KillOutcome::Refused("not killed: devbox/@2: link is down".to_owned())
        );
        assert!(matches!(
            KillOutcome::from_result(json("x")),
            KillOutcome::Unexpected(_)
        ));
    }

    /// The kill verbs send their frames, and an interleaved degradation
    /// notice reaches the caller beside the outcome.
    #[tokio::test]
    async fn kill_verbs_send_the_expected_frames_and_keep_degradation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (socket, server) =
            testkit::serve_one(dir.path(), ScriptSpec::new().degradation_notice(NOTICE));
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let ids = vec![ResourceId::local(1), ResourceId::local(2)];
        let (outcome, degradation) = kill_resources(&mut conn, 1, ids.clone())
            .await
            .expect("scripted server");
        assert_eq!(outcome, KillOutcome::Killed);
        assert_eq!(degradation.notices(), [NOTICE.to_owned()]);
        let (outcome, _) = kill_resource(&mut conn, 2, ResourceId::local(3))
            .await
            .expect("scripted server");
        assert_eq!(outcome, KillOutcome::Killed);
        drop(conn);
        let seen = server.await.expect("scripted server task");

        assert!(seen.iter().any(|frame| matches!(
            frame,
            FrameKind::Command { command: Command::KillResources { ids: seen_ids, .. }, .. }
                if *seen_ids == ids
        )));
        assert!(killed_one(&seen, 3), "sent {seen:?}");
    }

    /// A keyed kill is refused, with nothing sent, by a server that does not
    /// advertise `KEYED_SIGNAL`; one that does receives the key.
    #[tokio::test]
    async fn a_keyed_kill_needs_keyed_signal_and_carries_its_key() {
        async fn run(spec: ScriptSpec) -> (Result<(), KeyedError>, Vec<FrameKind>) {
            let dir = tempfile::tempdir().expect("temp dir");
            let (socket, server) = testkit::serve_one(dir.path(), spec);
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
        assert!(seen.iter().any(|frame| matches!(
            frame,
            FrameKind::Command {
                command: Command::KillResource {
                    operation_id: Some(_),
                    ..
                },
                ..
            }
        )));
    }

    fn pane_state() -> SessionSnapshot {
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

    /// The state once every pane is reaped: nothing listed.
    fn reaped_state() -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(10), ResourceId::local(1))
    }

    /// The first `GET_STATE` resolves against [`pane_state`]; every later
    /// one (the post-kill reap wait) answers [`reaped_state`].
    async fn run_selected(
        spec: ScriptSpec,
        target: &str,
    ) -> (Result<Selected, KillError>, Vec<String>, Vec<FrameKind>) {
        run_selected_through(spec.states([pane_state()]).state(reaped_state()), target).await
    }

    async fn run_selected_through(
        spec: ScriptSpec,
        target: &str,
    ) -> (Result<Selected, KillError>, Vec<String>, Vec<FrameKind>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let (socket, server) = testkit::serve_one(dir.path(), spec);
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let selector = crate::selector::parse(target).expect("selector");
        let mut notices = Vec::new();
        let result = selected(&mut conn, &selector, target, None, &mut notices).await;
        drop(conn);
        (result, notices, server.await.expect("scripted server task"))
    }

    fn killed_one(seen: &[FrameKind], id: u32) -> bool {
        seen.iter().any(|frame| {
            matches!(
                frame,
                FrameKind::Command { command: Command::KillResource { terminal_id, .. }, .. }
                    if *terminal_id == ResourceId::local(id)
            )
        })
    }

    /// A pane target rides `KILL_RESOURCE`; a whole-session target rides
    /// one `KILL_RESOURCES` for every pane in the session.
    #[tokio::test]
    async fn selected_kills_a_pane_or_a_whole_session() {
        let (result, notices, seen) = run_selected(ScriptSpec::new(), "@2").await;
        assert!(result.is_ok() && notices.is_empty(), "{result:?}");
        assert!(killed_one(&seen, 2), "sent {seen:?}");

        let (result, notices, seen) = run_selected(ScriptSpec::new(), "work").await;
        assert!(result.is_ok() && notices.is_empty(), "{result:?}");
        assert!(seen.iter().any(|frame| matches!(
            frame,
            FrameKind::Command { command: Command::KillResources { ids, .. }, .. }
                if ids.len() == 2
        )));
    }

    /// `%name` (ADR-0075) kills exactly the named pane, and every refusal is
    /// a typed [`KillError::Agent`] with nothing killed — not the plain
    /// "no such target" the set-valued path reported for a live name.
    #[tokio::test]
    async fn selected_resolves_percent_name_through_the_agent_resolver() {
        use crate::agent_meta::{AgentRecord, RESOURCE_AGENT_KEY};
        use crate::selector::AgentResolveError;
        fn named(records: &[(u32, &'static str, Option<&'static str>)]) -> ScriptSpec {
            records
                .iter()
                .fold(ScriptSpec::new(), |spec, (id, name, kind)| {
                    let record = AgentRecord {
                        name: (*name).to_owned(),
                        kind: kind.map(str::to_owned),
                        ..AgentRecord::default()
                    };
                    spec.stored_metadata(
                        Scope::Resource(ResourceId::local(*id)),
                        RESOURCE_AGENT_KEY,
                        record.encode(),
                    )
                })
        }
        let killed_any = |seen: &[FrameKind]| {
            seen.iter().any(|frame| {
                matches!(
                    frame,
                    FrameKind::Command {
                        command: Command::KillResource { .. } | Command::KillResources { .. },
                        ..
                    }
                )
            })
        };

        let (result, _, seen) = run_selected(named(&[(2, "build", None)]), "%build").await;
        assert!(result.is_ok(), "{result:?}");
        assert!(killed_one(&seen, 2), "sent {seen:?}");

        for (spec, target, want) in [
            (named(&[(2, "build", None)]), "%ghost", "no_such_target"),
            (
                named(&[(1, "build", None), (2, "build", None)]),
                "%build",
                "selector_not_single",
            ),
            (
                named(&[(2, "claude", Some("claude"))]),
                "%claude",
                "invalid_agent_name",
            ),
        ] {
            let (result, _, seen) = run_selected(spec, target).await;
            let Err(KillError::Agent(err)) = &result else {
                panic!("{target}: expected an agent refusal, got {result:?}");
            };
            assert_eq!(err.code(), want, "{target}: {err}");
            assert!(
                !killed_any(&seen),
                "{target}: a refusal kills nothing: {seen:?}"
            );
        }

        let (result, _, seen) = run_selected(
            named(&[(2, "build", None)])
                .refuse_metadata(ErrorCode::PermissionDenied, "store offline"),
            "%build",
        )
        .await;
        assert!(
            matches!(
                result,
                Err(KillError::Agent(AgentResolveError::PartialIndex { .. }))
            ),
            "an unreadable index is partial (exit 3), not a miss: {result:?}"
        );
        assert!(!killed_any(&seen), "sent {seen:?}");
    }

    /// A miss against a complete view is absence; a miss against a partial
    /// fleet is unresolved. A hit under degradation still kills and reports
    /// the notices for the caller's warning.
    #[tokio::test]
    async fn selected_splits_a_complete_miss_from_a_partial_view() {
        let (result, notices, _) = run_selected(ScriptSpec::new(), "@9").await;
        assert!(
            matches!(result, Err(KillError::NoSuchTarget { ref target }) if target == "@9"),
            "{result:?}"
        );
        assert!(notices.is_empty());

        let degraded = || ScriptSpec::new().degradation_notice(NOTICE);
        let (result, notices, _) = run_selected(degraded(), "@9").await;
        assert!(
            matches!(
                result,
                Err(KillError::Unresolved { ref target, ref degradation })
                    if target == "@9" && degradation.notices() == [NOTICE]
            ),
            "{result:?}"
        );
        assert!(notices.is_empty(), "a miss does not warn as a hit");

        let (result, notices, seen) = run_selected(degraded(), "@2").await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(notices, [NOTICE]);
        assert!(killed_one(&seen, 2), "a partial-view hit still kills");
    }

    fn state_reads(seen: &[FrameKind]) -> usize {
        seen.iter()
            .filter(|frame| {
                matches!(
                    frame,
                    FrameKind::Command {
                        command: Command::GetState { .. },
                        ..
                    }
                )
            })
            .count()
    }

    /// Against a server without `COMMITTED_TEARDOWN`, a successful kill
    /// returns only once its panes are no longer listed, so
    /// `phux kill work && phux new work` cannot race the reap.
    #[tokio::test]
    async fn selected_waits_until_an_older_server_reaps_the_killed_panes() {
        let spec = ScriptSpec::new()
            .states([pane_state(), pane_state(), pane_state()])
            .state(reaped_state());
        let (result, _, seen) = run_selected_through(spec, "work").await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            state_reads(&seen),
            4,
            "resolve, two still-listed polls, then reaped: {seen:?}"
        );
    }

    /// A server advertising `COMMITTED_TEARDOWN` replies only once the kill
    /// is the whole visible teardown (L1 §5.2), so the kill polls nothing.
    #[tokio::test]
    async fn selected_trusts_a_committed_teardown_reply() {
        use phux_protocol::caps::ServerFeatureExtSet;

        for target in ["work", "@2"] {
            let spec = ScriptSpec::new()
                .server_features_ext(ServerFeatureExtSet::with(&[
                    ServerFeatureExt::CommittedTeardown,
                ]))
                .states([pane_state(), pane_state()])
                .state(reaped_state());
            let (result, _, seen) = run_selected_through(spec, target).await;
            assert!(result.is_ok(), "{target}: {result:?}");
            assert_eq!(state_reads(&seen), 1, "{target}: resolve only: {seen:?}");
        }
    }

    fn org_spec(fixture: OrgFixture) -> ScriptSpec {
        ScriptSpec::new()
            .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
            .organization(fixture)
    }

    fn commands(seen: &[FrameKind], pick: fn(&Command) -> bool) -> usize {
        seen.iter()
            .filter(|frame| matches!(frame, FrameKind::Command { command, .. } if pick(command)))
            .count()
    }

    /// `phux kill work` under `ORGANIZATION`: terminate each executing
    /// member, remove each, delete the session; no `KILL_RESOURCES`.
    #[tokio::test]
    async fn a_session_kill_runs_the_terminate_remove_delete_workflow() {
        let mut fixture = OrgFixture::new();
        let work = fixture.session("work", SessionLifetime::Persistent);
        let shell = fixture.shell(work);
        let job = fixture.resource(work, Some(ExecState::Running));
        let done = fixture.resource(work, Some(ExecState::Completed));
        let (result, _, seen) = run_selected_through(org_spec(fixture), "work").await;
        let selected = result.expect("killed");
        let report = selected.report.expect("workflow report");
        assert!(report.complete(), "{report:?}");
        assert_eq!(
            report
                .targets
                .iter()
                .map(|(key, _)| *key)
                .collect::<Vec<_>>(),
            vec![shell, job, done]
        );
        assert_eq!(
            report.targets[0].1,
            TargetOutcome::AlreadyGone,
            "its exit policy removed it"
        );
        assert_eq!(report.targets[1].1, TargetOutcome::Removed);
        assert_eq!(report.targets[2].1, TargetOutcome::Removed);
        assert_eq!(
            commands(&seen, |c| matches!(c, Command::ResourceTerminate { .. })),
            2
        );
        assert_eq!(
            commands(&seen, |c| matches!(c, Command::ResourceRemove { .. })),
            2
        );
        assert_eq!(
            commands(&seen, |c| matches!(c, Command::SessionDelete { .. })),
            1
        );
        assert_eq!(
            commands(&seen, |c| matches!(c, Command::KillResources { .. })),
            0
        );
    }

    /// An empty session is deleted outright: no keep-empty write exists any
    /// more.
    #[tokio::test]
    async fn an_empty_session_is_deleted_by_the_workflow() {
        let mut fixture = OrgFixture::new();
        fixture.session("parked", SessionLifetime::Persistent);
        let (result, _, seen) = run_selected_through(org_spec(fixture), "parked").await;
        assert!(result.expect("deleted").report.expect("report").complete());
        assert_eq!(
            commands(&seen, |c| matches!(c, Command::SessionDelete { .. })),
            1
        );
        assert!(
            !seen
                .iter()
                .any(|frame| matches!(frame, FrameKind::SetMetadata { .. })),
            "{seen:?}"
        );
    }

    /// Work added while the kill runs survives: the final delete refuses
    /// `NOT_EMPTY` and the error carries the per-target report.
    #[tokio::test]
    async fn work_added_during_the_kill_survives_and_is_reported() {
        let mut fixture = OrgFixture::new();
        let work = fixture.session("work", SessionLifetime::Persistent);
        fixture.resource(work, Some(ExecState::Completed));
        let fixture = fixture.refuse_next("session_delete", Refusal::NotEmpty { members: 1 });
        let (result, _, _) = run_selected_through(org_spec(fixture), "work").await;
        let Err(KillError::Workflow { report, .. }) = result else {
            panic!("expected a workflow refusal, got {result:?}");
        };
        assert_eq!(report.targets[0].1, TargetOutcome::Removed);
        assert_eq!(
            report.session_outcome,
            SessionOutcome::Refused(Refusal::NotEmpty { members: 1 })
        );
    }

    /// A member that ignores the graceful terminate is forced, then timed
    /// out; the session is kept and the report says why.
    #[tokio::test(start_paused = true)]
    async fn a_member_that_never_stops_times_out_and_keeps_the_session() {
        let mut fixture = OrgFixture::new().terminate_holds();
        let work = fixture.session("work", SessionLifetime::Persistent);
        fixture.resource(work, Some(ExecState::Running));
        let (result, _, seen) = run_selected_through(org_spec(fixture), "work").await;
        let Err(KillError::Workflow { report, .. }) = result else {
            panic!("expected a workflow refusal, got {result:?}");
        };
        assert_eq!(report.targets[0].1, TargetOutcome::TimedOut);
        assert_eq!(report.session_outcome, SessionOutcome::Kept);
        assert!(seen.iter().any(|frame| matches!(
            frame,
            FrameKind::Command {
                command: Command::ResourceTerminate {
                    mode: phux_protocol::wire::frame::TerminateMode::Force,
                    ..
                },
                ..
            }
        )));
        assert_eq!(
            commands(&seen, |c| matches!(c, Command::SessionDelete { .. })),
            0
        );
    }

    /// Without `ORGANIZATION`, an empty session cannot be deleted and
    /// nothing is written.
    #[tokio::test]
    async fn an_empty_session_on_an_older_server_is_refused_without_writing() {
        let session = SessionId::new(1);
        let state = SessionSnapshot::new(session, WindowId::new(10), ResourceId::local(1))
            .with_sessions(vec![
                SessionInfo::new(session, "parked").with_window_count(0),
            ]);
        let (result, _, seen) =
            run_selected_through(ScriptSpec::new().state(state), "parked").await;
        assert!(
            matches!(result, Err(KillError::EmptySessionUnsupported { .. })),
            "{result:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|frame| matches!(frame, FrameKind::SetMetadata { .. })),
            "{seen:?}"
        );
    }
}
