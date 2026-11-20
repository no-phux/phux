//! Poll an authority's durable level state. No live-handle rebinding or shell I/O.
use std::{path::Path, time::Duration};

use phux_client_runtime::reconnect::Ladder;
use phux_protocol::api::{
    ErrorCode, ErrorDocument, ExecutionObservation, ExecutionResult, ExecutionResultRequest,
    ExecutionResultStatus, ExecutionState, MAX_EXECUTION_WAIT_SECS, OperationError,
    OperationOutcome, parse_hex16,
};
use phux_protocol::ids::{AuthorityId, ResourceKey};

use crate::{
    attach::connection::{Connection, Dial},
    deadline::Deadline,
    org::OrgError,
    resource::show::read_key_on,
};

const INTERVAL: Duration = Duration::from_millis(200);
const READ_BUDGET: Duration = Duration::from_secs(5);

/// An exact read could not be made. Neither error proves a process ended.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    /// Malformed request.
    #[error("{0}")]
    Invalid(&'static str),
    /// Malformed JSON request.
    #[error("{0}")]
    InvalidJson(serde_json::Error),
    /// This connection reached a different authority, even if the resource was absent.
    #[error("expected authority {expected}, connected to {actual}")]
    AuthorityMismatch {
        /// Authority named by the execution reference.
        expected: AuthorityId,
        /// Authority that answered the read.
        actual: AuthorityId,
    },
    /// The one-shot read did not obtain a response within its I/O budget.
    #[error("execution read timed out")]
    ReadTimedOut,
    /// Read failed, without an exact execution observation.
    #[error(transparent)]
    Organization(#[from] OrgError),
}

impl ExecutionError {
    /// Structured error shared by the CLI and MCP, without any mutation.
    #[must_use]
    pub fn document(&self) -> ErrorDocument {
        let code = match self {
            Self::Invalid(_) | Self::InvalidJson(_) => ErrorCode::InvalidArgument,
            Self::AuthorityMismatch { .. } => ErrorCode::AuthorityMismatch,
            Self::ReadTimedOut => ErrorCode::TimedOut,
            Self::Organization(error) => error.api_code(),
        };
        ErrorDocument {
            schema_version: 1,
            error: OperationError {
                code,
                outcome: OperationOutcome::NotSent,
                message: self.to_string(),
                idempotency_key: None,
            },
        }
    }
}

/// Read or wait for the exact retained activation on the supplied socket.
///
/// A wait redials after transport loss within the same deadline; it never
/// starts a server or submits a mutation. Each read rechecks the authority.
///
/// # Errors
///
/// Invalid identity/budget, wrong authority, unsupported read, denial or I/O.
pub async fn result(
    socket: &Path,
    request: &ExecutionResultRequest,
) -> Result<ExecutionResult, ExecutionError> {
    result_on(&Dial::uds(socket), request).await
}

/// Read or wait over an explicit authenticated route to the owner.
///
/// The route may use a remote transport; it must resolve to the authority in
/// the execution reference on every reconnect. There is no host fallback.
///
/// # Errors
///
/// As [`result`].
pub async fn result_on(
    dial: &Dial,
    request: &ExecutionResultRequest,
) -> Result<ExecutionResult, ExecutionError> {
    let (authority, resource, instance) = request
        .execution
        .validated()
        .map_err(ExecutionError::Invalid)?;
    let pin = Pin {
        authority,
        resource,
        instance,
        activation: request.execution.activation,
    };
    let (status, observation) = match request.timeout_secs {
        None => read_on(dial, &mut None, &pin).await?,
        Some(secs @ 1..=MAX_EXECUTION_WAIT_SECS) => {
            wait(dial, &pin, Duration::from_secs(u64::from(secs))).await?
        }
        Some(_) => return Err(ExecutionError::Invalid("timeout_secs must be in 1..=3600")),
    };
    Ok(ExecutionResult {
        schema_version: 1,
        execution: request.execution.clone(),
        status,
        observation,
    })
}

struct Pin {
    authority: AuthorityId,
    resource: ResourceKey,
    activation: u64,
    instance: [u8; 16],
}

type Level = (ExecutionResultStatus, Option<ExecutionObservation>);

async fn wait(dial: &Dial, pin: &Pin, duration: Duration) -> Result<Level, ExecutionError> {
    let budget = Deadline::new(Some(duration));
    let ladder = Ladder::AGENT_VERB;
    let mut backoff = ladder.floor;
    let mut conn = None;
    let mut last = None;
    while let Some(read) = budget.run(read_on(dial, &mut conn, pin)).await {
        let delay = match read {
            Ok(level) if !level.1.as_ref().is_some_and(active) => return Ok(level),
            Ok((_, observation)) => {
                last = observation;
                backoff = ladder.floor;
                INTERVAL
            }
            Err(error) if retry_read(&error) => {
                conn = None;
                let delay = backoff;
                backoff = ladder.next(backoff);
                delay
            }
            Err(error) => return Err(error),
        };
        if budget.run(tokio::time::sleep(delay)).await.is_none() {
            break;
        }
    }
    Ok((ExecutionResultStatus::TimedOut, last))
}

async fn read_on(
    dial: &Dial,
    conn: &mut Option<Connection>,
    pin: &Pin,
) -> Result<Level, ExecutionError> {
    let (actual, resource) = tokio::time::timeout(READ_BUDGET, async {
        let conn = match conn {
            Some(conn) => conn,
            slot => slot.insert(
                Connection::connect_dial(dial)
                    .await
                    .map_err(OrgError::Attach)?,
            ),
        };
        read_key_on(conn, pin.resource).await
    })
    .await
    .map_err(|_| ExecutionError::ReadTimedOut)??;
    if actual != pin.authority {
        return Err(ExecutionError::AuthorityMismatch {
            expected: pin.authority,
            actual,
        });
    }
    let observation = resource
        .and_then(|record| record.activation)
        .map(crate::terminal::observation);
    Ok(classify(pin.activation, pin.instance, observation))
}

fn retry_read(error: &ExecutionError) -> bool {
    match error {
        ExecutionError::Organization(OrgError::Attach(crate::attach::AttachError::Io(error)))
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidInput
            ) =>
        {
            false
        }
        ExecutionError::Organization(OrgError::Attach(error)) => {
            error.is_transport_loss() && !error.is_fatal_refusal()
        }
        // Organization historically represents an EOF after a sent read this
        // way. There was no mutation to recover; redial and read again.
        ExecutionError::Organization(OrgError::OutcomeUnknown { operation: None })
        | ExecutionError::ReadTimedOut => true,
        _ => false,
    }
}

const fn active(observation: &ExecutionObservation) -> bool {
    matches!(
        observation.state,
        ExecutionState::Starting | ExecutionState::Running | ExecutionState::Stopping
    ) || observation.persistence_pending
}

fn classify(
    activation: u64,
    instance: [u8; 16],
    observation: Option<ExecutionObservation>,
) -> Level {
    use ExecutionResultStatus as Status;
    match observation {
        Some(observed) if observed.activation > activation => (Status::Superseded, None),
        Some(observed)
            if observed.activation == activation
                && parse_hex16(&observed.instance) == Some(instance) =>
        {
            (Status::Observed, Some(observed))
        }
        _ => (Status::Unavailable, None),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use super::*;
    use phux_protocol::{
        api::{ExecutionOutcome, ExecutionRef, hex16},
        caps::ServerFeatureExt,
        wire::frame::{Command, FrameKind, SessionLifetime},
    };

    #[tokio::test]
    async fn an_exact_wait_redials_after_transport_loss() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("exec.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let mut org = crate::testkit::OrgFixture::new();
        let session = org.session("work", SessionLifetime::Persistent);
        let resource = org.ended_by_restart(session);
        let reference = ExecutionRef {
            authority: org.inventory().authority.to_string(),
            resource: resource.to_string(),
            activation: 1,
            instance: hex16(org.instance().as_bytes()),
        };
        let spec = crate::testkit::ScriptSpec::new()
            .organization(org)
            .server_features_ext(phux_protocol::caps::ServerFeatureExtSet::with(&[
                ServerFeatureExt::Organization,
                ServerFeatureExt::DurableResourceInspection,
            ]));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            drop(stream);
            crate::testkit::ScriptedServer::accept(&listener, spec).await
        });
        let observed = result(
            &socket,
            &ExecutionResultRequest {
                execution: reference,
                timeout_secs: Some(5),
            },
        )
        .await
        .unwrap();
        assert_eq!(observed.status, ExecutionResultStatus::Observed);
        assert_eq!(
            observed.observation.unwrap().outcome,
            Some(ExecutionOutcome::Unobserved)
        );
        assert!(matches!(
            server.await.unwrap().as_slice(),
            [
                FrameKind::Hello { .. },
                FrameKind::Command {
                    command: Command::GetOrganization {
                        filter: phux_protocol::wire::frame::OrgFilter::Resource(key),
                        ..
                    },
                    ..
                }
            ] if *key == resource
        ));
    }

    #[test]
    fn only_the_exact_activation_can_report_an_exit() {
        let observed = ExecutionObservation {
            activation: 2,
            instance: hex16(&[2; 16]),
            state: ExecutionState::Completed,
            outcome: Some(ExecutionOutcome::Exited { code: 7 }),
            diagnostic: None,
            persistence_pending: false,
        };
        for (pin, instance, status) in [
            (1, [2; 16], ExecutionResultStatus::Superseded),
            (2, [1; 16], ExecutionResultStatus::Unavailable),
            (3, [2; 16], ExecutionResultStatus::Unavailable),
        ] {
            assert_eq!(
                classify(pin, instance, Some(observed.clone())),
                (status, None)
            );
        }
        assert_eq!(
            classify(2, [2; 16], Some(observed.clone())),
            (ExecutionResultStatus::Observed, Some(observed))
        );
    }

    #[test]
    fn read_eof_can_retry_but_permanent_denial_cannot() {
        use crate::attach::AttachError;
        assert!(retry_read(
            &OrgError::OutcomeUnknown { operation: None }.into()
        ));
        assert!(!retry_read(
            &OrgError::Attach(AttachError::Io(std::io::ErrorKind::PermissionDenied.into())).into()
        ));
    }
}
