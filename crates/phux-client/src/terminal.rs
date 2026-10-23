//! One shared create/place/execute operation for CLI, MCP and Rust callers.
//! There is no client-side placement publication or compensating process kill.
use crate::{
    attach::connection::Connection,
    org::{self, OrgError},
};
use phux_protocol::{
    api,
    caps::ServerFeatureExt,
    wire::frame::{
        ActivationInfo, Command, CommandResult, CommandValue, Committed, ExecState, Outcome,
    },
};
use std::path::Path;

pub use phux_protocol::api::{
    CreatedPlacement, ExecutionObservation, ExecutionOutcome, ExecutionState, SplitAxis,
    TerminalCreate, TerminalCreated, TerminalOperation, TerminalPlacement,
};

#[cfg(test)]
mod tests;

/// A creation failure, retaining the difference between not sent, unknown and committed.
#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    /// Invalid local intention; nothing sent.
    #[error("{0}")]
    Invalid(String),
    /// Typed refusal or uncertain transport outcome from the shared organization path.
    #[error(transparent)]
    Organization(#[from] OrgError),
    /// A failed receipt read says nothing about whether the original creation ran.
    #[error(transparent)]
    Lookup(OrgError),
    /// Intent committed, but this client cannot interpret its complete receipt.
    #[error("intent committed but the receipt is incomplete: {0}")]
    Incomplete(String),
    /// No retained receipt is available. This does not prove the operation never ran.
    #[error(
        "creation receipt is not retained; outcome unknown, reconcile records instead of replaying"
    )]
    NotRetained,
}
impl CreateError {
    /// Stable machine-readable error document. Always preserves the supplied retry identity.
    #[must_use]
    pub fn document(&self, key: &str) -> serde_json::Value {
        use api::{ErrorCode as C, OperationOutcome as O};
        let (code, outcome) = match self {
            Self::Invalid(_) => (C::InvalidArgument, O::NotSent),
            Self::Incomplete(_) => (C::IncompleteReceipt, O::Committed),
            Self::NotRetained => (C::ReceiptNotRetained, O::Unknown),
            Self::Organization(OrgError::OutcomeUnknown { .. }) => (C::OutcomeUnknown, O::Unknown),
            Self::Organization(error @ (OrgError::Unsupported | OrgError::Attach(_))) => {
                (error.api_code(), O::NotSent)
            }
            Self::Organization(error @ (OrgError::Refused(_) | OrgError::Server { .. })) => {
                (error.api_code(), O::Refused)
            }
            Self::Lookup(error) | Self::Organization(error) => (error.api_code(), O::Unknown),
        };
        serde_json::to_value(api::ErrorDocument {
            schema_version: 1,
            error: api::OperationError {
                code,
                outcome,
                message: self.to_string(),
                idempotency_key: Some(key.to_owned()),
            },
        })
        .unwrap_or_default()
    }
}

/// Create, place and start a terminal on an already negotiated connection.
/// Retains the durable resource and failed/completed execution until explicit removal.
///
/// # Errors
/// Refuses before sending without `CREATE_PLACEMENT`. A lost/unreadable reply is
/// unknown, never permission to repeat the effect with a new key.
pub async fn create(
    conn: &mut Connection,
    request: &TerminalCreate,
) -> Result<TerminalCreated, CreateError> {
    let command = request.command().map_err(CreateError::Invalid)?;
    let receipt = org::commit(conn, 1, command).await?;
    project(&request.session, &request.idempotency_key, &receipt)
}

/// The same intention over one fresh connection. Never auto-starts a server.
///
/// # Errors
/// Invalid local request, connection failure, refusal, unknown outcome or incomplete committed receipt.
pub async fn create_on(
    socket: &Path,
    request: &TerminalCreate,
) -> Result<TerminalCreated, CreateError> {
    // Validate before dialing, so malformed requests cannot be confused with unavailable servers.
    request.command().map_err(CreateError::Invalid)?;
    let mut conn = Connection::connect(socket)
        .await
        .map_err(|error| CreateError::Organization(OrgError::Attach(error)))?;
    create(&mut conn, request).await
}

fn project(session: &str, key: &str, receipt: &Committed) -> Result<TerminalCreated, CreateError> {
    // A queued or mismatched acknowledgement is not evidence for this intention.
    if !api::hex16(receipt.operation.as_bytes()).eq_ignore_ascii_case(key) {
        return Err(OrgError::OutcomeUnknown { operation: None }.into());
    }
    let missing = |field: &str| {
        CreateError::Incomplete(format!(
            "missing {field}; reconcile the retained idempotency key"
        ))
    };
    Ok(TerminalCreated {
        schema_version: 1,
        idempotency_key: api::hex16(receipt.operation.as_bytes()),
        authority: receipt
            .authority
            .ok_or_else(|| missing("authority"))?
            .to_string(),
        session: session.to_ascii_lowercase(),
        resource: receipt
            .resource
            .ok_or_else(|| missing("resource"))?
            .to_string(),
        placement: CreatedPlacement {
            arrangement: receipt
                .arrangement
                .ok_or_else(|| missing("arrangement"))?
                .to_string(),
            entry: api::hex16(&receipt.entry.ok_or_else(|| missing("entry"))?),
        },
        execution: receipt.activation.map(observation),
        terminal_id: receipt
            .target
            .as_ref()
            .and_then(|target| target.resource.local_id()),
        replayed: receipt.replayed,
    })
}

pub(crate) fn observation(activation: ActivationInfo) -> ExecutionObservation {
    let state = match activation.state {
        ExecState::Starting => ExecutionState::Starting,
        ExecState::Running => ExecutionState::Running,
        ExecState::Stopping => ExecutionState::Stopping,
        ExecState::Completed => ExecutionState::Completed,
        ExecState::Failed => ExecutionState::Failed,
        ExecState::Lost => ExecutionState::Lost,
        ExecState::Abandoned => ExecutionState::Abandoned,
    };
    let outcome = activation.outcome.map(|outcome| match outcome {
        Outcome::Exited(code) => ExecutionOutcome::Exited { code },
        Outcome::Signaled(signal) => ExecutionOutcome::Signaled { signal },
        _ => ExecutionOutcome::Unobserved,
    });
    ExecutionObservation {
        activation: activation.id.get(),
        instance: api::hex16(activation.instance.as_bytes()),
        state,
        outcome,
        diagnostic: activation.diagnostic,
        persistence_pending: activation.persistence_pending,
    }
}

/// Read a retained creation receipt without repeating any effect.
///
/// # Errors
/// A missing/expired receipt is [`CreateError::NotRetained`], not evidence the
/// original operation never ran. Unsupported peers fail before sending.
pub async fn operation(
    conn: &mut Connection,
    request: &api::TerminalOperation,
) -> Result<TerminalCreated, CreateError> {
    let command = request.command().map_err(CreateError::Invalid)?;
    if !conn
        .server_features_ext()
        .contains(ServerFeatureExt::ApiDescription)
    {
        return Err(CreateError::Lookup(OrgError::Unsupported));
    }
    match org::request(conn, 1, command)
        .await
        .map_err(CreateError::Lookup)?
        .0
    {
        org::OrgOutcome::Committed(receipt) => {
            project(&request.session, &request.idempotency_key, &receipt)
        }
        org::OrgOutcome::Refused(phux_protocol::wire::frame::Refusal::NotFound) => {
            Err(CreateError::NotRetained)
        }
        org::OrgOutcome::Refused(refusal) => Err(CreateError::Lookup(OrgError::Refused(refusal))),
        org::OrgOutcome::Error { code, message } => {
            Err(CreateError::Lookup(OrgError::Server { code, message }))
        }
        _ => Err(CreateError::Lookup(OrgError::OutcomeUnknown {
            operation: None,
        })),
    }
}

/// Lookup one creation over a fresh connection, never resending a mutation.
///
/// # Errors
/// As [`operation`], plus connection failures.
pub async fn operation_on(
    socket: &Path,
    request: &api::TerminalOperation,
) -> Result<TerminalCreated, CreateError> {
    request.command().map_err(CreateError::Invalid)?;
    let mut conn = Connection::connect(socket)
        .await
        .map_err(|error| CreateError::Lookup(OrgError::Attach(error)))?;
    operation(&mut conn, request).await
}

/// Ask the connected server for the workflows and nested schemas it implements.
/// This does not infer unknown methods from this client's compiled catalog, nor authorize them.
///
/// # Errors
/// Unsupported server, transport/refusal, or malformed description. Nothing is mutated.
pub async fn describe(conn: &mut Connection) -> Result<serde_json::Value, OrgError> {
    if !conn
        .server_features_ext()
        .contains(ServerFeatureExt::ApiDescription)
    {
        return Err(OrgError::Unsupported);
    }
    let reply = conn
        .request(1, Command::DescribeApi)
        .await
        .map_err(OrgError::Attach)?;
    match reply.into_parts().0 {
        CommandResult::OkWith(CommandValue::Json(text)) => {
            serde_json::from_str(&text).map_err(|error| OrgError::Unexpected(error.to_string()))
        }
        CommandResult::Error { code, message } => Err(OrgError::Server { code, message }),
        _ => Err(OrgError::Unexpected(
            "DESCRIBE_API returned a non-description".into(),
        )),
    }
}

/// Read the server's description over a fresh connection.
///
/// # Errors
/// As [`describe`], plus connection failure.
pub async fn describe_on(socket: &Path) -> Result<serde_json::Value, OrgError> {
    let mut conn = Connection::connect(socket)
        .await
        .map_err(OrgError::Attach)?;
    describe(&mut conn).await
}
