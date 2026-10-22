//! The HTTP leg: one loopback HTTP/1.1 connection per exchange (hyper), the
//! header policy, and the body reader `SERVICE_HTTP_READ` drains.
//!
//! Requests never follow redirects: a `3xx` is relayed like any response.
//! Hop-by-hop headers (and any the `Connection` header lists) are dropped
//! both ways; the client's `Host`, `Authorization`, `Cookie`, `Proxy-*`,
//! `Content-Length`, and `Expect` are replaced by the server's own, and a
//! response's `Set-Cookie` never reaches the client.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::FutureExt as _;
use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use http::uri::PathAndQuery;
use http::{Method, Request, Response, StatusCode, Uri};
use http_body_util::{BodyExt, Full};
use hyper::body::{Body as _, Incoming};
use hyper_util::rt::TokioIo;
use phux_protocol::service_http::{ServiceHttpChunk, ServiceHttpHeader};
use tokio::net::TcpStream;
use tracing::debug;

use super::Refusal;
use super::opencode::{Credentials, OpenCode};

const HOP_BY_HOP: [&str; 7] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];
/// Request headers the server sets itself.
const SERVER_OWNED: [&str; 5] = [
    "host",
    "authorization",
    "cookie",
    "content-length",
    "expect",
];

/// Why one upstream request got no response head.
#[derive(Debug, thiserror::Error)]
pub(super) enum SendError {
    #[error("connect: {0}")]
    Connect(std::io::Error),
    #[error(transparent)]
    Http(#[from] hyper::Error),
}

/// Send `request` on a fresh loopback connection; the connection lives as
/// long as the response body.
pub(super) async fn send(
    addr: SocketAddr,
    request: Request<Full<Bytes>>,
) -> Result<Response<Incoming>, SendError> {
    let stream = TcpStream::connect(addr).await.map_err(SendError::Connect)?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            debug!(%err, "service HTTP connection ended with an error");
        }
    });
    Ok(sender.send_request(request).await?)
}

/// A validated request, kept whole so a retry can resend it.
#[derive(Debug, Clone)]
pub(super) struct Prepared {
    method: Method,
    target: PathAndQuery,
    headers: HeaderMap,
    body: Bytes,
}

impl Prepared {
    /// Validate the client's request: an HTTP method other than `CONNECT`,
    /// an origin-form target, and well-formed headers.
    pub(super) fn new(
        method: &str,
        target: &str,
        headers: &[ServiceHttpHeader],
        body: Vec<u8>,
    ) -> Result<Self, Refusal> {
        Ok(Self {
            method: parse_method(method)?,
            target: parse_target(target)?,
            headers: request_headers(headers)?,
            body: Bytes::from(body),
        })
    }

    /// The body bytes this request retains for a retry.
    pub(super) const fn body_len(&self) -> usize {
        self.body.len()
    }

    /// The target's path, without its query.
    pub(super) fn path(&self) -> &str {
        self.target.path()
    }

    fn request(&self, credentials: &Credentials) -> Request<Full<Bytes>> {
        let mut request = Request::new(Full::new(self.body.clone()));
        *request.method_mut() = self.method.clone();
        *request.uri_mut() = Uri::from(self.target.clone());
        let headers = request.headers_mut();
        headers.clone_from(&self.headers);
        headers.insert(header::HOST, credentials.host.clone());
        headers.insert(header::AUTHORIZATION, credentials.authorization.clone());
        request
    }
}

fn parse_method(method: &str) -> Result<Method, Refusal> {
    match Method::from_str(method) {
        Ok(method) if method != Method::CONNECT => Ok(method),
        _ => Err(Refusal::invalid(format!(
            "{method:?} is not an HTTP method this service accepts"
        ))),
    }
}

fn parse_target(target: &str) -> Result<PathAndQuery, Refusal> {
    let origin_form = target.starts_with('/') && !target.starts_with("//") && !target.contains('#');
    match PathAndQuery::from_str(target) {
        Ok(parsed) if origin_form => Ok(parsed),
        _ => Err(Refusal::invalid(format!(
            "{target:?} is not an origin-form target (a path and query starting with '/')"
        ))),
    }
}

fn request_headers(raw: &[ServiceHttpHeader]) -> Result<HeaderMap, Refusal> {
    let parsed = raw
        .iter()
        .map(|(name, value)| parse_header(name, value))
        .collect::<Result<Vec<_>, _>>()?;
    let listed = connection_listed(parsed.iter().map(|(name, value)| (name, value)));
    let mut headers = HeaderMap::new();
    for (name, value) in parsed {
        if !server_owned(&name) && !hop_by_hop(&name, &listed) {
            headers.append(name, value);
        }
    }
    Ok(headers)
}

fn parse_header(name: &str, value: &str) -> Result<(HeaderName, HeaderValue), Refusal> {
    let parsed = HeaderName::from_str(name)
        .ok()
        .zip(HeaderValue::from_str(value).ok());
    parsed.ok_or_else(|| Refusal::invalid(format!("header {name:?} is not a valid HTTP header")))
}

/// The response headers the client sees, in order.
pub(super) fn response_headers(headers: &HeaderMap) -> Vec<ServiceHttpHeader> {
    let listed = connection_listed(headers.iter());
    headers
        .iter()
        .filter(|(name, _)| *name != header::SET_COOKIE && !hop_by_hop(name, &listed))
        .filter_map(|(name, value)| {
            let Ok(value) = std::str::from_utf8(value.as_bytes()) else {
                debug!(%name, "dropping a non-UTF-8 service HTTP response header");
                return None;
            };
            Some((name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

/// The header names a `Connection` header lists, which are hop-by-hop too.
fn connection_listed<'a>(
    headers: impl Iterator<Item = (&'a HeaderName, &'a HeaderValue)>,
) -> HashSet<String> {
    headers
        .filter(|(name, _)| **name == header::CONNECTION)
        .filter_map(|(_, value)| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

fn hop_by_hop(name: &HeaderName, listed: &HashSet<String>) -> bool {
    let name = name.as_str();
    HOP_BY_HOP.contains(&name) || name.starts_with("proxy-") || listed.contains(name)
}

fn server_owned(name: &HeaderName) -> bool {
    SERVER_OWNED.contains(&name.as_str())
}

/// Send `prepared` to `provider` and wait for the response head, all within
/// `deadline`: discovery, minting, connecting, and both retries (a moved
/// origin is rediscovered; a Basic `401` re-mints the token, at most once per
/// cooldown). The request body is retained, so a retry resends it whole.
pub(super) async fn open(
    provider: &OpenCode,
    prepared: &Prepared,
    deadline: Duration,
) -> Result<Response<Incoming>, Refusal> {
    tokio::time::timeout(deadline, open_unbounded(provider, prepared))
        .await
        .map_err(|_| {
            Refusal::unavailable(format!(
                "OpenCode did not answer within {}s; long-running endpoints must be \
                 used asynchronously",
                deadline.as_secs()
            ))
        })?
}

async fn open_unbounded(
    provider: &OpenCode,
    prepared: &Prepared,
) -> Result<Response<Incoming>, Refusal> {
    let (credentials, response) = send_rediscovering(provider, prepared).await?;
    if !basic_challenge(&response) {
        return Ok(response);
    }
    if !provider
        .forget_token(&credentials.token, Instant::now())
        .await
    {
        return Ok(response);
    }
    drop(response);
    Ok(send_rediscovering(provider, prepared).await?.1)
}

/// A `401` whose `WWW-Authenticate` is the service's own Basic challenge,
/// that is, a refused token rather than a refusal of the request itself.
fn basic_challenge(response: &Response<Incoming>) -> bool {
    let challenged = response
        .headers()
        .get_all(header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            value
                .trim_start()
                .get(..5)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("basic"))
        });
    response.status() == StatusCode::UNAUTHORIZED && challenged
}

async fn send_rediscovering(
    provider: &OpenCode,
    prepared: &Prepared,
) -> Result<(Credentials, Response<Incoming>), Refusal> {
    let credentials = provider.credentials().await?;
    match send_once(&credentials, prepared).await {
        Err(SendError::Connect(_)) => {}
        sent => return sent.map(|response| (credentials, response)).map_err(failed),
    }
    provider.forget_origin(credentials.origin).await;
    let credentials = provider.credentials().await?;
    let response = send_once(&credentials, prepared).await.map_err(failed)?;
    Ok((credentials, response))
}

async fn send_once(
    credentials: &Credentials,
    prepared: &Prepared,
) -> Result<Response<Incoming>, SendError> {
    send(credentials.origin.addr(), prepared.request(credentials)).await
}

fn failed(err: SendError) -> Refusal {
    match err {
        SendError::Http(err) => Refusal::internal(format!("OpenCode's HTTP reply failed: {err}")),
        SendError::Connect(err) => {
            Refusal::unavailable(format!("OpenCode is not accepting connections: {err}"))
        }
    }
}

/// An open response body, drained by `SERVICE_HTTP_READ`.
#[derive(Debug)]
pub(super) struct BodyReader {
    body: Incoming,
    /// Bytes of the last frame not yet returned.
    pending: Bytes,
    /// The body has no more frames.
    done: bool,
}

impl BodyReader {
    pub(super) fn new(body: Incoming) -> Self {
        let done = body.is_end_stream();
        Self {
            body,
            pending: Bytes::new(),
            done,
        }
    }

    /// At most `max` bytes: what is ready now, else the first bytes to
    /// arrive within `idle`, else nothing. `end` once nothing follows.
    pub(super) async fn read(
        &mut self,
        max: usize,
        idle: Duration,
    ) -> Result<ServiceHttpChunk, Refusal> {
        let mut bytes = Vec::new();
        if self.pending.is_empty() && !self.done {
            match tokio::time::timeout(idle, self.fill()).await {
                Ok(filled) => filled?,
                Err(_) => return Ok(self.chunk(bytes)),
            }
        }
        self.drain(&mut bytes, max)?;
        Ok(self.chunk(bytes))
    }

    /// Move pending bytes, then frames that are already ready, into `out`.
    fn drain(&mut self, out: &mut Vec<u8>, max: usize) -> Result<(), Refusal> {
        loop {
            let take = self.pending.len().min(max - out.len());
            out.extend_from_slice(&self.pending.split_to(take));
            if out.len() == max || self.done {
                return Ok(());
            }
            match self.fill().now_or_never() {
                Some(filled) => filled?,
                None => return Ok(()),
            }
        }
    }

    /// Wait for the next data frame, or the end. Cancel-safe: a frame is
    /// consumed only once it is stored.
    async fn fill(&mut self) -> Result<(), Refusal> {
        while self.pending.is_empty() && !self.done {
            match self.body.frame().await {
                None => self.done = true,
                Some(Err(err)) => {
                    return Err(Refusal::unavailable(format!(
                        "the service's response body failed: {err}"
                    )));
                }
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        self.pending = data;
                    }
                }
            }
        }
        self.done |= self.body.is_end_stream();
        Ok(())
    }

    const fn chunk(&self, bytes: Vec<u8>) -> ServiceHttpChunk {
        ServiceHttpChunk {
            bytes,
            end: self.done && self.pending.is_empty(),
        }
    }
}
