//! `phux-client` wire primitives for session-identity writes: `phux rename`
//! and `phux new`'s create-without-attach (ADR-0022 §5).
//!
//! Since the v0.3.0 "Option B" re-tier (ADR-0019 / ADR-0027) dissolved the
//! L2 collection tier and removed the dedicated `CREATE_SESSION` /
//! `RENAME_SESSION` verbs, both are expressed as L3 `SET_METADATA` writes of
//! a conventional key that the server intercepts. Selector-driven duplicate
//! checks and the CLI's own degradation wording stay in
//! `crates/phux/src/commands/new.rs`; this module owns the write + read-back
//! round trips.

use std::collections::BTreeMap;

use phux_protocol::ids::{IdempotencyKey, ResourceId, SessionId};
use phux_protocol::wire::frame::{
    FrameKind, SESSION_CREATE_KEY, SESSION_CREATE_RESULT_KEY, SESSION_CREATE_RESULT_KEY_PREFIX,
    Scope,
};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout::Workspace;
use crate::layout_ops::{LayoutOps, LayoutOpsError};
use crate::rename::{BarrierVerdict, NamedSession, RenamePlan, RenameRefusal};
use phux_protocol::wire::info::SessionSnapshot;

/// The conventional rename write: `current\0new` under
/// [`SESSION_NAME_KEY`](phux_protocol::wire::frame::SESSION_NAME_KEY).
///
/// Since the v0.3.0 "Option B" re-tier (ADR-0019 / ADR-0027) dissolved the
/// L2 collection tier and removed the `RENAME_SESSION` verb, a rename is
/// expressed as an L3 `SET_METADATA` write of this conventional key
/// (`Scope::Global`, value `current\0new`). The server is authoritative —
/// it intercepts this write and applies the registry rename. The bytes are
/// [`crate::rename::write_frame`].
#[must_use]
pub fn rename_frame(request_id: u32, session: &str, new_name: &str) -> FrameKind {
    crate::rename::write_frame(request_id, session, new_name)
}

/// Send the fire-and-forget rename write.
///
/// `SET_METADATA` carries no reply frame, so existence and name-collision
/// checks are the caller's job against a fresh `GET_STATE` snapshot, before
/// and (as an ordering barrier) after this call.
///
/// `request_id` is the caller's to allocate: earlier revisions of this path
/// hardcoded `1` inside the write itself, so a caller composing two renames
/// on one connection sent the identical id twice. Taking it as a parameter
/// lets a caller vary it — see the `two_renames_on_one_connection_correlate`
/// test.
///
/// # Errors
///
/// Transport failures from [`Connection::send`].
pub async fn rename(
    conn: &mut Connection,
    request_id: u32,
    session: &str,
    new_name: &str,
) -> Result<(), AttachError> {
    conn.send(&rename_frame(request_id, session, new_name))
        .await
}

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
    /// The barrier snapshot no longer lists the session.
    #[error("the session no longer exists")]
    SessionGone,
    /// The barrier snapshot still has the session under another name.
    #[error("the server did not rename the session (the name may have been taken meanwhile)")]
    NotApplied,
    /// Transport or decode failure.
    #[error(transparent)]
    Attach(#[from] AttachError),
}

impl From<RenameRefusal> for RenameError {
    fn from(refusal: RenameRefusal) -> Self {
        match refusal {
            RenameRefusal::NoSuchSession => Self::NoSuchSession,
            RenameRefusal::AlreadyExists { new_name } => Self::AlreadyExists { new_name },
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
            Self::SessionGone | Self::NotApplied | Self::Attach(_) => self.to_string(),
        }
    }
}

/// Why the rename must not be sent, judged against a pre-write snapshot:
/// an unknown session, or a new name another session already holds.
#[must_use]
pub fn rename_refusal(
    snapshot: &SessionSnapshot,
    session: &str,
    new_name: &str,
) -> Option<RenameError> {
    match crate::rename::plan_rename(&named(snapshot), session, new_name) {
        RenamePlan::Refused(refusal) => Some(refusal.into()),
        RenamePlan::Unchanged { .. } | RenamePlan::Send { .. } => None,
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
/// Refuses against a fresh snapshot, skips a no-op (the session already has
/// `new_name`), writes, then reads `GET_STATE`. That read is the ordering
/// barrier and the outcome: the snapshot must show the new name on the same
/// session id, or the rename is refused. Snapshot notices from both reads
/// are appended to `notices` (a rename cannot be misled by a partial fleet
/// — session names are hub-local — but the CLI still warns).
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
    let (snapshot, degradation) = crate::state::get_state_on(conn).await?.into_parts();
    notices.extend(degradation.notices().iter().cloned());
    let session_id = match crate::rename::plan_rename(&named(&snapshot), session, new_name) {
        RenamePlan::Unchanged { .. } => return Ok(()),
        RenamePlan::Refused(refusal) => return Err(refusal.into()),
        RenamePlan::Send { session_id } => session_id,
    };
    rename(conn, 1, session, new_name).await?;
    let (after, degradation) = crate::state::get_state_on(conn).await?.into_parts();
    notices.extend(degradation.notices().iter().cloned());
    match crate::rename::barrier_verdict(&named(&after), session_id, new_name) {
        BarrierVerdict::Applied => Ok(()),
        BarrierVerdict::Gone => Err(RenameError::SessionGone),
        BarrierVerdict::NotApplied => Err(RenameError::NotApplied),
    }
}

/// Failure composing the `SESSION_CREATE_KEY` request document.
///
/// Realistically unreachable for the shapes this module builds (owned
/// strings, a string-keyed map), but kept typed rather than `unwrap`ped so a
/// caller composing this into a larger fallible pipeline is not forced to.
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
}

/// What the atomic-agent-session-restore capability probe found.
///
/// Older servers treat the nonce-result namespace
/// ([`SESSION_CREATE_RESULT_KEY_PREFIX`]) as ordinary metadata; current
/// servers reserve it and refuse a direct write. The probe distinguishes the
/// two without ever risking a real create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AtomicPreflightOutcome {
    /// The server reserves the namespace: `agent_session` can ride the
    /// create transaction atomically.
    Supported,
    /// The probe round-tripped instead of being rejected: an older server.
    Unsupported,
    /// The server refused the probe outright.
    Refused(String),
}

/// Probe whether the connected server supports atomic agent-session restore.
///
/// Writes a throwaway value under a fresh nonce key, then checks whether the
/// server reserved it (refused the read) or echoed it back (an older server
/// treating it as ordinary metadata) — cleaning up in the latter case so no
/// probe residue is left behind. Every interleaved degradation notice is
/// appended to `notices`.
///
/// # Errors
///
/// Transport failures from [`Connection::send`] / [`Connection::request_metadata`].
pub async fn atomic_agent_session_preflight(
    conn: &mut Connection,
    notices: &mut Vec<String>,
) -> Result<AtomicPreflightOutcome, AttachError> {
    let probe_key = format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{}", uuid::Uuid::new_v4());
    conn.send(&FrameKind::SetMetadata {
        request_id: 100,
        scope: Scope::Global,
        key: probe_key.clone(),
        value: uuid::Uuid::new_v4().as_bytes().to_vec(),
    })
    .await?;
    let (probe, interleaved) = conn
        .request_metadata(101, Scope::Global, probe_key.clone())
        .await?
        .into_parts();
    notices.extend(crate::state::degradation_notices(&interleaved));
    match probe {
        Ok(None) => Ok(AtomicPreflightOutcome::Supported),
        Ok(Some(_)) => {
            conn.send(&FrameKind::DeleteMetadata {
                request_id: 102,
                scope: Scope::Global,
                key: probe_key.clone(),
            })
            .await?;
            // Ordered read-back confirms the old server processed the
            // cleanup before this connection closes. Its reply (including
            // any interleaved degradation) is discarded whole, matching the
            // pre-migration `let _ = ...` byte-for-byte: this probe's own
            // outcome is already decided, and there is nothing left for a
            // notice arriving on this specific round trip to inform.
            let _ = conn.request_metadata(103, Scope::Global, probe_key).await?;
            Ok(AtomicPreflightOutcome::Unsupported)
        }
        Err(refusal) => Ok(AtomicPreflightOutcome::Refused(refusal.to_string())),
    }
}

/// What creating a session without attaching answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateOutcome {
    /// The seed pane's Terminal id.
    Created(u64),
    /// `agent_session` was requested but the server does not support atomic
    /// agent-session restore (an older server would silently ignore it).
    AtomicRestoreUnsupported,
    /// The atomic-restore capability probe itself was refused.
    ProbeRefused(String),
    /// The confirming read-back was refused.
    ReadRefused(String),
    /// The legacy fallback read-back was refused.
    LegacyReadRefused(String),
    /// No create result ever named this session.
    NotRegistered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CreatedSession {
    terminal_id: u32,
    session_id: Option<SessionId>,
}

/// A `phux new` create-without-attach request. Borrowed so a caller with
/// owned `Option<Vec<String>>`/`Option<String>` fields need not clone them.
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
    /// the create, when the server supports it.
    pub agent_session: Option<&'a [u8]>,
    /// Make the create idempotent (ADR-0126): the key becomes the request's
    /// `request_token`, so a repeat inside the server's horizon answers the
    /// same result. Check [`keyed_create_supported`] first.
    pub idempotency_key: Option<IdempotencyKey>,
}

/// Whether the connected server honors a keyed create (it advertises
/// `SPAWN_IDEMPOTENCY`, ADR-0126). An older server would treat the repeat as
/// a second create of a name already in use.
#[must_use]
pub fn keyed_create_supported(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|bootstrap| {
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

/// Create a named session without attaching, via the conventional
/// `SESSION_CREATE_KEY` write, then read the seed-pane id back from a
/// nonce-correlated, one-shot result key.
///
/// `agent_session_preflighted` is true only when a multi-session caller has
/// already run [`atomic_agent_session_preflight`] before creating any member
/// of its batch; otherwise, when `request.agent_session` is set and legacy
/// results are not allowed, this runs that probe itself. Every interleaved
/// degradation notice is appended to `notices`, in encounter order.
///
/// Duplicate-name rejection is the caller's job against a fresh `GET_STATE`
/// snapshot taken before this call — this function only writes and reads
/// back.
///
/// # Errors
///
/// Transport/decode failures, or a request document that fails to encode
/// (see [`CreateSessionError`]).
pub async fn create_session(
    conn: &mut Connection,
    request: &CreateSessionRequest<'_>,
    allow_legacy_result: bool,
    agent_session_preflighted: bool,
    notices: &mut Vec<String>,
) -> Result<CreateOutcome, CreateSessionError> {
    if !allow_legacy_result && !agent_session_preflighted {
        match atomic_agent_session_preflight(conn, notices).await? {
            AtomicPreflightOutcome::Supported => {}
            AtomicPreflightOutcome::Unsupported => {
                return Ok(CreateOutcome::AtomicRestoreUnsupported);
            }
            AtomicPreflightOutcome::Refused(message) => {
                return Ok(CreateOutcome::ProbeRefused(message));
            }
        }
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
    let created = match read_result(conn, result_key, allow_legacy_result, notices).await? {
        ReadBack::Refused(message) => return Ok(CreateOutcome::ReadRefused(message)),
        ReadBack::LegacyRefused(message) => return Ok(CreateOutcome::LegacyReadRefused(message)),
        ReadBack::Absent => return Ok(CreateOutcome::NotRegistered),
        ReadBack::Correlated(bytes) => {
            created_session_from_result(&bytes, request.name, &request_token, true)
        }
        ReadBack::Legacy(bytes) => {
            created_session_from_result(&bytes, request.name, &request_token, false)
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
    let fallback = Workspace::single(ResourceId::local(created.terminal_id));
    LayoutOps::new(conn, session_id, 10)
        .read_or_seed(fallback)
        .await?;
    Ok(CreateOutcome::Created(u64::from(created.terminal_id)))
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
}

/// Whether the connected server advertises `KeepEmptySessions` (ADR-0105).
#[must_use]
pub fn keep_empty_supported(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|bootstrap| {
        bootstrap
            .server_features
            .contains(phux_protocol::caps::ServerFeature::KeepEmptySessions)
    })
}

/// Create an empty, keep-empty session named `name` (ADR-0105).
///
/// Rides the same `SESSION_CREATE_KEY` write and nonce-correlated read-back
/// as [`create_session`], refusing before writing when the server does not
/// advertise support (see [`keep_empty_supported`]).
///
/// # Errors
///
/// Transport/decode failures, or a request document that fails to encode.
pub async fn create_empty_session(
    conn: &mut Connection,
    name: &str,
    notices: &mut Vec<String>,
) -> Result<CreateEmptyOutcome, CreateSessionError> {
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
    let outcome = match read_result(conn, result_key, false, notices).await? {
        ReadBack::Refused(message) | ReadBack::LegacyRefused(message) => {
            CreateEmptyOutcome::ReadRefused(message)
        }
        ReadBack::Absent | ReadBack::Legacy(_) => CreateEmptyOutcome::NotRegistered,
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

/// Send the `SESSION_CREATE_KEY` write. Frames are ordered on the
/// connection, while the nonce inside the request prevents another
/// concurrent creator from supplying a stale or unrelated Terminal id to the
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
    /// The correlated key was absent but the legacy key answered.
    Legacy(Vec<u8>),
    /// The correlated read was refused.
    Refused(String),
    /// The legacy fallback read was refused.
    LegacyRefused(String),
    /// Neither key held a value (or legacy was not attempted).
    Absent,
}

/// Read only this request's one-shot result, falling back to the legacy
/// uncorrelated key when `allow_legacy_result`. The read-back rides
/// `request_metadata`, not a hand-rolled wait, so a correlated `ERROR`
/// refusal (`proto.md` §9) is reported rather than hanging forever.
async fn read_result(
    conn: &mut Connection,
    result_key: String,
    allow_legacy_result: bool,
    notices: &mut Vec<String>,
) -> Result<ReadBack, AttachError> {
    let (answer, interleaved) = conn
        .request_metadata(2, Scope::Global, result_key)
        .await?
        .into_parts();
    notices.extend(crate::state::degradation_notices(&interleaved));
    let result_value = match answer {
        Ok(value) => value,
        Err(refusal) => return Ok(ReadBack::Refused(refusal.to_string())),
    };
    if let Some(bytes) = result_value {
        return Ok(ReadBack::Correlated(bytes));
    }
    if !allow_legacy_result {
        return Ok(ReadBack::Absent);
    }
    let (legacy_answer, legacy_interleaved) = conn
        .request_metadata(3, Scope::Global, SESSION_CREATE_RESULT_KEY.to_owned())
        .await?
        .into_parts();
    notices.extend(crate::state::degradation_notices(&legacy_interleaved));
    match legacy_answer {
        Ok(Some(bytes)) => Ok(ReadBack::Legacy(bytes)),
        Ok(None) => Ok(ReadBack::Absent),
        Err(refusal) => Ok(ReadBack::LegacyRefused(refusal.to_string())),
    }
}

/// Read the seed pane and owning Session ids out of a create-result document,
/// rejecting one that does not answer for this request: the name must
/// match, and the nonce must be present on a correlated read and absent on a
/// legacy one.
fn created_session_from_result(
    bytes: &[u8],
    name: &str,
    request_token: &str,
    correlated: bool,
) -> Option<CreatedSession> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .filter(|v| v.get("name").and_then(serde_json::Value::as_str) == Some(name))
        .filter(|v| {
            if correlated {
                v.get("request_token").and_then(serde_json::Value::as_str) == Some(request_token)
            } else {
                v.get("request_token").is_none()
            }
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
    let terminal = ResourceId::local(terminal_id);
    let resource = snapshot
        .resources
        .iter()
        .find(|resource| resource.id == terminal)?;
    let window = snapshot
        .windows
        .iter()
        .find(|window| window.id == resource.window_id)?;
    snapshot
        .sessions
        .iter()
        .find(|session| session.name == name && session.id == window.session_id)
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
    /// nonce this request minted; a legacy (uncorrelated) read must carry no
    /// nonce at all.
    #[test]
    fn created_session_from_result_requires_the_right_correlation() {
        let correlated = br#"{"name":"work","session_id":7,"terminal_id":3,"request_token":"tok"}"#;
        assert_eq!(
            created_session_from_result(correlated, "work", "tok", true),
            Some(CreatedSession {
                terminal_id: 3,
                session_id: Some(SessionId::new(7)),
            })
        );
        assert_eq!(
            created_session_from_result(correlated, "work", "other", true),
            None,
            "a mismatched nonce must not confirm the create"
        );
        let legacy = br#"{"name":"work","terminal_id":5}"#;
        assert_eq!(
            created_session_from_result(legacy, "work", "tok", false),
            Some(CreatedSession {
                terminal_id: 5,
                session_id: None,
            })
        );
        assert_eq!(
            created_session_from_result(correlated, "work", "tok", false),
            None,
            "a nonce present on a legacy (uncorrelated) read must not confirm it"
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

    #[test]
    fn the_rename_write_is_current_nul_new_under_the_conventional_key() {
        assert_eq!(
            rename_frame(7, "work", "play"),
            FrameKind::SetMetadata {
                request_id: 7,
                scope: Scope::Global,
                key: SESSION_NAME_KEY.to_owned(),
                value: b"work\0play".to_vec(),
            }
        );
    }

    #[test]
    fn refuses_an_unknown_session_and_a_taken_name() {
        let snap = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1))
            .with_sessions(vec![
                SessionInfo::new(SessionId::new(0), "work"),
                SessionInfo::new(SessionId::new(1), "play"),
            ]);
        assert!(matches!(
            rename_refusal(&snap, "gone", "x"),
            Some(RenameError::NoSuchSession)
        ));
        assert!(matches!(
            rename_refusal(&snap, "work", "play"),
            Some(RenameError::AlreadyExists { ref new_name }) if new_name == "play"
        ));
        assert!(rename_refusal(&snap, "work", "fresh").is_none());
        assert!(rename_refusal(&snap, "work", "work").is_none());
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
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
        let server =
            tokio::spawn(
                async move { crate::testkit::ScriptedServer::accept(&listener, spec).await },
            );
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

    #[tokio::test]
    async fn two_renames_on_one_connection_correlate() {
        // Historically both writes hardcoded request_id 1; a caller
        // allocating distinct ids per call (the fix) is pinned here against
        // a scripted server that pins the two frames it sees.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let socket = temp.path().join("rename.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind");
        let spec = crate::testkit::ScriptSpec::new();
        let server_task =
            tokio::spawn(
                async move { crate::testkit::ScriptedServer::accept(&listener, spec).await },
            );

        let mut conn = Connection::connect(&socket).await.expect("connect");
        rename(&mut conn, 1, "a", "b")
            .await
            .expect("first rename send");
        rename(&mut conn, 2, "a", "c")
            .await
            .expect("second rename send");
        drop(conn);
        let seen = server_task.await.expect("scripted server");
        let ids: Vec<u32> = seen
            .into_iter()
            .filter_map(|frame| match frame {
                FrameKind::SetMetadata {
                    request_id, key, ..
                } if key == SESSION_NAME_KEY => Some(request_id),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![1, 2], "each rename must carry its own request id");
    }

    fn scripted(
        spec: crate::testkit::ScriptSpec,
    ) -> (
        std::path::PathBuf,
        tempfile::TempDir,
        tokio::task::JoinHandle<Vec<FrameKind>>,
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
        let server =
            tokio::spawn(
                async move { crate::testkit::ScriptedServer::accept(&listener, spec).await },
            );
        (socket, dir, server)
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
        let request = CreateSessionRequest {
            idempotency_key: None,
            name: "work",
            command: None,
            cwd: None,
            env: &env,
            agent_session: None,
        };
        let mut notices = Vec::new();
        let outcome = create_session(&mut conn, &request, true, false, &mut notices)
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
    async fn create_session_falls_back_to_the_legacy_key_when_allowed() {
        let legacy = serde_json::json!({"name": "work", "terminal_id": 5})
            .to_string()
            .into_bytes();
        let session = SessionId::new(7);
        let window = WindowId::new(9);
        let snapshot = SessionSnapshot::new(session, window, ResourceId::local(5))
            .with_sessions(vec![SessionInfo::new(session, "work")])
            .with_windows(vec![WindowInfo::new(window, session, "1")])
            .with_resources(vec![ResourceInfo::new(
                ResourceId::local(5),
                window,
                80,
                24,
            )]);
        let spec = ScriptSpec::new()
            .stored_metadata(Scope::Global, SESSION_CREATE_RESULT_KEY, legacy)
            .state(snapshot);
        let (socket, _dir, server) = scripted(spec);

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let env = BTreeMap::new();
        let request = CreateSessionRequest {
            idempotency_key: None,
            name: "work",
            command: None,
            cwd: None,
            env: &env,
            agent_session: None,
        };
        let mut notices = Vec::new();
        let outcome = create_session(&mut conn, &request, true, false, &mut notices)
            .await
            .expect("scripted server answers");
        drop(conn);
        let seen = server.await.expect("scripted server task");

        assert_eq!(outcome, CreateOutcome::Created(5));
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::GetMetadata { request_id: 3, scope: Scope::Global, key }
                    if key == SESSION_CREATE_RESULT_KEY
            )),
            "expected the legacy fallback read at request_id 3; sent {seen:?}"
        );
    }

    #[tokio::test]
    async fn create_session_reports_not_registered_when_legacy_is_disallowed() {
        use crate::testkit::ScriptSpec;

        let (socket, _dir, server) = scripted(ScriptSpec::new());

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let env = BTreeMap::new();
        let request = CreateSessionRequest {
            idempotency_key: None,
            name: "work",
            command: None,
            cwd: None,
            env: &env,
            agent_session: None,
        };
        let mut notices = Vec::new();
        let outcome = create_session(&mut conn, &request, false, true, &mut notices)
            .await
            .expect("scripted server answers");
        drop(conn);
        let _ = server.await.expect("scripted server task");

        assert_eq!(outcome, CreateOutcome::NotRegistered);
    }

    #[tokio::test]
    async fn atomic_preflight_reports_unsupported_and_cleans_up_an_echoed_probe() {
        use crate::testkit::ScriptSpec;

        let (socket, _dir, server) = scripted(ScriptSpec::new());
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let mut notices = Vec::new();
        let outcome = atomic_agent_session_preflight(&mut conn, &mut notices)
            .await
            .expect("scripted server answers");
        drop(conn);
        let seen = server.await.expect("scripted server task");

        assert_eq!(outcome, AtomicPreflightOutcome::Unsupported);
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::DeleteMetadata { request_id: 102, scope: Scope::Global, key }
                    if key.starts_with(SESSION_CREATE_RESULT_KEY_PREFIX)
            )),
            "expected the probe cleanup delete at request_id 102; sent {seen:?}"
        );
    }

    #[tokio::test]
    async fn atomic_preflight_reports_a_refusal() {
        use crate::testkit::ScriptSpec;
        use phux_protocol::wire::frame::ErrorCode;

        let spec = ScriptSpec::new().refuse_metadata(ErrorCode::PermissionDenied, "no");
        let (socket, _dir, server) = scripted(spec);
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let mut notices = Vec::new();
        let outcome = atomic_agent_session_preflight(&mut conn, &mut notices)
            .await
            .expect("scripted server answers");
        drop(conn);
        let _ = server.await.expect("scripted server task");

        assert!(matches!(outcome, AtomicPreflightOutcome::Refused(_)));
    }

    #[tokio::test]
    async fn create_empty_session_refuses_before_writing_when_unsupported() {
        use crate::testkit::ScriptSpec;

        let (socket, _dir, server) = scripted(ScriptSpec::new());
        let mut conn = Connection::connect(&socket).await.expect("connect");
        let mut notices = Vec::new();
        let outcome = create_empty_session(&mut conn, "parked", &mut notices)
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
        use crate::testkit::ScriptSpec;
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
        let outcome = create_empty_session(&mut conn, "parked", &mut notices)
            .await
            .expect("scripted server answers");
        drop(conn);
        let _ = server.await.expect("scripted server task");

        assert_eq!(outcome, CreateEmptyOutcome::Created);
    }
}
