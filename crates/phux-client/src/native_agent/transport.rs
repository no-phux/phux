//! Guardian registration and native command transport.

use std::path::Path;

use phux_protocol::native_agent::{NativeRequest, NativeRequestPayload};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode};
use serde::{Deserialize, de::DeserializeOwned};

use crate::attach::connection::Connection;
use crate::attach::outcome::AttachError;

/// Native controller failures, keeping transport ambiguity separate from refusal.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The socket failed; publication/completion can retry the same payload.
    #[error("native controller transport: {0}")]
    Transport(#[source] AttachError),
    /// A publication, completion, or close was temporarily refused by a sealed
    /// or capacity-bound actor. Retry the exact payload after backoff.
    #[error("native controller mutation temporarily busy: {message}")]
    Busy {
        /// The broker's unchanged transient explanation.
        message: String,
    },
    /// The broker refused this exact request. Do not treat it as a disconnect.
    #[error("native controller refused ({code:?}): {message}")]
    Refused {
        /// The broker's structured refusal code.
        code: ErrorCode,
        /// The broker's unchanged explanation.
        message: String,
    },
    /// This controller or its resource has closed permanently.
    #[error("native controller closed: {message}")]
    Closed {
        /// The local or broker close explanation.
        message: String,
    },
    /// The request is invalid or the bounded pending publication queue is full.
    #[error("invalid native controller request: {0}")]
    Invalid(String),
    /// An unexpected response; acceptance cannot be inferred from it.
    #[error("native controller protocol: {0}")]
    Protocol(String),
}

impl ClientError {
    /// Transport loss or transient controller backpressure permits an identical retry.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::Busy { .. })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    generation: u64,
}

pub(super) fn set_generation(
    request: NativeRequestPayload,
    current: u64,
) -> Result<NativeRequestPayload, ClientError> {
    let generation = match request.request() {
        NativeRequest::Complete { generation, .. }
        | NativeRequest::Publish { generation, .. }
        | NativeRequest::Close { generation, .. } => *generation,
        _ => return Ok(request),
    };
    if generation == current {
        return Ok(request);
    }
    let mut request = request.into_request();
    match &mut request {
        NativeRequest::Complete { generation, .. }
        | NativeRequest::Publish { generation, .. }
        | NativeRequest::Close { generation, .. } => *generation = current,
        _ => {}
    }
    NativeRequestPayload::new(request).map_err(ClientError::Invalid)
}

pub(super) fn transport_error(error: AttachError) -> ClientError {
    if error.is_fatal_refusal() {
        return ClientError::Refused {
            code: ErrorCode::PermissionDenied,
            message: error.to_string(),
        };
    }
    match error {
        AttachError::Protocol(message) => ClientError::Protocol(message),
        AttachError::Framing(error) => ClientError::Protocol(error.to_string()),
        AttachError::Refused(message) => ClientError::Refused {
            code: ErrorCode::PermissionDenied,
            message,
        },
        AttachError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            ClientError::Refused {
                code: ErrorCode::PermissionDenied,
                message: error.to_string(),
            }
        }
        AttachError::Io(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            ClientError::Invalid(error.to_string())
        }
        error => ClientError::Transport(error),
    }
}

/// Return a usable transport only after validating the broker's generation.
pub(super) async fn register_connection(
    socket: &Path,
    request_id: u32,
    request: NativeRequestPayload,
) -> Result<(Connection, u64), ClientError> {
    let mut connection = Connection::connect(socket).await.map_err(transport_error)?;
    let registration: Registration =
        json_reply(round_trip(&mut connection, request_id, request).await?)?;
    if registration.generation == 0 {
        return Err(ClientError::Protocol(
            "registration returned a zero generation".into(),
        ));
    }
    Ok((connection, registration.generation))
}

pub(super) async fn round_trip(
    connection: &mut Connection,
    request_id: u32,
    request: NativeRequestPayload,
) -> Result<CommandResult, ClientError> {
    let mutation = matches!(
        request.request(),
        NativeRequest::Publish { .. }
            | NativeRequest::Complete { .. }
            | NativeRequest::Close { .. }
    );
    let (result, _interleaved) = connection
        .request(request_id, Command::NativeAgent { request })
        .await
        .map_err(transport_error)?
        .into_parts();
    // These connections subscribe to no resource output. Their only owned lane
    // is the correlated native reply; no transcript evidence is discarded here.
    match result {
        CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message,
        } => Err(ClientError::Closed { message }),
        CommandResult::Error {
            code: ErrorCode::Overflow,
            message,
        } if mutation => Err(ClientError::Busy { message }),
        CommandResult::Error { code, message } => Err(ClientError::Refused { code, message }),
        result => Ok(result),
    }
}

pub(super) fn json_reply<T: DeserializeOwned>(result: CommandResult) -> Result<T, ClientError> {
    match result {
        CommandResult::OkWith(CommandValue::Json(json)) => serde_json::from_str(&json)
            .map_err(|error| ClientError::Protocol(format!("invalid native reply: {error}"))),
        _ => Err(ClientError::Protocol("expected a native JSON reply".into())),
    }
}

pub(super) fn ok_reply(result: &CommandResult) -> Result<(), ClientError> {
    match result {
        CommandResult::Ok => Ok(()),
        _ => Err(ClientError::Protocol(
            "expected a native completion acknowledgement".into(),
        )),
    }
}
