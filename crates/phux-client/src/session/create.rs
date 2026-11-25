use std::collections::BTreeMap;

use phux_protocol::api::LaunchDiagnostic;
use phux_protocol::ids::{IdempotencyKey, OperationId, ResourceKey};
use phux_protocol::wire::frame::{
    Command, Committed, ExecState, LaunchSpec, Refusal, SessionLifetime,
};

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// Why a session create failed.
#[derive(Debug, thiserror::Error)]
pub enum CreateSessionError {
    /// Transport or decode failure.
    #[error(transparent)]
    Attach(#[from] AttachError),
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
    /// The mutation was answered with a legacy server error.
    ReadRefused(String),
    /// Intent committed, but no complete live/inspection evidence is available.
    /// Reconcile this receipt; never replace it with a fresh creation.
    Committed { receipt: Box<Committed> },
    /// The server refused the `SESSION_CREATE`.
    Refused(Refusal),
    /// The session was created, but its seed command did not start. The
    /// session stays, holding the failed launch.
    LaunchFailed {
        /// The seed pane's Terminal id.
        terminal: u64,
        /// Why, when the server recorded it.
        diagnostic: Option<LaunchDiagnostic>,
        /// The exact admitted operation and durable resource identities.
        receipt: Box<Committed>,
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
/// Name collision is refused atomically by the host. Interleaved degradation
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
                return Ok(CreateOutcome::Committed { receipt: committed });
            };
            let Some(seed_key) = committed.resource else {
                return Ok(CreateOutcome::Committed { receipt: committed });
            };
            let seed = match inspect_created_seed(conn, &committed, seed_key).await {
                Ok(seed) => seed,
                Err(_) => return Ok(CreateOutcome::Committed { receipt: committed }),
            };
            Ok(match seed {
                Seed::NoFailureObserved => CreateOutcome::Created(u64::from(terminal)),
                Seed::Failed(diagnostic) => CreateOutcome::LaunchFailed {
                    terminal: u64::from(terminal),
                    diagnostic,
                    receipt: committed,
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
    /// The lifecycle read exposed no recorded launch failure.
    NoFailureObserved,
    /// Its command did not start, with the server's reason.
    Failed(Option<LaunchDiagnostic>),
}

/// Inspect the exact seed after admission. Arrangement construction belongs to
/// the host's committed create, never a second client-side mutation.
async fn inspect_created_seed(
    conn: &mut Connection,
    receipt: &Committed,
    seed: ResourceKey,
) -> Result<Seed, crate::org::OrgError> {
    let (_, inventory) = crate::org::session_inventory_for_receipt(conn, receipt).await?;
    if let Some(activation) = inventory.resource(&seed)
        .and_then(|record| record.activation.as_ref())
        && activation.state == ExecState::Failed
    {
        return Ok(Seed::Failed(activation.diagnostic));
    }
    Ok(Seed::NoFailureObserved)
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
