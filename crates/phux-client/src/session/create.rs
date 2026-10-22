use std::collections::BTreeMap;

use phux_protocol::ids::{IdempotencyKey, OperationId, ResourceId};
use phux_protocol::wire::frame::{
    Command, LaunchSpec, OrgFilter, Refusal, SESSION_CREATE_RESULT_KEY_PREFIX, SessionLifetime,
};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout::ArrangementDoc;
use crate::layout_ops::{LayoutOps, LayoutOpsError};

use super::{
    ReadBack, created_session_from_result, created_session_id_from_snapshot, read_result,
    send_create,
};

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
    /// identity (ADR-0168). Only a server that advertises `ORGANIZATION`
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
