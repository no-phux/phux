//! WebSocket upgrade authorization: bearer and browser credentials, origins, and refusal warnings.
#![allow(
    clippy::redundant_pub_crate,
    reason = "private transport module; the accept loop uses these through the transport re-export"
)]

use std::io;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};

use super::{
    HANDSHAKE_DEADLINE, ServerStream, WS_REJECTION_WARN_INTERVAL, WsAcceptStage, ws_accept_error,
};

#[derive(Debug)]
pub(super) struct PeerRejectionWarnLimiter {
    last_warning: Option<Instant>,
    pub(super) suppressed: u64,
}

impl PeerRejectionWarnLimiter {
    pub(super) const fn new() -> Self {
        Self {
            last_warning: None,
            suppressed: 0,
        }
    }

    /// Warn on the first rejection, then at most once per interval, counting
    /// what was suppressed. One listener-wide counter keeps memory bounded.
    pub(super) fn observe(&mut self, now: Instant) -> PeerRejectionWarnDecision {
        let should_warn = self
            .last_warning
            .is_none_or(|last| now.saturating_duration_since(last) >= WS_REJECTION_WARN_INTERVAL);
        if should_warn {
            self.last_warning = Some(now);
            let suppressed = std::mem::take(&mut self.suppressed);
            PeerRejectionWarnDecision::Emit { suppressed }
        } else {
            self.suppressed = self.suppressed.saturating_add(1);
            PeerRejectionWarnDecision::Suppress
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PeerRejectionWarnDecision {
    Suppress,
    Emit { suppressed: u64 },
}

/// One listener's peer refusals, logged at WARN at most once per
/// [`WS_REJECTION_WARN_INTERVAL`] with the count suppressed since, so an
/// unauthenticated peer cannot flood the log. Callers log every refusal at
/// DEBUG regardless.
#[derive(Debug)]
pub(crate) struct RefusalWarnings(Mutex<PeerRejectionWarnLimiter>);

impl RefusalWarnings {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(PeerRejectionWarnLimiter::new()))
    }

    /// Record one refusal; `Some(suppressed)` when a WARN is due now.
    pub(crate) fn due(&self) -> Option<u64> {
        let decision = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe(Instant::now());
        match decision {
            PeerRejectionWarnDecision::Suppress => None,
            PeerRejectionWarnDecision::Emit { suppressed } => Some(suppressed),
        }
    }
}

/// Verify either the native Authorization header or the browser's credential
/// subprotocol. Ambiguous carriers are refused before the HTTP upgrade.
fn authorize_request(
    req: &Request,
    store: &crate::auth::ReloadingTokenStore,
) -> Option<crate::auth::AuthenticatedCredential> {
    let token_hex = request_token(req)?;
    if token_hex.len() != crate::auth::TOKEN_LEN * 2 {
        return None;
    }
    let token = hex::decode(token_hex).ok()?;
    store.authenticate_and_touch(&token)
}

/// What an admitted upgrade carries: the connection's credential (the
/// workload's under workload mTLS, else the bearer's), then the bearer.
pub(super) type Admitted = (
    crate::auth::AuthenticatedCredential,
    crate::auth::AuthenticatedCredential,
);

/// Admit a WebSocket upgrade: the pairing token first, then, with a workload
/// CA, the client certificate through the registry's current generation.
/// Every refusal is the same 401 (`docs/remote.md#who-can-connect`).
pub(super) fn admit_upgrade(
    req: &Request,
    store: &crate::auth::ReloadingTokenStore,
    workload: Option<(&crate::workload::ReloadingWorkloadRegistry, Option<&[u8]>)>,
) -> Option<Admitted> {
    let bearer = authorize_request(req, store)?;
    let Some((registry, leaf)) = workload else {
        return Some((bearer.clone(), bearer));
    };
    registry
        .lookup_certificate(leaf?)
        .map(|workload| (workload, bearer))
}

/// Terminate TLS under [`HANDSHAKE_DEADLINE`], keeping the client's leaf
/// certificate, if it sent one, for the workload registry lookup.
pub(super) async fn tls_handshake(
    acceptor: &tokio_rustls::TlsAcceptor,
    tcp: TcpStream,
    source_ip: IpAddr,
) -> io::Result<(
    ServerStream,
    Option<rustls::pki_types::CertificateDer<'static>>,
)> {
    let tls = tokio::time::timeout(HANDSHAKE_DEADLINE, acceptor.accept(tcp))
        .await
        .map_err(|_| {
            tracing::debug!(%source_ip, "WebSocket TLS handshake timed out");
            ws_accept_error(WsAcceptStage::TlsHandshake, source_ip)
        })?
        .map_err(|err| {
            tracing::debug!(%source_ip, error = %err, "WebSocket TLS handshake failed");
            ws_accept_error(WsAcceptStage::TlsHandshake, source_ip)
        })?;
    let leaf = peer_leaf_certificate(&tls);
    Ok((ServerStream::Right(Box::new(tls)), leaf))
}

/// The leaf certificate a TLS client presented (chain already verified).
fn peer_leaf_certificate(
    tls: &tokio_rustls::server::TlsStream<TcpStream>,
) -> Option<rustls::pki_types::CertificateDer<'static>> {
    tls.get_ref()
        .1
        .peer_certificates()?
        .first()
        .map(|leaf| leaf.clone().into_owned())
}

fn request_token(req: &Request) -> Option<&str> {
    let (browser_protocol, browser_token) = browser_auth_protocols(req)?;
    let mut headers = req.headers().get_all("authorization").iter();
    let header = headers.next();
    if headers.next().is_some() || (header.is_some() && browser_token.is_some()) {
        return None;
    }
    match header {
        Some(header) => {
            let header = header.to_str().ok()?;
            Some(
                header
                    .strip_prefix("Bearer ")
                    .or_else(|| header.strip_prefix("bearer "))?
                    .trim(),
            )
        }
        None if browser_protocol => browser_token,
        None => None,
    }
}

/// Parse the small browser auth vocabulary without copying credential bytes.
fn browser_auth_protocols(req: &Request) -> Option<(bool, Option<&str>)> {
    let mut application = false;
    let mut bearer = None;
    for header in req.headers().get_all("sec-websocket-protocol") {
        for protocol in header.to_str().ok()?.split(',').map(str::trim) {
            if protocol == "phux.v1" {
                if application {
                    return None;
                }
                application = true;
            } else if let Some(token) = protocol.strip_prefix("phux.bearer.")
                && bearer.replace(token).is_some()
            {
                return None;
            }
        }
    }
    Some((application, bearer))
}

/// Select only the public application protocol. Never echo the auth carrier.
pub(super) fn select_ws_protocol(req: &Request, mut response: Response) -> Response {
    if matches!(browser_auth_protocols(req), Some((true, _))) {
        response.headers_mut().insert(
            "sec-websocket-protocol",
            tokio_tungstenite::tungstenite::http::HeaderValue::from_static("phux.v1"),
        );
    }
    response
}

/// A browser asking for paired authentication must not accidentally establish
/// an anonymous connection when the listener has no authority store, and a
/// page from an origin the listener does not admit is refused outright.
#[allow(
    clippy::result_large_err,
    reason = "tungstenite fixes the HTTP upgrade callback error type"
)]
pub(super) fn anonymous_ws_upgrade(
    req: &Request,
    response: Response,
    origins: &AllowedOrigins,
) -> Result<Response, ErrorResponse> {
    if !origins.admits_request(req) {
        return Err(forbidden_origin_response());
    }
    match browser_auth_protocols(req) {
        Some((_, None)) => Ok(select_ws_protocol(req, response)),
        _ => Err(unauthorized_response()),
    }
}

/// Browser origins the anonymous WebSocket listener admits.
///
/// Browsers attach `Origin` to every WebSocket handshake but do not hold it
/// to the same-origin policy, so without this any web page the user visits
/// could open `ws://127.0.0.1:PORT` and drive a shell (cross-site WebSocket
/// hijacking). A request with no `Origin` (a native client) or a loopback one
/// (a locally served page) is admitted, plus whatever
/// `PHUX_WS_ALLOWED_ORIGINS` names: a comma-separated list of exact origins,
/// or `*` for a listener fronted by a proxy that checks origins itself.
/// `null` (sandboxed frames, `file:` pages) is never loopback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AllowedOrigins {
    any: bool,
    exact: Vec<String>,
}

impl AllowedOrigins {
    /// Parse `PHUX_WS_ALLOWED_ORIGINS`; unset admits loopback origins only.
    #[must_use]
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        let mut origins = Self::default();
        for entry in raw.unwrap_or_default().split(',').map(str::trim) {
            if entry == "*" {
                origins.any = true;
            } else if !entry.is_empty() {
                origins
                    .exact
                    .push(entry.trim_end_matches('/').to_ascii_lowercase());
            }
        }
        origins
    }

    fn admits_request(&self, req: &Request) -> bool {
        let mut values = req.headers().get_all("origin").iter();
        let Some(value) = values.next() else {
            return true;
        };
        if values.next().is_some() {
            return false;
        }
        value.to_str().is_ok_and(|origin| self.admits(origin))
    }

    pub(super) fn admits(&self, origin: &str) -> bool {
        let origin = origin.trim().to_ascii_lowercase();
        self.any || self.exact.contains(&origin) || is_loopback_origin(&origin)
    }
}

/// Whether a serialized origin (`scheme://host[:port]`) names this machine.
fn is_loopback_origin(origin: &str) -> bool {
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    if rest.contains(['/', '@', '?', '#']) {
        return false;
    }
    let host = match rest.strip_prefix('[') {
        Some(bracketed) => match bracketed.split_once(']') {
            Some((host, tail)) if tail.is_empty() || tail.starts_with(':') => host,
            _ => return false,
        },
        None => rest.split_once(':').map_or(rest, |(host, _)| host),
    };
    host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The HTTP 403 for a browser page from an origin the listener refuses.
fn forbidden_origin_response() -> ErrorResponse {
    use tokio_tungstenite::tungstenite::http::StatusCode;
    let mut resp = ErrorResponse::new(Some("origin not allowed".to_owned()));
    *resp.status_mut() = StatusCode::FORBIDDEN;
    resp
}

/// The generic HTTP 401 for any pairing failure, leaking nothing about which
/// check failed.
pub(super) fn unauthorized_response() -> ErrorResponse {
    use tokio_tungstenite::tungstenite::http::StatusCode;
    let mut resp = ErrorResponse::new(Some("missing or invalid pairing token".to_owned()));
    *resp.status_mut() = StatusCode::UNAUTHORIZED;
    resp
}
