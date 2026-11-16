//! Approved, single-delivery credentials over an already negotiated connection.
//!
//! Requesters consume secrets in memory; providers fetch approved jobs and complete
//! them on the same connection. No operation retries an uncertain exchange.

use std::time::Duration;

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::credentials::CredentialOp;
pub use phux_protocol::credentials::{CredentialRequest, SecretValues};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode};
use serde::Deserialize;

use crate::attach::{AttachError, connection::Connection};

/// Maximum size of an individual secret, in UTF-8 bytes.
pub const MAX_SECRET_BYTES: usize = phux_protocol::credentials::MAX_FIELD_BYTES;
/// Maximum combined size of one request's secrets.
pub const MAX_TOTAL_BYTES: usize = phux_protocol::credentials::MAX_SECRET_BYTES;

/// An approved job claimed by a provider. It contains references, never values.
#[derive(Debug, Deserialize)]
pub struct ProviderJob {
    /// One-time completion identifier, bound to the claiming connection.
    pub id: String,
    /// The approved provider, references, and purpose.
    pub request: CredentialRequest,
    /// The requesting actor as reported by the server.
    pub requester: serde_json::Value,
}

/// A credential exchange failure. Unexpected replies are never debug-printed.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// Nothing was sent to a server without the credential broker feature.
    #[error("the server does not support the credential broker")]
    Unsupported,
    /// Local request validation failed before sending.
    #[error("{0}")]
    Invalid(String),
    /// The server refused the exchange.
    #[error("credential broker refused the operation: {message}")]
    Refused {
        /// Structured refusal; a spent completion need not terminate a provider.
        code: ErrorCode,
        /// Payload-free server diagnostic.
        message: String,
    },
    /// The reply was missing its expected, bounded payload.
    #[error("invalid credential broker reply")]
    Malformed,
    /// An exchange timed out. It is not retried.
    /// Discard the connection: a late reply may still carry this request's id.
    #[error("credential broker exchange timed out; not retried")]
    Timeout,
    /// The connection failed.
    #[error(transparent)]
    Transport(#[from] AttachError),
}

/// Request credentials, waiting for mandatory human approval and provider completion.
/// The returned values are ordered exactly like the references; never print them.
///
/// # Errors
/// Invalid input, an unsupported server, refusal, malformed values, or a failed
/// exchange. A lost reply is not retried: secrets are delivered at most once.
/// Discard the connection after a timeout or transport failure.
pub async fn request(
    conn: &mut Connection,
    request: CredentialRequest,
) -> Result<SecretValues, CredentialError> {
    request.validate().map_err(CredentialError::Invalid)?;
    let count = request.references.len();
    match exchange(
        conn,
        CredentialOp::Request(request),
        Duration::from_secs(125),
    )
    .await?
    {
        CommandResult::OkWith(CommandValue::CredentialSecrets(values)) => {
            validate_values(&values, count)?;
            Ok(values)
        }
        _ => Err(CredentialError::Malformed),
    }
}

/// Claim the next approved request for `provider` on this connection.
///
/// # Errors
/// Unsupported server, refusal, malformed job, or failed exchange. A job must be
/// completed using this same connection; do not retry a lost claim.
pub async fn next(
    conn: &mut Connection,
    provider: &str,
) -> Result<Option<ProviderJob>, CredentialError> {
    match exchange(
        conn,
        CredentialOp::Next {
            provider: provider.to_owned(),
        },
        Duration::from_secs(10),
    )
    .await?
    {
        CommandResult::Ok => Ok(None),
        CommandResult::OkWith(CommandValue::Json(json)) => {
            let job: ProviderJob =
                serde_json::from_str(&json).map_err(|_| CredentialError::Malformed)?;
            job.request
                .validate()
                .map_err(|_| CredentialError::Malformed)?;
            if job.request.provider != provider || job.id.is_empty() {
                return Err(CredentialError::Malformed);
            }
            Ok(Some(job))
        }
        _ => Err(CredentialError::Malformed),
    }
}

/// Complete a claimed job; `None` reports provider failure without secret output.
///
/// # Errors
/// Unsupported server, refusal, malformed acknowledgement, or failed exchange.
/// An uncertain completion is never retried.
pub async fn complete(
    conn: &mut Connection,
    id: String,
    values: Option<SecretValues>,
) -> Result<(), CredentialError> {
    if let Some(values) = &values {
        validate_values(values, values.0.len())?;
    }
    match exchange(
        conn,
        CredentialOp::Complete { id, values },
        Duration::from_secs(10),
    )
    .await?
    {
        CommandResult::Ok => Ok(()),
        _ => Err(CredentialError::Malformed),
    }
}

async fn exchange(
    conn: &mut Connection,
    op: CredentialOp,
    timeout: Duration,
) -> Result<CommandResult, CredentialError> {
    op.validate().map_err(CredentialError::Invalid)?;
    let features = crate::state::probe_hello_features_ext(conn).await?;
    if !features.contains(ServerFeatureExt::Credentials) {
        return Err(CredentialError::Unsupported);
    }
    let reply = tokio::time::timeout(timeout, conn.request(1, Command::Credentials { op }))
        .await
        .map_err(|_| CredentialError::Timeout)??;
    match reply.into_parts().0 {
        CommandResult::Error { code, message } => Err(CredentialError::Refused { code, message }),
        result => Ok(result),
    }
}

fn validate_values(values: &SecretValues, count: usize) -> Result<(), CredentialError> {
    values.validate().map_err(|_| CredentialError::Malformed)?;
    if values.0.len() != count {
        return Err(CredentialError::Malformed);
    }
    if values.0.iter().any(|value| value.len() > MAX_SECRET_BYTES) {
        return Err(CredentialError::Malformed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test setup and assertions")]

    use super::*;

    #[tokio::test]
    async fn unsupported_peers_receive_no_credential_commands() {
        use crate::testkit::{ScriptSpec, ScriptedServer};
        use phux_protocol::wire::frame::FrameKind;

        let (client, server) = tokio::net::UnixStream::pair().expect("stream pair");
        let peer = tokio::spawn(ScriptedServer::on_stream(server, ScriptSpec::new()).run());
        let mut conn = Connection::from_stream(client);
        assert!(matches!(
            next(&mut conn, "desktop").await,
            Err(CredentialError::Unsupported)
        ));
        drop(conn);
        let seen = peer.await.expect("scripted peer");
        assert!(matches!(seen.as_slice(), [FrameKind::Hello { .. }]));
    }

    #[test]
    fn values_are_counted_and_bounded_without_printing_them() {
        assert!(validate_values(&SecretValues(vec!["value".into()]), 1).is_ok());
        assert!(validate_values(&SecretValues(vec![]), 1).is_err());
        assert!(validate_values(&SecretValues(vec!["a\0b".into()]), 1).is_err());
        assert!(validate_values(&SecretValues(vec!["a".repeat(MAX_SECRET_BYTES + 1)]), 1).is_err());
        assert!(validate_values(&SecretValues(vec!["a".repeat(MAX_SECRET_BYTES); 3]), 3).is_err());
    }
}
