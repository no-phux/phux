use std::collections::BTreeMap;

use phux_protocol::api::LaunchDiagnostic;
use phux_protocol::ids::{IdempotencyKey, OperationId, ResourceId, ResourceKey};
use phux_protocol::wire::frame::{
    Command, ExecState, LaunchSpec, OrgFilter, Refusal, SessionLifetime,
};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout::ArrangementDoc;
use crate::layout_ops::{LayoutOps, LayoutOpsError};

/// Why a session create failed.
#[derive(Debug, thiserror::Error)]
pub enum CreateSessionError {
    /// Transport or decode failure.
    #[error(transparent)]
    Attach(#[from] AttachError),
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
    /// The session was created, but its seed command did not start. The
    /// session stays, holding the failed launch.
    LaunchFailed {
        /// The seed pane's Terminal id.
        terminal: u64,
        /// Why, when the server recorded it.
        diagnostic: Option<LaunchDiagnostic>,
    },
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
    /// Make the create idempotent: the key is the `SESSION_CREATE`
    /// operation id, so a repeat answers the stored result.
    pub idempotency_key: Option<IdempotencyKey>,
    /// Whether the session outlives its last resource. `phux new NAME` is
    /// persistent; a bare `phux` session is ephemeral.
    pub lifetime: SessionLifetime,
    /// Descriptive attributes (`docs/organization.md`), such as the project
    /// identity (ADR-0168).
    pub attrs: &'a [(String, String)],
}

/// Create a named session without attaching: one `SESSION_CREATE` whose
/// launch is the seed pane, its agent-session provenance riding the launch's
/// restore record (atomic by construction).
///
/// Duplicate-name rejection is the caller's job. Interleaved degradation
/// notices are appended to `notices`.
///
/// # Errors
///
/// Transport/decode failures, or an organization request that failed
/// outright (see [`CreateSessionError`]).
pub async fn create_session(
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
            let seed =
                seed_created_layout(conn, committed.session, committed.resource, terminal).await?;
            Ok(match seed {
                Seed::Placed => CreateOutcome::Created(u64::from(terminal)),
                Seed::Failed(diagnostic) => CreateOutcome::LaunchFailed {
                    terminal: u64::from(terminal),
                    diagnostic,
                },
            })
        }
        crate::org::OrgOutcome::Refused(refusal) => Ok(CreateOutcome::Refused(refusal)),
        crate::org::OrgOutcome::Error { message, .. } => Ok(CreateOutcome::ReadRefused(message)),
        _ => Err(CreateSessionError::Org(
            crate::org::OrgError::OutcomeUnknown {
                operation: Some(operation),
            },
        )),
    }
}

/// What became of the seed pane of a session created headless.
enum Seed {
    /// Its arrangement is in place, or it ended and took its session along.
    Placed,
    /// Its command did not start, with the server's reason.
    Failed(Option<LaunchDiagnostic>),
}

/// Give a session created headless the one-group arrangement of its seed
/// pane, unless it already has one. The server runs the launch before it
/// answers the create, so a seed whose command could not start already
/// reads as failed here.
async fn seed_created_layout(
    conn: &mut Connection,
    session: Option<phux_protocol::ids::SessionKey>,
    seed: Option<ResourceKey>,
    terminal: u32,
) -> Result<Seed, CreateSessionError> {
    let Some(session) = session else {
        return Ok(Seed::Placed);
    };
    let inventory = crate::org::fetch(conn, OrgFilter::Session(session), 3)
        .await
        .map_err(CreateSessionError::Org)?;
    if let Some(activation) = seed
        .and_then(|key| inventory.resource(&key))
        .and_then(|record| record.activation.as_ref())
        && activation.state == ExecState::Failed
    {
        return Ok(Seed::Failed(activation.diagnostic));
    }
    let Some(live) = inventory.session(&session).and_then(|record| record.live) else {
        return Ok(Seed::Placed);
    };
    match LayoutOps::new(conn, live, 10)
        .read_or_seed(ArrangementDoc::single(ResourceId::local(terminal)))
        .await
    {
        // The seed pane ended and took its session with it: nothing to seed.
        Ok(_) | Err(LayoutOpsError::MissingLayout) => Ok(Seed::Placed),
        Err(err) => Err(err.into()),
    }
}

/// Why a seed command did not start, in words: `command not found` for the
/// common case of a missing program.
#[must_use]
pub fn launch_failure_reason(diagnostic: Option<&LaunchDiagnostic>) -> String {
    use phux_protocol::api::LaunchStage;
    let Some(diagnostic) = diagnostic else {
        return "it did not start".to_owned();
    };
    let errno = diagnostic.errno.map(std::io::Error::from_raw_os_error);
    match (diagnostic.stage, errno) {
        (LaunchStage::Exec, Some(errno)) if errno.kind() == std::io::ErrorKind::NotFound => {
            "command not found".to_owned()
        }
        (LaunchStage::Exec, Some(errno)) => format!("it could not be run: {errno}"),
        (LaunchStage::Cwd, Some(errno)) => format!("its working directory is unusable: {errno}"),
        (stage, Some(errno)) => format!("its launch failed at {stage:?}: {errno}"),
        (stage, None) => format!("its launch failed at {stage:?}"),
    }
}
