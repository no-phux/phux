//! Host-service HTTP over the connection (`HOST_SERVICE_HTTP`), shaped for
//! a native HTTP adapter (iOS `URLProtocol`, Android OkHttp): open answers
//! the response head, repeated reads stream the body, and close (or a
//! cancel that calls it) ends the exchange. Each call is async and resolves
//! once the server answers; nothing is sent when the server lacks the
//! capability (`negotiated_features` names it `service-http`).

use phux_client_runtime::control::ServiceHttpError;
use phux_protocol::service_http::{
    MAX_SERVICE_HTTP_READ_BYTES, ServiceHttpChunk, ServiceHttpHead, ServiceHttpRequest,
};
use phux_protocol::wire::frame::ErrorCode;

use super::RemoteClient;

/// One HTTP header, in order; names may repeat.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireServiceHttpHeader {
    pub name: String,
    pub value: String,
}

/// One request to a service on the connected host (or on the satellite
/// `host` names). `target` is the path and query, starting with `/`. The
/// server owns `Host`, `Authorization`, cookies, and framing headers.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireServiceHttpRequest {
    pub host: Option<String>,
    pub service: String,
    pub method: String,
    pub target: String,
    pub headers: Vec<WireServiceHttpHeader>,
    pub body: Vec<u8>,
}

/// The response head; read the body from `exchange`, then close it.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireServiceHttpHead {
    pub exchange: u64,
    pub status: u16,
    pub headers: Vec<WireServiceHttpHeader>,
}

/// Body bytes in order. Empty with `end == false` means nothing arrived
/// before the server's idle deadline: read again (event streams). `end`
/// also ends the exchange; closing it after is a no-op.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireServiceHttpChunk {
    pub bytes: Vec<u8>,
    pub end: bool,
}

/// Why a service call has no value.
#[derive(uniffi::Error, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WireServiceHttpError {
    /// Nothing was sent: no connection is established.
    #[error("not connected")]
    NotConnected,
    /// Nothing was sent: the server does not serve host services.
    #[error("this server does not serve host services")]
    Unsupported,
    /// Too many calls or exchanges are open; finish or close some.
    #[error("{reason}")]
    Busy { reason: String },
    /// The request is malformed or exceeds the wire bounds.
    #[error("{reason}")]
    Invalid { reason: String },
    /// The connection ended before the answer; exchanges do not survive it.
    #[error("the connection ended before the server answered")]
    Disconnected,
    /// No such service on that host, or the exchange is not open (closed,
    /// or from an earlier connection).
    #[error("{reason}")]
    NotFound { reason: String },
    /// The service exists but cannot answer now (not installed, or not
    /// starting); `reason` says which.
    #[error("{reason}")]
    Unavailable { reason: String },
    /// The connection's grant does not allow `INPUT` on the host.
    #[error("{reason}")]
    PermissionDenied { reason: String },
    /// Any other refusal, with its wire error code.
    #[error("{reason}")]
    Refused { code: u16, reason: String },
}

impl From<ServiceHttpError> for WireServiceHttpError {
    fn from(error: ServiceHttpError) -> Self {
        let reason = error.to_string();
        match error {
            ServiceHttpError::NotConnected => Self::NotConnected,
            ServiceHttpError::Unsupported => Self::Unsupported,
            ServiceHttpError::Busy => Self::Busy { reason },
            ServiceHttpError::Invalid(reason) => Self::Invalid { reason },
            ServiceHttpError::Disconnected => Self::Disconnected,
            ServiceHttpError::Refused { code, message } => refused(code, message),
        }
    }
}

fn refused(code: ErrorCode, reason: String) -> WireServiceHttpError {
    match code {
        ErrorCode::NotFound => WireServiceHttpError::NotFound { reason },
        ErrorCode::Unavailable => WireServiceHttpError::Unavailable { reason },
        ErrorCode::InvalidCommand => WireServiceHttpError::Invalid { reason },
        ErrorCode::ResourceExhausted => WireServiceHttpError::Busy { reason },
        ErrorCode::PermissionDenied => WireServiceHttpError::PermissionDenied { reason },
        other => WireServiceHttpError::Refused {
            code: other.as_wire(),
            reason,
        },
    }
}

fn headers(headers: Vec<(String, String)>) -> Vec<WireServiceHttpHeader> {
    headers
        .into_iter()
        .map(|(name, value)| WireServiceHttpHeader { name, value })
        .collect()
}

impl From<WireServiceHttpRequest> for ServiceHttpRequest {
    fn from(request: WireServiceHttpRequest) -> Self {
        Self {
            host: request.host,
            service: request.service,
            method: request.method,
            target: request.target,
            headers: request
                .headers
                .into_iter()
                .map(|header| (header.name, header.value))
                .collect(),
            body: request.body,
        }
    }
}

impl From<ServiceHttpHead> for WireServiceHttpHead {
    fn from(head: ServiceHttpHead) -> Self {
        Self {
            exchange: head.exchange,
            status: head.status,
            headers: headers(head.headers),
        }
    }
}

impl From<ServiceHttpChunk> for WireServiceHttpChunk {
    fn from(chunk: ServiceHttpChunk) -> Self {
        Self {
            bytes: chunk.bytes,
            end: chunk.end,
        }
    }
}

/// The most bytes one `service_http_read` may ask for.
#[uniffi::export]
pub fn service_http_max_read_bytes() -> u32 {
    u32::try_from(MAX_SERVICE_HTTP_READ_BYTES).unwrap_or(u32::MAX)
}

#[uniffi::export]
impl RemoteClient {
    /// Send one request to a host service and wait for its response head.
    pub async fn service_http_open(
        &self,
        request: WireServiceHttpRequest,
    ) -> Result<WireServiceHttpHead, WireServiceHttpError> {
        let client = self
            .runtime_client()
            .ok_or(WireServiceHttpError::NotConnected)?;
        Ok(client.service_http_open(request.into()).await?.into())
    }

    /// Read the next body bytes of `exchange`, at most `max_bytes`
    /// (`1..=service_http_max_read_bytes()`). Resolves with at least one
    /// byte, the end, or, after the server's idle deadline, nothing.
    pub async fn service_http_read(
        &self,
        exchange: u64,
        max_bytes: u32,
    ) -> Result<WireServiceHttpChunk, WireServiceHttpError> {
        let client = self
            .runtime_client()
            .ok_or(WireServiceHttpError::NotConnected)?;
        Ok(client.service_http_read(exchange, max_bytes).await?.into())
    }

    /// Close `exchange` and release its upstream connection; a read in
    /// flight resolves `NotFound`. Idempotent: closing twice succeeds.
    pub async fn service_http_close(&self, exchange: u64) -> Result<(), WireServiceHttpError> {
        let client = self
            .runtime_client()
            .ok_or(WireServiceHttpError::NotConnected)?;
        Ok(client.service_http_close(exchange).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_refusals_keep_their_meaning() {
        let cases = [
            (ErrorCode::NotFound, "NotFound"),
            (ErrorCode::Unavailable, "Unavailable"),
            (ErrorCode::InvalidCommand, "Invalid"),
            (ErrorCode::ResourceExhausted, "Busy"),
            (ErrorCode::PermissionDenied, "PermissionDenied"),
            (ErrorCode::UnsupportedSatelliteRoute, "Refused"),
        ];
        for (code, variant) in cases {
            let error = WireServiceHttpError::from(ServiceHttpError::Refused {
                code,
                message: "why".to_owned(),
            });
            assert!(
                format!("{error:?}").starts_with(variant),
                "{code:?} -> {error:?}"
            );
            assert_eq!(error.to_string(), "why");
        }
        assert_eq!(
            WireServiceHttpError::from(ServiceHttpError::Disconnected),
            WireServiceHttpError::Disconnected
        );
    }
}
