//! Session-identity writes: `phux rename` and `phux new`'s
//! create-without-attach (ADR-0022 §5).
//!
//! Against a server that advertises `ORGANIZATION` both are organization
//! commands: `SESSION_RENAME` naming the session's key and the revision read
//! from its record, and `SESSION_CREATE` with the seed pane as its launch.
//! Each carries a fresh `OperationId` (a keyed create's idempotency key is
//! its operation id), and the `COMMITTED` or `REFUSED` reply is the outcome.
//!
//! Against an older server both are L3 `SET_METADATA` writes of a
//! conventional key the server intercepts (ADR-0027); this module owns those
//! write + read-back round trips too.

use std::collections::BTreeMap;

use phux_protocol::ids::{IdempotencyKey, OperationId, ResourceId, SessionId};
use phux_protocol::wire::frame::{
    Command, FrameKind, LaunchSpec, OrgFilter, Refusal, SESSION_CREATE_KEY,
    SESSION_CREATE_RESULT_KEY_PREFIX, Scope, SessionLifetime,
};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout::ArrangementDoc;
use crate::layout_ops::{LayoutOps, LayoutOpsError};
use crate::rename::{BarrierVerdict, NamedSession, RenamePlan, RenameRefusal};
use phux_protocol::wire::info::SessionSnapshot;

/// Why [`rename_checked`] refused to send the write.
#[derive(Debug, thiserror::Error)]
pub enum RenameError {
    /// `session` is not in the snapshot. Session names are hub-local.
    #[error("no such session")]
    NoSuchSession,
    /// `new_name` is already held by another session.
    #[error("{new_name:?} already exists")]
    AlreadyExists {
        /// The name that collided.
        new_name: String,
    },
    /// `new_name` could not be typed back as a session selector.
    #[error(transparent)]
    InvalidName(#[from] crate::rename::SessionNameError),
    /// The barrier snapshot no longer lists the session.
    #[error("the session no longer exists")]
    SessionGone,
    /// The barrier snapshot still has the session under another name.
    #[error("the server did not rename the session (the name may have been taken meanwhile)")]
    NotApplied,
    /// The server refused the `SESSION_RENAME`.
    #[error("{}", crate::org::refusal_sentence(.0))]
    Refused(Refusal),
    /// An organization request failed outright.
    #[error(transparent)]
    Org(crate::org::OrgError),
    /// Transport or decode failure.
    #[error(transparent)]
    Attach(#[from] AttachError),
}

impl From<RenameRefusal> for RenameError {
    fn from(refusal: RenameRefusal) -> Self {
        match refusal {
            RenameRefusal::NoSuchSession => Self::NoSuchSession,
            RenameRefusal::AlreadyExists { new_name } => Self::AlreadyExists { new_name },
            RenameRefusal::InvalidName(invalid) => Self::InvalidName(invalid),
        }
    }
}

impl RenameError {
    /// The refusal reason both surfaces interpolate after `rename refused
    /// for session …:`.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::NoSuchSession => "no such session".to_owned(),
            Self::AlreadyExists { new_name } => format!("{new_name:?} already exists"),
            Self::InvalidName(_)
            | Self::SessionGone
            | Self::NotApplied
            | Self::Refused(_)
            | Self::Org(_)
            | Self::Attach(_) => self.to_string(),
        }
    }
}

fn named(snapshot: &SessionSnapshot) -> Vec<NamedSession<'_>> {
    snapshot
        .sessions
        .iter()
        .map(|session| NamedSession {
            id: session.id,
            name: session.name.as_str(),
        })
        .collect()
}

/// The checked rename: the shared [`crate::rename`] policy on this connection.
///
/// Refuses against a fresh snapshot and skips a no-op. Against a server that
/// advertises `ORGANIZATION` it then sends `SESSION_RENAME` with the
/// revision read from the session's record, retrying against a fresh record
/// when another writer moved it (`CONFLICT`); the reply is the outcome.
/// Against an older server it writes the rename (which has no reply), then
/// reads `GET_STATE` as the ordering barrier: it must show the new name on
/// the same session id. Snapshot notices are appended to `notices`.
///
/// # Errors
///
/// [`RenameError`] — see its variants.
pub async fn rename_checked(
    conn: &mut Connection,
    session: &str,
    new_name: &str,
    notices: &mut Vec<String>,
) -> Result<(), RenameError> {
    if crate::org::supported(conn) {
        return rename_by_record(conn, session, new_name, notices).await;
    }
    let (snapshot, degradation) = crate::state::get_state_on(conn).await?.into_parts();
    notices.extend(degradation.notices().iter().cloned());
    let session_id = match crate::rename::plan_rename(&named(&snapshot), session, new_name) {
        RenamePlan::Unchanged { .. } => return Ok(()),
        RenamePlan::Refused(refusal) => return Err(refusal.into()),
        RenamePlan::Send { session_id } => session_id,
    };
    conn.send(&crate::rename::write_frame(1, session, new_name))
        .await?;
    let (after, degradation) = crate::state::get_state_on(conn).await?.into_parts();
    notices.extend(degradation.notices().iter().cloned());
    match crate::rename::barrier_verdict(&named(&after), session_id, new_name) {
        BarrierVerdict::Applied => Ok(()),
        BarrierVerdict::Gone => Err(RenameError::SessionGone),
        BarrierVerdict::NotApplied => Err(RenameError::NotApplied),
    }
}

/// [`rename_checked`] against a server that advertises `ORGANIZATION`: one
/// `SESSION_RENAME`, last writer wins.
async fn rename_by_record(
    conn: &mut Connection,
    session: &str,
    new_name: &str,
    notices: &mut Vec<String>,
) -> Result<(), RenameError> {
    let inventory = crate::org::fetch(conn, OrgFilter::All, 1)
        .await
        .map_err(RenameError::Org)?;
    notices.extend(inventory.degradation.notices().iter().cloned());
    let roster: Vec<NamedSession<'_>> = inventory
        .sessions
        .iter()
        .enumerate()
        .map(|(index, record)| NamedSession {
            id: record
                .live
                .unwrap_or_else(|| SessionId::new(u32::MAX - u32::try_from(index).unwrap_or(0))),
            name: record.name.as_str(),
        })
        .collect();
    let target = match crate::rename::plan_rename(&roster, session, new_name) {
        RenamePlan::Unchanged { .. } => return Ok(()),
        RenamePlan::Refused(refusal) => return Err(refusal.into()),
        RenamePlan::Send { .. } => inventory
            .session_named(session)
            .ok_or(RenameError::NoSuchSession)?,
    };
    let command =
        crate::rename::org_rename_command(crate::org::mint_operation(), target.key, new_name);
    let (outcome, degradation) = crate::org::request(conn, 2, command)
        .await
        .map_err(RenameError::Org)?;
    notices.extend(degradation.notices().iter().cloned());
    match outcome {
        crate::org::OrgOutcome::Committed(_) | crate::org::OrgOutcome::Accepted => Ok(()),
        crate::org::OrgOutcome::Refused(Refusal::NotFound) => Err(RenameError::SessionGone),
        crate::org::OrgOutcome::Refused(Refusal::NameTaken) => Err(RenameError::AlreadyExists {
            new_name: new_name.to_owned(),
        }),
        crate::org::OrgOutcome::Refused(refusal) => Err(RenameError::Refused(refusal)),
        _ => Err(RenameError::NotApplied),
    }
}

/// Why a session create failed.
#[derive(Debug, thiserror::Error)]
pub enum CreateSessionError {
    /// Transport or decode failure.
    #[error(transparent)]
    Attach(#[from] AttachError),
    /// The request document could not be serialized.
    #[error("failed to serialize create request: {0}")]
    Encode(#[from] serde_json::Error),
    /// The session was created but its initial headless layout could not be
    /// read, encoded, written, or confirmed.
    #[error("failed to initialize created session layout: {0}")]
    Layout(#[from] LayoutOpsError),
    /// An organization request failed outright (the server refused it, or
    /// the connection dropped with the outcome unknown).
    #[error(transparent)]
    Org(crate::org::OrgError),
}

/// What creating a session without attaching answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateOutcome {
    /// The seed pane's Terminal id.
    Created(u64),
    /// The confirming read-back was refused.
    ReadRefused(String),
    /// No create result ever named this session.
    NotRegistered,
    /// The server refused the `SESSION_CREATE`.
    Refused(Refusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CreatedSession {
    terminal_id: u32,
    session_id: Option<SessionId>,
}

/// A `phux new` create-without-attach request.
#[derive(Debug, Clone, Copy)]
pub struct CreateSessionRequest<'a> {
    /// The session name.
    pub name: &'a str,
    /// The seed pane's command, or `None` for the default shell.
    pub command: Option<&'a [String]>,
    /// The seed pane's working directory.
    pub cwd: Option<&'a str>,
    /// Extra environment variables for the seed pane.
    pub env: &'a BTreeMap<String, String>,
    /// Encoded `AgentSessionRecord` provenance to restore atomically with
    /// the create.
    pub agent_session: Option<&'a [u8]>,
    /// Make the create idempotent (ADR-0126): the key becomes the request's
    /// `request_token`, so a repeat inside the server's horizon answers the
    /// same result. Check [`keyed_create_supported`] first. Against a server
    /// that advertises `ORGANIZATION` it is the `SESSION_CREATE` operation
    /// id.
    pub idempotency_key: Option<IdempotencyKey>,
    /// Whether the session outlives its last resource. `phux new NAME` is
    /// persistent; a bare `phux` session is ephemeral. Only a server that
    /// advertises `ORGANIZATION` honors ephemeral.
    pub lifetime: SessionLifetime,
    /// Descriptive attributes (`docs/spec/L2.md` §2.2), such as the project
    /// identity (ADR-0167). Only a server that advertises `ORGANIZATION`
    /// records them.
    pub attrs: &'a [(String, String)],
}

/// Whether the connected server honors a keyed create.
///
/// It advertises `ORGANIZATION`, whose operation ids dedupe, or
/// `SPAWN_IDEMPOTENCY` (ADR-0126). An older server would treat the repeat as
/// a second create of a name already in use.
#[must_use]
pub fn keyed_create_supported(conn: &Connection) -> bool {
    crate::org::supported(conn)
        || conn.negotiated_bootstrap().is_some_and(|bootstrap| {
            bootstrap
                .server_features
                .contains(phux_protocol::caps::ServerFeature::SpawnIdempotency)
        })
}

/// The `request_token` for a create: the idempotency key in the UUID shape
/// the server accepts, or a fresh random one.
fn request_token(key: Option<IdempotencyKey>) -> String {
    key.map_or_else(
        || uuid::Uuid::new_v4().to_string(),
        |key| uuid::Uuid::from_bytes(*key.as_bytes()).to_string(),
    )
}

/// Create a named session without attaching via the `SESSION_CREATE_KEY`
/// write, then read the seed-pane id back from a nonce-correlated result key.
///
/// Duplicate-name rejection is the caller's job. Interleaved degradation
/// notices are appended to `notices`.
///
/// # Errors
///
/// Transport/decode failures, or a request document that fails to encode
/// (see [`CreateSessionError`]).
pub async fn create_session(
    conn: &mut Connection,
    request: &CreateSessionRequest<'_>,
    notices: &mut Vec<String>,
) -> Result<CreateOutcome, CreateSessionError> {
    if crate::org::supported(conn) {
        return create_by_record(conn, request, notices).await;
    }
    let request_token = request_token(request.idempotency_key);
    let result_key = format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{request_token}");
    let create_bytes = serde_json::to_vec(&serde_json::json!({
        "name": request.name,
        "command": request.command,
        "cwd": request.cwd,
        "env": request.env,
        "request_token": request_token,
        "agent_session": request.agent_session,
    }))?;
    send_create(conn, create_bytes).await?;
    let created = match read_result(conn, result_key, notices).await? {
        ReadBack::Refused(message) => return Ok(CreateOutcome::ReadRefused(message)),
        ReadBack::Absent => return Ok(CreateOutcome::NotRegistered),
        ReadBack::Correlated(bytes) => {
            created_session_from_result(&bytes, request.name, &request_token)
        }
    };
    let Some(created) = created else {
        return Ok(CreateOutcome::NotRegistered);
    };
    let session_id = if let Some(session_id) = created.session_id {
        session_id
    } else {
        let view = crate::state::get_state_on(conn).await?;
        notices.extend(view.degradation().notices().iter().cloned());
        let Some(session_id) =
            created_session_id_from_snapshot(view.snapshot(), request.name, created.terminal_id)
        else {
            return Ok(CreateOutcome::NotRegistered);
        };
        session_id
    };
    let fallback = ArrangementDoc::single(ResourceId::local(created.terminal_id));
    LayoutOps::new(conn, session_id, 10)
        .read_or_seed(fallback)
        .await?;
    Ok(CreateOutcome::Created(u64::from(created.terminal_id)))
}

/// [`create_session`] against a server that advertises `ORGANIZATION`: one
/// `SESSION_CREATE` whose launch is the seed pane, its agent-session
/// provenance riding the launch's restore record (atomic by construction).
async fn create_by_record(
    conn: &mut Connection,
    request: &CreateSessionRequest<'_>,
    notices: &mut Vec<String>,
) -> Result<CreateOutcome, CreateSessionError> {
    let launch = LaunchSpec::new(request.command.map(<[String]>::to_vec).unwrap_or_default())
        .with_cwd(request.cwd.map(str::to_owned))
        .with_env(
            request
                .env
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        )
        .with_restore(request.agent_session.map(<[u8]>::to_vec));
    let operation: OperationId = request
        .idempotency_key
        .unwrap_or_else(crate::org::mint_operation);
    let command = Command::SessionCreate {
        operation,
        name: request.name.to_owned(),
        lifetime: request.lifetime,
        attrs: request.attrs.to_vec(),
        launch: Some(Box::new(launch)),
    };
    let (outcome, degradation) = crate::org::request(conn, 1, command)
        .await
        .map_err(CreateSessionError::Org)?;
    notices.extend(degradation.notices().iter().cloned());
    match outcome {
        crate::org::OrgOutcome::Committed(committed) => {
            let Some(terminal) = committed
                .target
                .as_ref()
                .and_then(|target| target.resource.local_id())
            else {
                return Ok(CreateOutcome::NotRegistered);
            };
            seed_created_layout(conn, committed.session, terminal).await?;
            Ok(CreateOutcome::Created(u64::from(terminal)))
        }
        crate::org::OrgOutcome::Refused(refusal) => Ok(CreateOutcome::Refused(refusal)),
        crate::org::OrgOutcome::Error { message, .. } => Ok(CreateOutcome::ReadRefused(message)),
        _ => Ok(CreateOutcome::NotRegistered),
    }
}

/// Give a session created headless the one-group arrangement of its seed
/// pane, unless it already has one.
async fn seed_created_layout(
    conn: &mut Connection,
    session: Option<phux_protocol::ids::SessionKey>,
    terminal: u32,
) -> Result<(), CreateSessionError> {
    let Some(session) = session else {
        return Ok(());
    };
    let inventory = crate::org::fetch(conn, OrgFilter::Session(session), 3)
        .await
        .map_err(CreateSessionError::Org)?;
    let Some(live) = inventory.session(&session).and_then(|record| record.live) else {
        return Ok(());
    };
    LayoutOps::new(conn, live, 10)
        .read_or_seed(ArrangementDoc::single(ResourceId::local(terminal)))
        .await?;
    Ok(())
}

/// What creating an empty, keep-empty session (ADR-0105) answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateEmptyOutcome {
    /// The empty session was created.
    Created,
    /// The connected server does not advertise `KeepEmptySessions`: an older
    /// server would ignore the unknown `empty` field and seed a shell.
    Unsupported,
    /// The confirming read-back was refused.
    ReadRefused(String),
    /// No create result ever named this session as empty.
    NotRegistered,
    /// The server refused the `SESSION_CREATE`.
    Refused(Refusal),
}

/// Whether the connected server can create an empty session: it advertises
/// `ORGANIZATION` (a persistent session with no launch) or, before that,
/// `KeepEmptySessions` (ADR-0105).
#[must_use]
pub fn keep_empty_supported(conn: &Connection) -> bool {
    crate::org::supported(conn)
        || conn.negotiated_bootstrap().is_some_and(|bootstrap| {
            bootstrap
                .server_features
                .contains(phux_protocol::caps::ServerFeature::KeepEmptySessions)
        })
}

/// Create an empty, persistent session named `name`.
///
/// Against a server that advertises `ORGANIZATION` this is one
/// `SESSION_CREATE` with no launch. Against an older one it rides the same
/// `SESSION_CREATE_KEY` write and nonce-correlated read-back as
/// [`create_session`] (ADR-0105), refusing before writing when the server
/// does not advertise support (see [`keep_empty_supported`]).
///
/// # Errors
///
/// Transport/decode failures, or a request document that fails to encode.
pub async fn create_empty_session(
    conn: &mut Connection,
    name: &str,
    attrs: &[(String, String)],
    notices: &mut Vec<String>,
) -> Result<CreateEmptyOutcome, CreateSessionError> {
    if crate::org::supported(conn) {
        let command = Command::SessionCreate {
            operation: crate::org::mint_operation(),
            name: name.to_owned(),
            lifetime: SessionLifetime::Persistent,
            attrs: attrs.to_vec(),
            launch: None,
        };
        let (outcome, degradation) = crate::org::request(conn, 1, command)
            .await
            .map_err(CreateSessionError::Org)?;
        notices.extend(degradation.notices().iter().cloned());
        return Ok(match outcome {
            crate::org::OrgOutcome::Committed(_) => CreateEmptyOutcome::Created,
            crate::org::OrgOutcome::Refused(refusal) => CreateEmptyOutcome::Refused(refusal),
            crate::org::OrgOutcome::Error { message, .. } => {
                CreateEmptyOutcome::ReadRefused(message)
            }
            _ => CreateEmptyOutcome::NotRegistered,
        });
    }
    if !keep_empty_supported(conn) {
        return Ok(CreateEmptyOutcome::Unsupported);
    }
    let request_token = uuid::Uuid::new_v4().to_string();
    let result_key = format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{request_token}");
    let create_bytes = serde_json::to_vec(&serde_json::json!({
        "name": name,
        "empty": true,
        "keep_empty": true,
        "request_token": request_token,
    }))?;
    send_create(conn, create_bytes).await?;
    let outcome = match read_result(conn, result_key, notices).await? {
        ReadBack::Refused(message) => CreateEmptyOutcome::ReadRefused(message),
        ReadBack::Absent => CreateEmptyOutcome::NotRegistered,
        ReadBack::Correlated(bytes) => {
            if empty_session_result_matches(&bytes, name, &request_token) {
                CreateEmptyOutcome::Created
            } else {
                CreateEmptyOutcome::NotRegistered
            }
        }
    };
    Ok(outcome)
}

/// Send the `SESSION_CREATE_KEY` write; the nonce inside correlates the
/// read-back that follows.
async fn send_create(conn: &mut Connection, create_bytes: Vec<u8>) -> Result<(), AttachError> {
    conn.send(&FrameKind::SetMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: SESSION_CREATE_KEY.to_owned(),
        value: create_bytes,
    })
    .await
}

/// One create's read-back, classified.
enum ReadBack {
    /// The correlated nonce key answered with a value.
    Correlated(Vec<u8>),
    /// The correlated read was refused.
    Refused(String),
    /// The correlated key held no value.
    Absent,
}

/// Read only this request's one-shot result.
async fn read_result(
    conn: &mut Connection,
    result_key: String,
    notices: &mut Vec<String>,
) -> Result<ReadBack, AttachError> {
    let (answer, interleaved) = conn
        .request_metadata(2, Scope::Global, result_key)
        .await?
        .into_parts();
    notices.extend(crate::state::degradation_notices(&interleaved));
    Ok(match answer {
        Ok(Some(bytes)) => ReadBack::Correlated(bytes),
        Ok(None) => ReadBack::Absent,
        Err(refusal) => ReadBack::Refused(refusal.to_string()),
    })
}

/// Read the seed pane and owning Session ids out of a create-result document,
/// rejecting one that does not answer for this request: both the name and
/// the nonce must match.
fn created_session_from_result(
    bytes: &[u8],
    name: &str,
    request_token: &str,
) -> Option<CreatedSession> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .filter(|v| v.get("name").and_then(serde_json::Value::as_str) == Some(name))
        .filter(|v| {
            v.get("request_token").and_then(serde_json::Value::as_str) == Some(request_token)
        })
        .and_then(|v| {
            let terminal_id = u32::try_from(v.get("terminal_id")?.as_u64()?).ok()?;
            let session_id = v
                .get("session_id")
                .and_then(serde_json::Value::as_u64)
                .and_then(|id| u32::try_from(id).ok())
                .map(SessionId::new);
            Some(CreatedSession {
                terminal_id,
                session_id,
            })
        })
}

/// Bind a legacy name-only create receipt to the session that still owns its
/// returned Terminal, so concurrent rename/recreate activity cannot seed a
/// same-named but unrelated session.
fn created_session_id_from_snapshot(
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    name: &str,
    terminal_id: u32,
) -> Option<SessionId> {
    let owner = crate::resource::session_of(snapshot, &ResourceId::local(terminal_id))?;
    snapshot
        .sessions
        .iter()
        .find(|session| session.name == name && session.id == owner)
        .map(|session| session.id)
}

/// Whether a create-result document answers an empty-session request: the
/// name and nonce match, and the server confirms it created no terminal.
fn empty_session_result_matches(bytes: &[u8], name: &str, request_token: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|v| {
        v.get("name").and_then(serde_json::Value::as_str) == Some(name)
            && v.get("request_token").and_then(serde_json::Value::as_str) == Some(request_token)
            && v.get("empty").and_then(serde_json::Value::as_bool) == Some(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::ScriptSpec;
    use phux_protocol::ids::WindowId;
    use phux_protocol::wire::frame::SESSION_NAME_KEY;
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

    #[test]
    fn empty_session_result_requires_name_nonce_and_empty() {
        let ok = br#"{"name":"parked","terminal_id":null,"request_token":"tok","empty":true}"#;
        assert!(empty_session_result_matches(ok, "parked", "tok"));
        let other_nonce = br#"{"name":"parked","request_token":"x","empty":true}"#;
        assert!(!empty_session_result_matches(other_nonce, "parked", "tok"));
        let seeded = br#"{"name":"parked","terminal_id":3,"request_token":"tok"}"#;
        assert!(!empty_session_result_matches(seeded, "parked", "tok"));
    }

    /// The correlated read-back must match both the requested name and the
    /// nonce this request minted.
    #[test]
    fn created_session_from_result_requires_the_right_correlation() {
        let correlated = br#"{"name":"work","session_id":7,"terminal_id":3,"request_token":"tok"}"#;
        assert_eq!(
            created_session_from_result(correlated, "work", "tok"),
            Some(CreatedSession {
                terminal_id: 3,
                session_id: Some(SessionId::new(7)),
            })
        );
        assert_eq!(
            created_session_from_result(correlated, "work", "other"),
            None,
            "a mismatched nonce must not confirm the create"
        );
        let unnonced = br#"{"name":"work","terminal_id":5}"#;
        assert_eq!(
            created_session_from_result(unnonced, "work", "tok"),
            None,
            "a result without this request's nonce must not confirm it"
        );
    }

    #[test]
    fn legacy_create_receipt_must_match_the_terminals_current_owner() {
        let owner = SessionId::new(7);
        let impostor = SessionId::new(8);
        let window = WindowId::new(9);
        let snapshot = SessionSnapshot::new(owner, window, ResourceId::local(11))
            .with_sessions(vec![
                SessionInfo::new(owner, "renamed"),
                SessionInfo::new(impostor, "requested"),
            ])
            .with_windows(vec![WindowInfo::new(window, owner, "1")])
            .with_resources(vec![ResourceInfo::new(
                ResourceId::local(11),
                window,
                80,
                24,
            )]);

        assert_eq!(
            created_session_id_from_snapshot(&snapshot, "renamed", 11),
            Some(owner)
        );
        assert_eq!(
            created_session_id_from_snapshot(&snapshot, "requested", 11),
            None,
            "a same-named session that does not own the receipt Terminal must not be seeded"
        );
    }

    fn snap(names: &[(&str, u32)]) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
            .with_sessions(
                names
                    .iter()
                    .map(|(name, id)| SessionInfo::new(SessionId::new(*id), *name))
                    .collect(),
            )
    }

    #[tokio::test]
    async fn rename_checked_warns_on_a_partial_view_then_writes() {
        const NOTICE: &str = "satellite edge is unreachable: timed out";
        // Pre-check sees "work"; the barrier snapshot is the applied name.
        let spec = ScriptSpec::new()
            .states([snap(&[("work", 1)]), snap(&[("play", 1)])])
            .degradation_notice(NOTICE);
        let (socket, _dir, server) = scripted(spec);
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let mut notices = Vec::new();
        rename_checked(&mut conn, "work", "play", &mut notices)
            .await
            .expect("rename");
        drop(conn);
        let seen = server.await.expect("scripted server");
        assert_eq!(notices, [NOTICE]);
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::SetMetadata { key, .. } if key == SESSION_NAME_KEY
            )),
            "expected the rename write; sent {seen:?}"
        );
        let gets = seen
            .iter()
            .filter(|frame| {
                matches!(
                    frame,
                    FrameKind::Command {
                        command: phux_protocol::wire::frame::Command::GetState { .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            gets, 2,
            "pre-check and barrier each read GET_STATE; sent {seen:?}"
        );
    }

    #[tokio::test]
    async fn rename_checked_refuses_without_writing() {
        let spec = ScriptSpec::new().state(snap(&[("work", 1), ("play", 2)]));
        let (socket, _dir, server) = scripted(spec);
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let mut notices = Vec::new();
        let unknown = rename_checked(&mut conn, "gone", "x", &mut notices)
            .await
            .expect_err("unknown session");
        assert_eq!(unknown.reason(), "no such session");
        let taken = rename_checked(&mut conn, "work", "play", &mut notices)
            .await
            .expect_err("taken name");
        assert_eq!(taken.reason(), "\"play\" already exists");
        rename_checked(&mut conn, "work", "work", &mut notices)
            .await
            .expect("already named");
        drop(conn);
        let seen = server.await.expect("scripted server");
        assert!(
            !seen.iter().any(|frame| matches!(
                frame,
                FrameKind::SetMetadata { key, .. } if key == SESSION_NAME_KEY
            )),
            "a refusal or a no-op must not write; sent {seen:?}"
        );
    }

    #[tokio::test]
    async fn rename_checked_barrier_reports_a_name_the_server_did_not_apply() {
        let spec = ScriptSpec::new().states([snap(&[("work", 1)]), snap(&[("work", 1)])]);
        let (socket, _dir, server) = scripted(spec);
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let mut notices = Vec::new();
        let err = rename_checked(&mut conn, "work", "notes", &mut notices)
            .await
            .expect_err("barrier still shows the old name");
        assert!(err.reason().contains("did not rename"), "{}", err.reason());
        drop(conn);
        let seen = server.await.expect("scripted server");
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::SetMetadata { key, value, .. }
                    if key == SESSION_NAME_KEY && value == b"work\0notes"
            )),
            "the write is sent before the barrier can refuse it; sent {seen:?}"
        );
    }

    #[tokio::test]
    async fn rename_checked_barrier_reports_a_session_that_disappeared() {
        let spec = ScriptSpec::new().states([snap(&[("work", 1)]), snap(&[("other", 2)])]);
        let (socket, _dir, server) = scripted(spec);
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let mut notices = Vec::new();
        let err = rename_checked(&mut conn, "work", "notes", &mut notices)
            .await
            .expect_err("session gone");
        assert_eq!(err.reason(), "the session no longer exists");
        drop(conn);
        let _ = server.await.expect("scripted server");
    }

    fn scripted(
        spec: ScriptSpec,
    ) -> (
        std::path::PathBuf,
        tempfile::TempDir,
        tokio::task::JoinHandle<Vec<FrameKind>>,
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let (socket, server) = crate::testkit::serve_one(dir.path(), spec);
        (socket, dir, server)
    }

    fn work_request(env: &BTreeMap<String, String>) -> CreateSessionRequest<'_> {
        CreateSessionRequest {
            idempotency_key: None,
            name: "work",
            command: None,
            cwd: None,
            env,
            agent_session: None,
            lifetime: SessionLifetime::Persistent,
            attrs: &[],
        }
    }

    #[tokio::test]
    async fn create_session_reads_back_the_correlated_result_and_surfaces_degradation() {
        let spec = ScriptSpec::new()
            .metadata(|_scope, key| {
                key.strip_prefix(SESSION_CREATE_RESULT_KEY_PREFIX)
                    .map(|token| {
                        serde_json::json!({
                            "name": "work",
                            "session_id": 7,
                            "terminal_id": 3,
                            "request_token": token,
                        })
                        .to_string()
                        .into_bytes()
                    })
            })
            .degradation_notice("satellite edge is unreachable: timed out");
        let (socket, _dir, server) = scripted(spec);

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let env = BTreeMap::new();
        let request = work_request(&env);
        let mut notices = Vec::new();
        let outcome = create_session(&mut conn, &request, &mut notices)
            .await
            .expect("scripted server answers");
        drop(conn);
        let seen = server.await.expect("scripted server task");

        assert_eq!(outcome, CreateOutcome::Created(3));
        assert_eq!(notices, ["satellite edge is unreachable: timed out"]);
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::SetMetadata { request_id: 1, scope: Scope::Global, key, .. }
                    if key == SESSION_CREATE_KEY
            )),
            "expected the SESSION_CREATE_KEY write at request_id 1; sent {seen:?}"
        );
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::GetMetadata { request_id: 2, scope: Scope::Global, key }
                    if key.starts_with(SESSION_CREATE_RESULT_KEY_PREFIX)
            )),
            "expected the correlated read-back at request_id 2; sent {seen:?}"
        );
    }

    #[tokio::test]
    async fn create_empty_session_refuses_before_writing_when_unsupported() {
        let (socket, _dir, server) = scripted(ScriptSpec::new());
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let mut notices = Vec::new();
        let outcome = create_empty_session(&mut conn, "parked", &[], &mut notices)
            .await
            .expect("scripted server answers");
        drop(conn);
        let seen = server.await.expect("scripted server task");

        assert_eq!(outcome, CreateEmptyOutcome::Unsupported);
        assert!(
            !seen
                .iter()
                .any(|frame| matches!(frame, FrameKind::SetMetadata { .. })),
            "an unsupported server must never see the create write; sent {seen:?}"
        );
    }

    #[tokio::test]
    async fn create_empty_session_creates_when_the_server_confirms_it() {
        use phux_protocol::caps::{ServerFeature, ServerFeatureSet};

        let spec = ScriptSpec::new()
            .server_features(ServerFeatureSet::with(&[ServerFeature::KeepEmptySessions]))
            .metadata(|_scope, key| {
                key.strip_prefix(SESSION_CREATE_RESULT_KEY_PREFIX)
                    .map(|token| {
                        serde_json::json!({
                            "name": "parked",
                            "terminal_id": null,
                            "request_token": token,
                            "empty": true,
                        })
                        .to_string()
                        .into_bytes()
                    })
            });
        let (socket, _dir, server) = scripted(spec);
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let mut notices = Vec::new();
        let outcome = create_empty_session(&mut conn, "parked", &[], &mut notices)
            .await
            .expect("scripted server answers");
        drop(conn);
        let _ = server.await.expect("scripted server task");

        assert_eq!(outcome, CreateEmptyOutcome::Created);
    }

    fn org_spec(fixture: crate::testkit::OrgFixture) -> ScriptSpec {
        use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
        ScriptSpec::new()
            .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
            .organization(fixture)
    }

    /// Under `ORGANIZATION` a rename is one `SESSION_RENAME` naming the
    /// session's key, last writer wins; no metadata write.
    #[tokio::test]
    async fn rename_rides_one_session_rename() {
        let mut fixture = crate::testkit::OrgFixture::new();
        let work = fixture.session("work", SessionLifetime::Persistent);
        fixture.session("play", SessionLifetime::Persistent);
        let (socket, _dir, server) = scripted(org_spec(fixture));
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let mut notices = Vec::new();
        rename_checked(&mut conn, "work", "notes", &mut notices)
            .await
            .expect("rename");
        let taken = rename_checked(&mut conn, "notes", "play", &mut notices)
            .await
            .expect_err("taken");
        assert_eq!(taken.reason(), "\"play\" already exists");
        drop(conn);
        let seen = server.await.expect("scripted server");
        let renames: Vec<_> = seen
            .iter()
            .filter_map(|frame| match frame {
                FrameKind::Command {
                    command: Command::SessionRename { session, name, .. },
                    ..
                } => Some((*session, name.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(renames, vec![(work, "notes".to_owned())]);
        assert!(
            !seen
                .iter()
                .any(|frame| matches!(frame, FrameKind::SetMetadata { .. }))
        );
    }

    /// Under `ORGANIZATION` a create is one `SESSION_CREATE` carrying the
    /// seed launch and the idempotency key as its operation id.
    #[tokio::test]
    async fn create_rides_session_create_with_the_seed_launch() {
        let (socket, _dir, server) = scripted(org_spec(crate::testkit::OrgFixture::new()));
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let env = BTreeMap::from([("A".to_owned(), "1".to_owned())]);
        let key = IdempotencyKey::new([9; 16]).expect("non-zero");
        let mut request = work_request(&env);
        request.idempotency_key = Some(key);
        request.cwd = Some("/src");
        request.agent_session = Some(b"resume");
        let mut notices = Vec::new();
        let outcome = create_session(&mut conn, &request, &mut notices)
            .await
            .expect("create");
        assert!(matches!(outcome, CreateOutcome::Created(_)), "{outcome:?}");
        let again = create_session(&mut conn, &request, &mut notices)
            .await
            .expect("same-key resend");
        assert_eq!(again, outcome, "a same-id resend answers the stored result");
        let empty = create_empty_session(&mut conn, "parked", &[], &mut notices)
            .await
            .expect("empty");
        assert_eq!(empty, CreateEmptyOutcome::Created);
        let taken = create_empty_session(&mut conn, "parked", &[], &mut notices)
            .await
            .expect("answered");
        assert_eq!(taken, CreateEmptyOutcome::Refused(Refusal::NameTaken));
        drop(conn);
        let seen = server.await.expect("scripted server");
        let Some(FrameKind::Command {
            command:
                Command::SessionCreate {
                    operation,
                    launch: Some(launch),
                    lifetime,
                    ..
                },
            ..
        }) = seen.iter().find(|f| {
            matches!(
                f,
                FrameKind::Command {
                    command: Command::SessionCreate { .. },
                    ..
                }
            )
        })
        else {
            panic!("no SESSION_CREATE in {seen:?}");
        };
        assert_eq!(*operation, key);
        assert_eq!(*lifetime, SessionLifetime::Persistent);
        assert_eq!(launch.cwd.as_deref(), Some("/src"));
        assert_eq!(launch.env, vec![("A".to_owned(), "1".to_owned())]);
        assert_eq!(launch.restore.as_deref(), Some(&b"resume"[..]));
        assert!(
            !seen.iter().any(|frame| matches!(
                frame,
                FrameKind::SetMetadata { key, .. } if key == SESSION_CREATE_KEY
            )),
            "the create is not the legacy metadata write"
        );
    }
}
