//! HTTP exchanges with a named host service, carried over the phux
//! connection (`features_ext.HOST_SERVICE_HTTP`).
//!
//! A client opens one exchange with `SERVICE_HTTP_OPEN` (a whole request,
//! answered once the upstream response head arrives with
//! `CommandValue::ServiceHttpHead`), pulls the response body with repeated
//! `SERVICE_HTTP_READ`s (each answered with `CommandValue::ServiceHttpChunk`)
//! until the chunk that ends it, or ends it early with `SERVICE_HTTP_CLOSE`.
//! The server names the services it serves (`opencode`); it owns the
//! upstream origin and credential, so the client never holds either. Every value here is a positional record whose
//! byte bounds the decoder enforces with
//! [`DecodeError::ServiceHttpLimitExceeded`](crate::wire::DecodeError::ServiceHttpLimitExceeded).

use crate::wire::frame::MAX_FILE_UPLOAD_CHUNK;

/// Largest request body one `SERVICE_HTTP_OPEN` carries, and the most bytes
/// one `SERVICE_HTTP_READ` may ask for.
pub const MAX_SERVICE_HTTP_BODY_BYTES: usize = MAX_FILE_UPLOAD_CHUNK;
/// Most headers one request or response head carries.
pub const MAX_SERVICE_HTTP_HEADERS: usize = 128;
/// Most bytes of one head: host, service, method, target, and every header
/// name and value, summed.
pub const MAX_SERVICE_HTTP_HEAD_BYTES: usize = 64 * 1024;

/// One header: name and value, in order. Names repeat as HTTP allows.
pub type ServiceHttpHeader = (String, String);

/// One request to a host service (`SERVICE_HTTP_OPEN`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceHttpRequest {
    /// A satellite of the serving hub that runs the service; `None` is the
    /// serving server itself.
    pub host: Option<String>,
    /// The service the server names, e.g. `opencode`.
    pub service: String,
    /// The HTTP method, e.g. `GET`.
    pub method: String,
    /// The origin-form target: path and query, starting with `/`.
    pub target: String,
    /// Request headers. The server drops hop-by-hop headers and the ones it
    /// owns (`Host`, `Authorization`, `Cookie`, `Proxy-*`, framing).
    pub headers: Vec<ServiceHttpHeader>,
    /// The whole request body, at most [`MAX_SERVICE_HTTP_BODY_BYTES`].
    pub body: Vec<u8>,
}

/// The upstream response head (`CommandValue::ServiceHttpHead`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceHttpHead {
    /// The exchange to read the body from and close. Never reused by the
    /// server that issued it.
    pub exchange: u64,
    /// The HTTP status code.
    pub status: u16,
    /// Response headers, without hop-by-hop headers or `Set-Cookie`.
    pub headers: Vec<ServiceHttpHeader>,
}

/// One piece of a response body (`CommandValue::ServiceHttpChunk`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceHttpChunk {
    /// Body bytes, in order. Empty with `end: false` when the read's idle
    /// deadline passed with nothing new: read again.
    pub bytes: Vec<u8>,
    /// The body is complete; nothing follows these bytes, and the exchange
    /// is over (closing it again is a no-op).
    pub end: bool,
}

/// Whether `request` fits the wire bounds a decoder enforces.
#[must_use]
pub fn request_fits(request: &ServiceHttpRequest) -> bool {
    let fields = [
        request.host.as_deref().unwrap_or_default(),
        &request.service,
        &request.method,
        &request.target,
    ];
    request.body.len() <= MAX_SERVICE_HTTP_BODY_BYTES && parts_fit(&fields, &request.headers)
}

/// Whether a response head fits the wire bounds a decoder enforces.
#[must_use]
pub fn head_fits(head: &ServiceHttpHead) -> bool {
    parts_fit(&[], &head.headers)
}

/// Whether a chunk fits the wire bounds a decoder enforces.
#[must_use]
pub const fn chunk_fits(chunk: &ServiceHttpChunk) -> bool {
    chunk.bytes.len() <= MAX_SERVICE_HTTP_BODY_BYTES
}

/// Whether a `SERVICE_HTTP_READ` asks for no more than one chunk may carry.
#[must_use]
pub fn read_fits(max_bytes: u32) -> bool {
    usize::try_from(max_bytes).is_ok_and(|max| max <= MAX_SERVICE_HTTP_BODY_BYTES)
}

fn parts_fit(fields: &[&str], headers: &[ServiceHttpHeader]) -> bool {
    let field_bytes = fields.iter().map(|field| field.len());
    let header_bytes = headers.iter().map(|(name, value)| name.len() + value.len());
    headers.len() <= MAX_SERVICE_HTTP_HEADERS
        && field_bytes.chain(header_bytes).sum::<usize>() <= MAX_SERVICE_HTTP_HEAD_BYTES
}
