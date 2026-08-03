//! WebTransport transport (`phux-0wmf`): HTTP/3 over QUIC for browsers.
//!
//! Browsers cannot open raw QUIC connections, so the QUIC listener
//! (`transport::quic`) is unreachable from `phux-web`. WebTransport is the
//! browser's door to QUIC-class transport: an HTTP/3 `CONNECT` session over
//! QUIC whose bidirectional streams a page may open via the `WebTransport`
//! JS API. This listener speaks the **identical** length-prefixed phux frames
//! every other transport does (`docs/spec/proto.md` §5): the consumer opens
//! exactly one bidirectional stream per session and frames flow over it
//! exactly as over a Unix socket. The HTTP/3 session establishment is a
//! transport detail below the frame seam — no phux wire change.
//!
//! **Auth.** WebTransport is always TLS 1.3 (QUIC mandates it). For routable
//! (non-loopback) consumers this listener mirrors the WebSocket bearer-token
//! model (ADR-0031): the token rides the WebTransport `CONNECT` request —
//! inside TLS, before any phux frame — either as an `Authorization: Bearer
//! <hex>` header (native consumers) or as a `token=<hex>` query parameter on
//! the request path (browsers: the JS `WebTransport` API cannot set request
//! headers, and the URL is the one authenticated slot it does control). A
//! missing or invalid token refuses the session with HTTP 403 before it is
//! established. On a loopback (unauthenticated) listener no token is expected.
//!
//! The listener shares the persisted certificate, key, and token store with
//! the `wss://` and QUIC paths (`transport::tls`, [`crate::auth`]), so one
//! `phux pair` covers all three remote transports. It binds its own UDP
//! socket: the raw-QUIC endpoint advertises the phux-private ALPN while
//! browsers offer only `h3`, so the two cannot share a listener without
//! ALPN-demultiplexing complexity that buys nothing here.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::frame::MAX_FRAME_LEN;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};
use wtransport::endpoint::{IncomingSession, SessionRequest};
use wtransport::error::StreamReadExactError;
use wtransport::stream::{RecvStream, SendStream};

use super::{FrameReader, FrameWriter, Incoming, LENGTH_PREFIX};

/// QUIC idle timeout, matching the raw-QUIC listener: a connection with no
/// traffic and no keep-alive for this long is dropped.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep-alive interval, comfortably under [`IDLE_TIMEOUT`] so an attached but
/// quiet browser tab holds its session open across NATs.
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// A WebTransport listener: a wtransport server endpoint bound to a UDP
/// socket, optionally token-authenticated for routable consumers.
pub(crate) struct WtListener {
    endpoint: wtransport::Endpoint<wtransport::endpoint::endpoint_side::Server>,
    tokens: Option<Arc<crate::auth::TokenStore>>,
}

/// Errors from constructing a [`WtListener`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum WtBindError {
    /// Building the rustls TLS config failed.
    #[error("webtransport tls: {0}")]
    Tls(#[from] super::tls::TlsError),
    /// The idle-timeout constant overflowed QUIC's varint encoding (cannot
    /// happen with the compiled-in value; surfaced rather than swallowed).
    #[error("webtransport transport config: invalid idle timeout")]
    IdleTimeout,
    /// Binding the UDP endpoint failed.
    #[error("webtransport bind: {0}")]
    Io(#[from] io::Error),
}

impl WtListener {
    /// Bind a WebTransport listener: build the (always-TLS) rustls config from
    /// the persisted cert + key, then open the endpoint. `tokens` selects the
    /// auth mode — `Some` requires a valid bearer token on each `CONNECT`
    /// (routable consumers, ADR-0031 parity with `wss://`); `None` is the
    /// loopback/dev path that expects none. TLS is on in both modes (QUIC
    /// mandates it).
    pub(crate) fn from_pem(
        addr: SocketAddr,
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
        tokens: Option<Arc<crate::auth::TokenStore>>,
    ) -> Result<Self, WtBindError> {
        let tls = super::tls::webtransport_server_config(cert_path, key_path)?;
        let config = wtransport::ServerConfig::builder()
            .with_bind_address(addr)
            .with_custom_tls(tls)
            .max_idle_timeout(Some(IDLE_TIMEOUT))
            .map_err(|_| WtBindError::IdleTimeout)?
            .keep_alive_interval(Some(KEEP_ALIVE))
            .build();
        Ok(Self {
            endpoint: wtransport::Endpoint::server(config)?,
            tokens,
        })
    }

    /// The local address the endpoint is bound to (for logging the OS-assigned
    /// port when binding to `:0`).
    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Drive one incoming session to an accepted phux byte stream: HTTP/3
    /// handshake, token gate, session accept, then the consumer's single
    /// bidirectional stream. `None` means "refused or failed — next session".
    async fn establish(
        &self,
        incoming: IncomingSession,
    ) -> Option<(WtReader, WtWriter, PeerIdentity)> {
        let request = match incoming.await {
            Ok(request) => request,
            Err(err) => {
                debug!(error = %err, "webtransport session handshake failed");
                return None;
            }
        };
        let remote = request.remote_address();

        // Token gate BEFORE the session is accepted, mirroring the WebSocket
        // path's reject-at-the-upgrade: an unauthorized consumer sees HTTP
        // 403 and no WebTransport session ever exists.
        let device_id = match &self.tokens {
            Some(store) => {
                let Some(id) = authorize_request(&request, store) else {
                    warn!(%remote, "webtransport consumer refused: missing or invalid token");
                    request.forbidden().await;
                    return None;
                };
                Some(id)
            }
            None => None,
        };

        let connection = match request.accept().await {
            Ok(connection) => connection,
            Err(err) => {
                debug!(%remote, error = %err, "webtransport session accept failed");
                return None;
            }
        };

        // The consumer opens one bidi stream and immediately writes its first
        // frame, so `accept_bi` resolves promptly.
        let (send, recv) = match connection.accept_bi().await {
            Ok(pair) => pair,
            Err(err) => {
                debug!(%remote, error = %err, "webtransport stream accept failed");
                return None;
            }
        };

        let peer_identity = PeerIdentity {
            uid: 0,
            pid: None,
            exe_path: None,
            mcp_host_key: device_id,
            transport: TransportType::WebTransport,
            source_addr: Some(remote.ip()),
        };

        // Each half keeps a clone of the session-owning `Connection`: dropping
        // the last handle tears the WebTransport session down, and the frame
        // halves must outlive this accept scope.
        Some((
            WtReader {
                _connection: connection.clone(),
                recv,
                header: [0u8; LENGTH_PREFIX],
            },
            WtWriter {
                _connection: connection,
                send,
            },
            peer_identity,
        ))
    }
}

impl std::fmt::Debug for WtListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WtListener")
            .field("authenticated", &self.tokens.is_some())
            .finish_non_exhaustive()
    }
}

/// WebTransport read half: reassembles length-prefixed frames off the bidi
/// stream, byte-for-byte the same framing as the UDS and QUIC paths.
pub(crate) struct WtReader {
    /// Keeps the WebTransport session alive for the stream's lifetime.
    _connection: wtransport::Connection,
    recv: RecvStream,
    header: [u8; LENGTH_PREFIX],
}

impl FrameReader for WtReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        if !read_exact_wt(&mut self.recv, &mut self.header).await? {
            // Clean stream finish at a frame boundary: end of connection.
            return Ok(None);
        }
        let body_len = u32::from_be_bytes(self.header);
        if !(1..=MAX_FRAME_LEN).contains(&body_len) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized or empty frame",
            ));
        }
        let body_len = body_len as usize;
        let mut framed = BytesMut::with_capacity(LENGTH_PREFIX + body_len);
        framed.extend_from_slice(&self.header);
        framed.resize(LENGTH_PREFIX + body_len, 0);
        if !read_exact_wt(&mut self.recv, &mut framed[LENGTH_PREFIX..]).await? {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stream finished mid-frame",
            ));
        }
        Ok(Some(framed))
    }
}

/// WebTransport write half.
pub(crate) struct WtWriter {
    /// Keeps the WebTransport session alive for the stream's lifetime.
    _connection: wtransport::Connection,
    send: SendStream,
}

impl FrameWriter for WtWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.send.write_all(frame).await.map_err(io::Error::other)
    }

    async fn close(&mut self) -> io::Result<()> {
        self.send.finish().await.map_err(io::Error::other)
    }
}

impl Incoming for WtListener {
    type Reader = WtReader;
    type Writer = WtWriter;

    async fn accept(&self) -> io::Result<(WtReader, WtWriter, PeerIdentity)> {
        // One endpoint multiplexes many sessions; a single bad handshake or
        // refused token must not tear the listener down, so per-session
        // failures loop (logged inside `establish`). This mirrors the QUIC
        // listener's multiplexed accept loop.
        loop {
            let incoming = self.endpoint.accept().await;
            if let Some(accepted) = self.establish(incoming).await {
                return Ok(accepted);
            }
        }
    }

    fn kind(&self) -> &'static str {
        "webtransport"
    }
}

/// Extract and verify the bearer token from a WebTransport `CONNECT` request.
///
/// Two carriers are accepted, both inside TLS: an `Authorization: Bearer
/// <hex>` header (native consumers, exactly the `wss://` shape) or a
/// `token=<hex>` query parameter on the `:path` (browsers — the JS
/// `WebTransport` constructor takes a URL and nothing else). Returns a
/// non-reversible device id (a short SHA-256 prefix of the *presented*
/// token, matching the WebSocket and QUIC paths) on success, `None` on a
/// missing, malformed, or unrecognized token. Deriving the id from the
/// presented token (not the matched stored one) keeps it off the
/// constant-time comparison and never logs the secret.
fn authorize_request(request: &SessionRequest, store: &crate::auth::TokenStore) -> Option<String> {
    let token_hex =
        bearer_from_headers(request.headers()).or_else(|| token_from_path(request.path()))?;
    let token = hex::decode(token_hex.trim()).ok()?;
    if !store.verify(&token) {
        return None;
    }
    let digest = Sha256::digest(&token);
    Some(hex::encode(&digest[..8]))
}

/// The `Bearer` value of an `Authorization` header, matched
/// case-insensitively on the field name (HTTP/3 encodes field names
/// lowercase on the wire, but a hand-built native client may not).
fn bearer_from_headers(headers: &std::collections::HashMap<String, String>) -> Option<&str> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .and_then(|(_, value)| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
}

/// The `token` query parameter of a `:path`, e.g. `/session?token=<hex>`.
fn token_from_path(path: &str) -> Option<&str> {
    let (_, query) = path.split_once('?')?;
    query.split('&').find_map(|kv| kv.strip_prefix("token="))
}

/// Fill `buf` from the WebTransport stream. Returns `Ok(true)` when `buf` is
/// filled, `Ok(false)` on a clean stream finish before any byte was read (end
/// of the connection at a frame boundary), and `Err` on a
/// partial-then-finished read (a truncated frame) or a transport error.
async fn read_exact_wt(recv: &mut RecvStream, buf: &mut [u8]) -> io::Result<bool> {
    match recv.read_exact(buf).await {
        Ok(()) => Ok(true),
        // Zero bytes before the stream finished is a clean EOF at a frame
        // boundary; any other shortfall is a truncated frame.
        Err(StreamReadExactError::FinishedEarly(0)) => Ok(false),
        Err(err) => Err(io::Error::other(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::time::Duration;

    use super::super::tls::ensure_self_signed;
    use wtransport::ClientConfig;
    use wtransport::endpoint::ConnectOptions;

    const TEST_TOKEN: [u8; crate::auth::TOKEN_LEN] = [0x11; crate::auth::TOKEN_LEN];
    /// One complete framed message: 4-byte length prefix (body = 3) + body.
    const FRAME: [u8; 7] = [0, 0, 0, 3, 0xde, 0xad, 0xbe];
    /// A second frame for the echo direction (server -> client).
    const ECHO_FRAME: [u8; 6] = [0, 0, 0, 2, 0xca, 0xfe];

    /// A self-signed cert + key in a fresh tempdir, kept alive for the test.
    fn cert_pair() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        ensure_self_signed(&cert, &key).unwrap();
        (dir, cert, key)
    }

    /// A token store file holding the one known [`TEST_TOKEN`].
    fn token_store() -> (tempfile::NamedTempFile, Arc<crate::auth::TokenStore>) {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "{}", hex::encode(TEST_TOKEN)).unwrap();
        let store = crate::auth::TokenStore::load(file.path()).unwrap();
        (file, Arc::new(store))
    }

    /// A native WebTransport client endpoint. Certificate validation is
    /// skipped (the self-signed leaf is trusted blindly): the TLS handshake
    /// and the full HTTP/3 `CONNECT` session establishment are still
    /// exercised end-to-end; fingerprint pinning is the dialer's concern,
    /// out of scope for the listener under test.
    fn client_endpoint() -> wtransport::Endpoint<wtransport::endpoint::endpoint_side::Client> {
        let config = ClientConfig::builder()
            .with_bind_default()
            .with_no_cert_validation()
            .build();
        wtransport::Endpoint::client(config).unwrap()
    }

    /// Bind a listener on an ephemeral loopback port and return its URL base.
    fn listener(tokens: Option<Arc<crate::auth::TokenStore>>) -> (tempfile::TempDir, WtListener) {
        let (dir, cert, key) = cert_pair();
        let listener =
            WtListener::from_pem("127.0.0.1:0".parse().unwrap(), &cert, &key, tokens).unwrap();
        (dir, listener)
    }

    #[tokio::test]
    async fn round_trips_frames_unauthenticated() {
        let (_dir, listener) = listener(None);
        let addr = listener.local_addr().unwrap();
        let url = format!("https://127.0.0.1:{}/session", addr.port());

        let server = async {
            let (mut reader, mut writer, peer) = listener.accept().await.unwrap();
            let got = reader.read_frame().await.unwrap();
            // Echo direction: the server writes a frame back over the same
            // bidi stream.
            writer.write_frame(&ECHO_FRAME).await.unwrap();
            // Hold the session open until the client has read the echo:
            // returning here would drop the Connection (and the unflushed
            // stream) before delivery. The next read resolves once the
            // client tears the session down.
            let _ = reader.read_frame().await;
            (got, peer)
        };
        let client = async {
            let conn = client_endpoint().connect(url).await.unwrap();
            let (mut send, mut recv) = conn.open_bi().await.unwrap().await.unwrap();
            send.write_all(&FRAME).await.unwrap();
            let mut echoed = [0u8; ECHO_FRAME.len()];
            recv.read_exact(&mut echoed).await.unwrap();
            echoed
        };

        let ((got, peer), echoed) = tokio::join!(server, client);
        assert_eq!(
            got.unwrap().as_ref(),
            &FRAME,
            "frame round-trips over WebTransport"
        );
        assert_eq!(echoed, ECHO_FRAME, "server frame reaches the client");
        assert_eq!(peer.transport, TransportType::WebTransport);
        assert!(
            peer.mcp_host_key.is_none(),
            "an unauthenticated loopback peer carries no device id"
        );
    }

    #[tokio::test]
    async fn valid_bearer_header_authenticates_and_round_trips() {
        let (_tok_file, store) = token_store();
        let (_dir, listener) = listener(Some(store));
        let addr = listener.local_addr().unwrap();
        let url = format!("https://127.0.0.1:{}/session", addr.port());

        let server = async {
            let (mut reader, _writer, peer) = listener.accept().await.unwrap();
            let got = reader.read_frame().await.unwrap();
            (got, peer)
        };
        let client = async {
            let options = ConnectOptions::builder(&url)
                .add_header(
                    "authorization",
                    format!("Bearer {}", hex::encode(TEST_TOKEN)),
                )
                .build();
            let conn = client_endpoint().connect(options).await.unwrap();
            let (mut send, _recv) = conn.open_bi().await.unwrap().await.unwrap();
            send.write_all(&FRAME).await.unwrap();
            // Hold the connection open until the server has read the frame.
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        let ((got, peer), ()) = tokio::join!(server, client);
        assert_eq!(
            got.unwrap().as_ref(),
            &FRAME,
            "frame round-trips after auth"
        );
        assert_eq!(peer.transport, TransportType::WebTransport);
        assert!(
            peer.mcp_host_key.is_some(),
            "an authenticated remote peer is non-anonymous"
        );
    }

    #[tokio::test]
    async fn valid_token_query_param_authenticates() {
        let (_tok_file, store) = token_store();
        let (_dir, listener) = listener(Some(store));
        let addr = listener.local_addr().unwrap();
        // The browser carrier: the JS WebTransport API cannot set headers,
        // so the token rides the URL query.
        let url = format!(
            "https://127.0.0.1:{}/session?token={}",
            addr.port(),
            hex::encode(TEST_TOKEN)
        );

        let server = async {
            let (mut reader, _writer, peer) = listener.accept().await.unwrap();
            let got = reader.read_frame().await.unwrap();
            (got, peer)
        };
        let client = async {
            let conn = client_endpoint().connect(url).await.unwrap();
            let (mut send, _recv) = conn.open_bi().await.unwrap().await.unwrap();
            send.write_all(&FRAME).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        let ((got, peer), ()) = tokio::join!(server, client);
        assert_eq!(got.unwrap().as_ref(), &FRAME);
        assert!(peer.mcp_host_key.is_some());
    }

    #[tokio::test]
    async fn invalid_token_is_refused_before_the_session_exists() {
        let (_tok_file, store) = token_store();
        let (_dir, listener) = listener(Some(store));
        let addr = listener.local_addr().unwrap();
        let wrong = hex::encode([0x22u8; crate::auth::TOKEN_LEN]);
        let url = format!("https://127.0.0.1:{}/session?token={wrong}", addr.port());

        // The listener loops internally on a refused session (it serves a
        // multiplexed endpoint), so drive `accept` concurrently and assert
        // the refusal from the *client* side: the CONNECT is rejected before
        // any WebTransport session exists.
        let client = async {
            let result = client_endpoint().connect(url).await;
            assert!(result.is_err(), "unknown token must refuse the session");
        };
        let server = listener.accept();

        tokio::select! {
            () = client => {}
            accepted = server => {
                let _ = accepted;
                panic!("server must not accept a session with an unknown token");
            }
        }
    }

    #[tokio::test]
    async fn missing_token_is_refused() {
        let (_tok_file, store) = token_store();
        let (_dir, listener) = listener(Some(store));
        let addr = listener.local_addr().unwrap();
        let url = format!("https://127.0.0.1:{}/session", addr.port());

        let client = async {
            let result = client_endpoint().connect(url).await;
            assert!(result.is_err(), "a missing token must refuse the session");
        };
        let server = listener.accept();

        tokio::select! {
            () = client => {}
            accepted = server => {
                let _ = accepted;
                panic!("server must not accept a session without a token");
            }
        }
    }
}
