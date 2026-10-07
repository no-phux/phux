//! WebTransport transport: HTTP/3 over QUIC for browsers.
//!
//! Browsers cannot open raw QUIC. The consumer opens
//! one bidirectional stream per session and the identical length-prefixed
//! phux frames (`docs/spec/proto.md` §5) flow over it.
//!
//! Routable consumers authenticate with the pairing token (ADR-0031) on the
//! `CONNECT` request, inside TLS: an `Authorization: Bearer` header (native)
//! or a `token=` query parameter (browsers cannot set headers). A refused
//! session gets HTTP 403 before it exists. Duplicate `Authorization` fields
//! are refused on the raw QPACK field list, before a header map can collapse
//! them. The listener shares the certificate and token store with `wss://`
//! and QUIC, but binds its own UDP socket because browsers offer only the
//! `h3` ALPN.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use phux_dial::window::{SendWindow, TrackedSend};
use phux_protocol::policy::{PeerIdentity, TransportType};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt};
use tracing::{debug, warn};
use wtransport_proto::qpack::Decoder;

use super::quic::QuicBindError as WtBindError;
use super::{FrameReader, FrameWriter, Incoming};

mod connect_headers;
mod h3;

/// Bound on the HTTP/3 request, token gate, session accept, and first stream.
const ESTABLISH_DEADLINE: Duration = super::HANDSHAKE_DEADLINE;

/// How long a refused peer has to read its 403 before its connection is
/// closed.
const REFUSAL_LINGER: Duration = Duration::from_secs(1);

type Accepted = (WtReader, WtWriter, crate::auth::ConnectionIdentity);

/// A WebTransport listener, optionally token-authenticated.
pub(crate) struct WtListener {
    endpoint: quinn::Endpoint,
    tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
    /// The end-to-end session a native client may run inside its stream to
    /// present a workload certificate (ADR-0154 item 6). Under `paired` it is
    /// the only way in; a browser cannot run it yet.
    inner: Option<Arc<super::inner_tls::InnerTls>>,
    admissions: super::Admissions<Accepted>,
    /// Refused `CONNECT`s, warned about at a bounded rate.
    refusals: Arc<super::RefusalWarnings>,
}

/// Closes a connection that establishment abandons, whether it refused the
/// peer, failed, or was dropped at its deadline. The drain task holds a
/// clone of the connection, so without an explicit close a refused peer's
/// connection and task would live until the peer chose to leave.
struct CloseUnlessEstablished(Option<quinn::Connection>);

impl CloseUnlessEstablished {
    fn established(mut self) {
        self.0 = None;
    }
}

impl Drop for CloseUnlessEstablished {
    fn drop(&mut self) {
        if let Some(connection) = self.0.take() {
            connection.close(0_u32.into(), b"refused");
        }
    }
}

impl WtListener {
    /// Bind a listener; `tokens` requires a bearer token on every `CONNECT`.
    pub(crate) fn from_pem(
        addr: SocketAddr,
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
        tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
        inner: Option<Arc<super::inner_tls::InnerTls>>,
    ) -> Result<Self, WtBindError> {
        let tls = super::tls::webtransport_server_config(cert_path, key_path)?;
        Ok(Self {
            endpoint: super::quic::server_endpoint(addr, tls, None)?,
            tokens,
            inner,
            admissions: super::Admissions::new(),
            refusals: Arc::new(super::RefusalWarnings::new()),
        })
    }

    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Drive one session to an accepted phux stream: HTTP/3 handshake, token
    /// gate, session accept, then the consumer's bidi stream. `None` means
    /// refused or failed.
    async fn establish(
        incoming: quinn::Incoming,
        tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
        inner: Option<Arc<super::inner_tls::InnerTls>>,
        refusals: Arc<super::RefusalWarnings>,
    ) -> Option<Accepted> {
        let connection = match incoming.await {
            Ok(connection) => connection,
            Err(err) => {
                debug!(error = %err, "webtransport session handshake failed");
                return None;
            }
        };
        let remote = connection.remote_address();
        let abandon = CloseUnlessEstablished(Some(connection.clone()));

        let settings_send = match h3::send_local_settings(&connection).await {
            Ok(send) => send,
            Err(err) => {
                debug!(%remote, error = %err, "webtransport SETTINGS send failed");
                return None;
            }
        };
        h3::drain_uni_streams(connection.clone());

        let (mut connect_send, connect_recv, payload, session_id) =
            match h3::accept_connect(&connection).await {
                Ok(accepted) => accepted,
                Err(err) => {
                    debug!(%remote, error = %err, "webtransport CONNECT accept failed");
                    return None;
                }
            };

        // Token gate before the session exists: a refusal is HTTP 403.
        let credential = match admit_connect(&payload, tokens.as_deref()) {
            Ok(credential) => credential,
            Err(reason) => {
                if let Some(suppressed) = refusals.due() {
                    warn!(%remote, suppressed, "webtransport consumer refused: {reason}");
                } else {
                    debug!(%remote, "webtransport consumer refused: {reason}");
                }
                let _ = h3::send_connect_status(&mut connect_send, false).await;
                // Let the 403 land before the connection is closed under it.
                let _ = tokio::time::timeout(REFUSAL_LINGER, connect_send.stopped()).await;
                return None;
            }
        };

        if h3::send_connect_status(&mut connect_send, true)
            .await
            .is_err()
        {
            debug!(%remote, "webtransport CONNECT 200 failed");
            return None;
        }

        let (send, recv) = match h3::accept_wt_bidi(&connection, session_id).await {
            Ok(pair) => pair,
            Err(err) => {
                debug!(%remote, error = %err, "webtransport stream accept failed");
                return None;
            }
        };

        let (stream, workload) =
            settle_stream(send, recv, inner.as_deref(), remote, &refusals).await?;

        abandon.established();
        let identity = session_identity(remote, tokens.as_ref(), credential, workload);
        let (recv, send) = match stream {
            WtStream::Plain { recv, send } => (
                recv,
                WtSend::Plain(TrackedSend::new(send, SendWindow::new(connection.clone()))),
            ),
            WtStream::Inner((recv, send)) => (
                Box::new(recv) as Box<dyn tokio::io::AsyncRead + Unpin>,
                WtSend::Inner(send),
            ),
        };

        Some((
            WtReader {
                _session: h3::SessionStreams {
                    _connection: connection.clone(),
                    _connect_send: connect_send,
                    _connect_recv: connect_recv,
                    _settings_send: settings_send,
                },
                recv,
                frames: super::FrameAssembler::default(),
            },
            WtWriter {
                send,
                _connection: connection,
            },
            identity,
        ))
    }
}

/// A session's identity: its inner workload credential when it presented
/// one, else the `CONNECT` bearer, whose revocation ends it either way.
fn session_identity(
    remote: SocketAddr,
    tokens: Option<&Arc<crate::auth::ReloadingTokenStore>>,
    bearer: Option<crate::auth::AuthenticatedCredential>,
    workload: Option<crate::auth::AuthenticatedCredential>,
) -> crate::auth::ConnectionIdentity {
    let admission = tokens.zip(bearer.as_ref()).map(|(store, credential)| {
        crate::auth::BearerAdmission::new(Arc::clone(store), credential)
    });
    let credential = workload.or(bearer);
    crate::auth::ConnectionIdentity {
        peer: PeerIdentity {
            uid: 0,
            pid: None,
            exe_path: None,
            mcp_host_key: credential.as_ref().map(|credential| credential.id.clone()),
            transport: TransportType::WebTransport,
            source_addr: Some(remote.ip()),
        },
        credential,
        ssh_origin: None,
        bearer: admission,
    }
}

/// Settle what the consumer's stream carries: a native client may open an
/// end-to-end TLS session to present its workload certificate (ADR-0154 item
/// 6), told from a frame's length prefix by its first byte. Under `paired`
/// nothing else is admitted. `None` refuses the session.
async fn settle_stream(
    send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    inner: Option<&super::inner_tls::InnerTls>,
    remote: SocketAddr,
    refusals: &super::RefusalWarnings,
) -> Option<(WtStream, Option<crate::auth::AuthenticatedCredential>)> {
    let mut first = [0_u8; 1];
    if recv.read_exact(&mut first).await.is_err() {
        debug!(%remote, "webtransport stream ended before its first byte");
        return None;
    }
    match inner {
        Some(inner) if first[0] == super::inner_tls::TLS_HANDSHAKE => {
            let io = tokio::io::join(std::io::Cursor::new(first).chain(recv), send);
            if let super::inner_tls::InnerAccepted::Terminal { stream, workload } =
                inner.accept(io).await
            {
                return Some((WtStream::Inner(tokio::io::split(stream)), workload));
            }
            debug!(%remote, "webtransport end-to-end session refused or done");
            None
        }
        Some(inner) if !inner.admits_plain() => {
            if let Some(suppressed) = refusals.due() {
                warn!(%remote, suppressed, "webtransport consumer refused: no workload certificate under paired");
            } else {
                debug!(%remote, "webtransport consumer refused: no workload certificate under paired");
            }
            None
        }
        _ => Some((
            WtStream::Plain {
                recv: Box::new(std::io::Cursor::new(first).chain(recv)),
                send,
            },
            None,
        )),
    }
}

impl std::fmt::Debug for WtListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WtListener")
            .field("authenticated", &self.tokens.is_some())
            .finish_non_exhaustive()
    }
}

/// The stream a session settled on: plain, or the end-to-end session
/// inside it.
enum WtStream {
    Plain {
        recv: Box<dyn tokio::io::AsyncRead + Unpin>,
        send: quinn::SendStream,
    },
    Inner(InnerHalves),
}

/// The two halves of an end-to-end session.
type InnerHalves = (
    tokio::io::ReadHalf<Box<dyn super::inner_tls::InnerIo>>,
    tokio::io::WriteHalf<Box<dyn super::inner_tls::InnerIo>>,
);

/// WebTransport read half: the same length-prefixed framing as QUIC.
pub(crate) struct WtReader {
    /// Keeps the HTTP/3 CONNECT session and control stream alive.
    _session: h3::SessionStreams,
    recv: Box<dyn tokio::io::AsyncRead + Unpin>,
    frames: super::FrameAssembler,
}

/// The write side: congestion-tracked QUIC, or the end-to-end session.
enum WtSend {
    Plain(TrackedSend<quinn::SendStream>),
    Inner(tokio::io::WriteHalf<Box<dyn super::inner_tls::InnerIo>>),
}

impl FrameReader for WtReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        self.frames.read_frame(&mut self.recv).await
    }
}

/// WebTransport write half, through the same congestion-tracked
/// [`TrackedSend`] as `QuicWriter`, so a slow browser blocks within about a
/// round trip instead of queueing megabytes.
pub(crate) struct WtWriter {
    send: WtSend,
    /// Keeps the WebTransport session alive for the stream's lifetime.
    _connection: quinn::Connection,
}

impl FrameWriter for WtWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        match &mut self.send {
            WtSend::Plain(send) => send.write_all(frame).await,
            WtSend::Inner(send) => send.write_all(frame).await,
        }
    }

    /// One write for the whole batch, as `QuicWriter::write_frames`.
    async fn write_frames(&mut self, batch: &[u8], _ends: &[usize]) -> io::Result<()> {
        self.write_frame(batch).await
    }

    async fn close(&mut self) -> io::Result<()> {
        match &mut self.send {
            WtSend::Plain(send) => send.get_mut().finish().map_err(io::Error::other),
            WtSend::Inner(send) => send.shutdown().await,
        }
    }
}

impl Incoming for WtListener {
    type Reader = WtReader;
    type Writer = WtWriter;

    fn transport_type(&self) -> TransportType {
        TransportType::WebTransport
    }

    async fn accept(&self) -> io::Result<Accepted> {
        self.admissions
            .next(|| async {
                let incoming = self.endpoint.accept().await.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotConnected, "webtransport endpoint closed")
                })?;
                let establish = tokio::time::timeout(
                    ESTABLISH_DEADLINE,
                    Self::establish(
                        incoming,
                        self.tokens.clone(),
                        self.inner.clone(),
                        Arc::clone(&self.refusals),
                    ),
                );
                Ok(Box::pin(async move {
                    establish
                        .await
                        .unwrap_or_else(|_| {
                            debug!("webtransport establishment timed out");
                            None
                        })
                        .map(Ok)
                }) as super::Admission<Accepted>)
            })
            .await
    }

    fn max_connections(&self) -> Option<usize> {
        self.tokens
            .is_none()
            .then_some(super::ANONYMOUS_MAX_CONNECTIONS)
    }

    fn kind(&self) -> &'static str {
        "webtransport"
    }
}

/// Admit a CONNECT from its raw QPACK payload, refusing duplicate
/// `Authorization` fields before a collapsed header map could hide them.
fn admit_connect(
    payload: &[u8],
    tokens: Option<&crate::auth::ReloadingTokenStore>,
) -> Result<Option<crate::auth::AuthenticatedCredential>, &'static str> {
    match connect_headers::has_duplicate_authorization(payload) {
        Ok(true) | Err(_) => {
            return Err("duplicate or malformed authorization");
        }
        Ok(false) => {}
    }
    let headers = Decoder::decode(payload).map_err(|_| "CONNECT QPACK decode failed")?;
    tokens.map_or(Ok(None), |store| {
        authorize_request(&headers, store)
            .map(Some)
            .ok_or("missing or invalid token")
    })
}

/// Verify the bearer token from exactly one carrier: an `Authorization:
/// Bearer` header or a `token=` query parameter on `:path`.
fn authorize_request(
    headers: &HashMap<String, String>,
    store: &crate::auth::ReloadingTokenStore,
) -> Option<crate::auth::AuthenticatedCredential> {
    let token_hex = request_token(headers)?;
    let token = hex::decode(token_hex.trim()).ok()?;
    store.authenticate_and_touch(&token)
}

fn request_token(headers: &HashMap<String, String>) -> Option<&str> {
    let path = headers.get(":path").map_or("/", String::as_str);
    match (unique_bearer(headers), unique_query_token(path)) {
        (UniqueToken::Valid(token), UniqueToken::Missing)
        | (UniqueToken::Missing, UniqueToken::Valid(token)) => Some(token),
        _ => None,
    }
}

enum UniqueToken<'a> {
    Missing,
    Valid(&'a str),
    Invalid,
}

/// The `Bearer` value of the one `Authorization` header, matching the field
/// name case-insensitively.
fn unique_bearer(headers: &HashMap<String, String>) -> UniqueToken<'_> {
    let mut values = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.as_str());
    let Some(value) = values.next() else {
        return UniqueToken::Missing;
    };
    if values.next().is_some() {
        return UniqueToken::Invalid;
    }
    value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .filter(|token| !token.is_empty())
        .map_or(UniqueToken::Invalid, UniqueToken::Valid)
}

/// The `token` query parameter of a `:path`, e.g. `/session?token=<hex>`.
fn unique_query_token(path: &str) -> UniqueToken<'_> {
    let Some((_, query)) = path.split_once('?') else {
        return UniqueToken::Missing;
    };
    let mut carriers = query
        .split('&')
        .filter(|part| *part == "token" || part.starts_with("token="));
    let Some(carrier) = carriers.next() else {
        return UniqueToken::Missing;
    };
    let Some(value) = carrier.strip_prefix("token=") else {
        return UniqueToken::Invalid;
    };
    if value.is_empty() || carriers.next().is_some() {
        return UniqueToken::Invalid;
    }
    UniqueToken::Valid(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::tls::ensure_self_signed;
    use wtransport::ClientConfig;
    use wtransport::endpoint::ConnectOptions;

    const TEST_TOKEN: [u8; crate::auth::TOKEN_LEN] = [0x11; crate::auth::TOKEN_LEN];
    /// One complete framed message: 4-byte length prefix (body = 3) + body.
    const FRAME: [u8; 7] = [0, 0, 0, 3, 0xde, 0xad, 0xbe];
    /// A second frame for the echo direction (server -> client).
    const ECHO_FRAME: [u8; 6] = [0, 0, 0, 2, 0xca, 0xfe];
    /// Hang guard for the no-stream admission wait, never a timing assertion.
    const HANG_GUARD: Duration = Duration::from_secs(60);

    /// A token store file holding the one known [`TEST_TOKEN`].
    fn token_store() -> (
        tempfile::NamedTempFile,
        Arc<crate::auth::ReloadingTokenStore>,
    ) {
        let file = tempfile::NamedTempFile::new().unwrap();
        crate::auth::write_test_credential(file.path(), &TEST_TOKEN);
        let store = crate::auth::ReloadingTokenStore::load(file.path().to_path_buf()).unwrap();
        (file, Arc::new(store))
    }

    /// A native client that skips certificate validation; the TLS handshake
    /// and HTTP/3 CONNECT are still exercised end to end.
    ///
    /// It binds IPv4 loopback like the listener, never the dual-stack
    /// default: macOS picks a dual-stack socket's ephemeral port without
    /// checking IPv4 bindings, and when it collides with another process's
    /// IPv4 socket that socket receives every reply, so the handshake
    /// silently times out after 30 s (about 1 run in 300 on a busy host).
    fn client_endpoint() -> wtransport::Endpoint<wtransport::endpoint::endpoint_side::Client> {
        let config = ClientConfig::builder()
            .with_bind_address(SocketAddr::from(([127, 0, 0, 1], 0)))
            .with_no_cert_validation()
            .build();
        wtransport::Endpoint::client(config).unwrap()
    }

    /// A loopback listener with a fresh self-signed pair, and its session
    /// URL base (`https://127.0.0.1:<port>/session`).
    fn listener(
        tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
    ) -> (tempfile::TempDir, WtListener, String) {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        ensure_self_signed(&cert, &key).unwrap();
        let listener =
            WtListener::from_pem("127.0.0.1:0".parse().unwrap(), &cert, &key, tokens, None)
                .unwrap();
        let url = format!(
            "https://127.0.0.1:{}/session",
            listener.local_addr().unwrap().port()
        );
        (dir, listener, url)
    }

    /// The listener accepts nothing while `options` is refused.
    async fn assert_refused(listener: &WtListener, options: ConnectOptions) {
        let client = async {
            assert!(client_endpoint().connect(options).await.is_err());
        };
        tokio::select! {
            () = client => {}
            _ = listener.accept() => panic!("a refused CONNECT was accepted"),
        }
    }

    /// A `paired` listener's end-to-end session (ADR-0154 item 6): an
    /// enrolled certificate presented inside the stream is the connection's
    /// credential; a plain stream (what a browser sends) is refused.
    #[tokio::test]
    async fn under_paired_only_an_inner_session_with_an_enrolled_certificate_is_admitted() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let paths = crate::workload::WorkloadPaths {
            ca_cert: dir.path().join("workload-ca.pem"),
            ca_key: dir.path().join("workload-ca.key"),
            registry: dir.path().join("workload-keys"),
        };
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        super::super::tls::ensure_server_identity(&cert, &key, &[], &paths).unwrap();
        let client_key = rcgen::KeyPair::generate().unwrap();
        let csr = rcgen::CertificateParams::new(Vec::<String>::new())
            .unwrap()
            .serialize_request(&client_key)
            .unwrap();
        let material =
            crate::workload::ClientMaterial::from_pem(csr.pem().unwrap().as_bytes()).unwrap();
        let expires = chrono::Utc::now().timestamp() + 3600;
        let prepared = crate::workload::prepare_enrollment(&paths, &material, expires).unwrap();
        let enrolled = prepared
            .commit(&paths.registry, vec!["*@global".to_owned()], expires)
            .unwrap();
        let (client_cert, client_key_path) = (dir.path().join("c.pem"), dir.path().join("c.key"));
        std::fs::write(&client_cert, prepared.issued_chain_pem().unwrap()).unwrap();
        std::fs::write(&client_key_path, client_key.serialize_pem()).unwrap();
        std::fs::set_permissions(&client_key_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let ca = crate::workload::authority_certificate(&paths.ca_cert).unwrap();
        let registry = Arc::new(
            crate::workload::ReloadingWorkloadRegistry::load(paths.registry.clone()).unwrap(),
        );
        let inner = super::super::inner_tls::InnerTls::new(
            &cert,
            &key,
            Some((&ca, registry)),
            paths.clone(),
        )
        .unwrap();
        let listener = WtListener::from_pem(
            "127.0.0.1:0".parse().unwrap(),
            &cert,
            &key,
            None,
            Some(Arc::new(inner)),
        )
        .unwrap();
        let url = format!(
            "https://127.0.0.1:{}/session",
            listener.local_addr().unwrap().port()
        );

        // A browser's plain stream: refused before any frame is read.
        let plain = async {
            let conn = client_endpoint().connect(url.clone()).await.unwrap();
            let (mut send, mut recv) = conn.open_bi().await.unwrap().await.unwrap();
            send.write_all(&FRAME).await.unwrap();
            let mut byte = [0_u8; 1];
            assert!(recv.read_exact(&mut byte).await.is_err(), "refused");
        };
        tokio::time::timeout(HANG_GUARD, async {
            tokio::select! {
                () = plain => {}
                _ = listener.accept() => panic!("a plain stream was admitted under paired"),
            }
        })
        .await
        .unwrap();

        let client = async {
            let conn = client_endpoint().connect(url).await.unwrap();
            let (send, recv) = conn.open_bi().await.unwrap().await.unwrap();
            let config = phux_dial::tls::client_config_with_identity(
                &phux_dial::CertTrust::Authority {
                    ca: crate::workload::ca_fingerprint(&paths.ca_cert).unwrap(),
                    leaf: None,
                },
                &phux_dial::TlsClientIdentity::PemFiles {
                    certificate: client_cert,
                    private_key: client_key_path,
                },
                Some(phux_protocol::policy::QUIC_ALPN),
            )
            .unwrap();
            let mut tls = tokio_rustls::TlsConnector::from(Arc::new(config))
                .connect(
                    rustls::pki_types::ServerName::try_from("localhost").unwrap(),
                    tokio::io::join(recv, send),
                )
                .await
                .unwrap();
            tls.write_all(&FRAME).await.unwrap();
            tls.flush().await.unwrap();
            // Hold the session until the server has read the frame.
            let mut byte = [0_u8; 1];
            let _ = tokio::time::timeout(Duration::from_secs(2), tls.read(&mut byte)).await;
            conn
        };
        let server = async {
            let (mut reader, _writer, peer) = listener.accept().await.unwrap();
            (reader.read_frame().await.unwrap(), peer)
        };
        let (_conn, (frame, peer)) = tokio::join!(client, server);
        assert_eq!(frame.unwrap().as_ref(), &FRAME);
        assert_eq!(peer.mcp_host_key.as_deref(), Some(enrolled.id.as_str()));
    }

    #[tokio::test]
    async fn round_trips_frames_unauthenticated() {
        let (_dir, listener, url) = listener(None);
        let server = async {
            let (mut reader, mut writer, peer) = listener.accept().await.unwrap();
            let got = reader.read_frame().await.unwrap();
            writer.write_frame(&ECHO_FRAME).await.unwrap();
            // Hold the session open until the client has read the echo.
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
        assert_eq!(got.unwrap().as_ref(), &FRAME);
        assert_eq!(echoed, ECHO_FRAME);
        assert_eq!(peer.transport, TransportType::WebTransport);
        assert!(peer.mcp_host_key.is_none());
    }

    /// With the acks cut off, the writer stops near one congestion window
    /// plus the unsent slack instead of the client's full stream credit.
    #[tokio::test]
    async fn writer_blocks_near_the_congestion_window_when_the_path_stalls() {
        let (_dir, listener, _) = listener(None);
        let proxy = phux_dial::testing::DropProxy::start(listener.local_addr().unwrap())
            .await
            .unwrap();
        let url = format!("https://127.0.0.1:{}/session", proxy.addr().port());

        let client = async {
            let conn = client_endpoint().connect(url).await.unwrap();
            let (mut send, recv) = conn.open_bi().await.unwrap().await.unwrap();
            send.write_all(&FRAME).await.unwrap();
            (conn, send, recv)
        };
        let server = async {
            let (mut reader, writer, _peer) = listener.accept().await.unwrap();
            reader.read_frame().await.unwrap();
            (reader, writer)
        };
        let (_client, (_reader, mut writer)) = tokio::join!(client, server);

        proxy.blackhole_downstream();
        let WtSend::Plain(send) = &mut writer.send else {
            panic!("a plain session");
        };
        let chunk = vec![0x5a_u8; 64 * 1024];
        let mut accepted = 0_usize;
        while accepted < 8 * 1024 * 1024 {
            match tokio::time::timeout(Duration::from_millis(300), send.write(&chunk)).await {
                Ok(Ok(written)) => accepted += written,
                Ok(Err(err)) => panic!("write failed: {err}"),
                Err(_) => break,
            }
        }
        let cwnd = send.window().connection().stats().path.cwnd;
        let bound = phux_dial::window::send_window_for(cwnd).max(64 * 1024);
        assert!(
            accepted as u64 <= bound,
            "WtWriter accepted {accepted} bytes against a {cwnd}-byte congestion window"
        );
    }

    /// Both carriers authenticate: the native header and the browser's URL
    /// query parameter.
    #[tokio::test]
    async fn bearer_header_and_query_token_authenticate() {
        let token = hex::encode(TEST_TOKEN);
        let (_tokens, store) = token_store();
        let (_dir, listener, url) = listener(Some(store));
        let carriers = [
            ConnectOptions::builder(&url)
                .add_header("authorization", format!("Bearer {token}"))
                .build(),
            ConnectOptions::builder(format!("{url}?token={token}")).build(),
        ];
        for options in carriers {
            let server = async {
                let (mut reader, _writer, peer) = listener.accept().await.unwrap();
                (reader.read_frame().await.unwrap(), peer)
            };
            let client = async {
                let conn = client_endpoint().connect(options).await.unwrap();
                let (mut send, _recv) = conn.open_bi().await.unwrap().await.unwrap();
                send.write_all(&FRAME).await.unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
            };
            let ((got, peer), ()) = tokio::join!(server, client);
            assert_eq!(got.unwrap().as_ref(), &FRAME);
            assert_eq!(peer.transport, TransportType::WebTransport);
            assert!(peer.mcp_host_key.is_some());
        }
    }

    /// Missing, unknown, mixed, duplicate, bare, and non-bearer carriers are
    /// all refused before a session exists.
    #[tokio::test]
    async fn bad_token_carriers_are_refused() {
        let (_tokens, store) = token_store();
        let (_dir, listener, url) = listener(Some(store));
        let token = hex::encode(TEST_TOKEN);
        let wrong = hex::encode([0x22u8; crate::auth::TOKEN_LEN]);
        let refused = [
            ConnectOptions::builder(&url).build(),
            ConnectOptions::builder(format!("{url}?token={wrong}")).build(),
            ConnectOptions::builder(format!("{url}?token={token}"))
                .add_header("authorization", format!("Bearer {token}"))
                .build(),
            ConnectOptions::builder(format!("{url}?token={token}&token={token}")).build(),
            ConnectOptions::builder(format!("{url}?token={token}&token")).build(),
            ConnectOptions::builder(&url)
                .add_header("authorization", "Basic not-a-bearer")
                .build(),
        ];
        for options in refused {
            assert_refused(&listener, options).await;
        }
    }

    /// wtransport's client cannot emit repeated identical `authorization`
    /// fields (its headers are a map), so speak raw HTTP/3 to send them and
    /// assert the 403 comes before token verification would have succeeded.
    #[tokio::test]
    async fn raw_duplicate_authorization_is_refused_before_auth() {
        let (_tokens, store) = token_store();
        let (_dir, listener, _) = listener(Some(store));
        let addr = listener.local_addr().unwrap();
        let bearer = format!("Bearer {}", hex::encode(TEST_TOKEN));

        let client = async {
            let (status, conn) = raw_connect_status(
                addr,
                vec![
                    (":method", "CONNECT"),
                    (":scheme", "https"),
                    (":protocol", "webtransport"),
                    (":authority", "127.0.0.1"),
                    (":path", "/session"),
                    ("authorization", bearer.as_str()),
                    ("authorization", bearer.as_str()),
                ],
            )
            .await;
            assert_eq!(status, 403);
            // A refused peer's connection is closed, not held open by the
            // listener's stream-drain task until the peer leaves.
            tokio::time::timeout(Duration::from_secs(5), conn.closed())
                .await
                .expect("the refused connection is closed");
        };
        tokio::select! {
            () = client => {}
            _ = listener.accept() => panic!("duplicate Authorization was accepted"),
        }
    }

    /// Drive a CONNECT whose QPACK payload is `headers` (preserving repeats)
    /// and return the `:status` the listener answers with, and the
    /// connection.
    async fn raw_connect_status(
        addr: SocketAddr,
        headers: Vec<(&str, &str)>,
    ) -> (u16, quinn::Connection) {
        use std::borrow::Cow;
        use wtransport_proto::frame::Frame;
        use wtransport_proto::qpack::Encoder;

        let crypto =
            phux_dial::tls::client_config(&phux_dial::CertTrust::SkipVerify, Some(b"h3")).unwrap();
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
        )));
        let conn = endpoint
            .connect(addr, "localhost")
            .unwrap()
            .await
            .expect("QUIC handshake");
        let _settings = h3::send_local_settings(&conn)
            .await
            .expect("client SETTINGS");
        let (mut send, mut recv) = conn.open_bi().await.expect("CONNECT stream");
        let payload = Encoder::encode(headers);
        Frame::new_headers(Cow::Owned(payload.into_vec()))
            .write_async(&mut h3::H3Send(&mut send))
            .await
            .expect("CONNECT HEADERS");
        let frame = Frame::read_async(&mut h3::H3Recv(&mut recv))
            .await
            .expect("CONNECT response");
        let status = Decoder::decode(frame.payload())
            .expect("response QPACK")
            .get(":status")
            .expect("response :status")
            .parse()
            .expect("numeric :status");
        (status, conn)
    }

    #[tokio::test]
    async fn no_stream_session_does_not_block_next_consumer() {
        let (_dir, listener, url) = listener(None);
        let clients = async {
            let stalled = client_endpoint().connect(&url).await.unwrap();
            let conn = client_endpoint().connect(&url).await.unwrap();
            let (mut send, _recv) = conn.open_bi().await.unwrap().await.unwrap();
            send.write_all(&FRAME).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(stalled);
        };
        let accepted = async {
            let (mut reader, _writer, _peer) = listener.accept().await.unwrap();
            reader.read_frame().await.unwrap().unwrap()
        };

        let ((), frame) =
            tokio::time::timeout(HANG_GUARD, async { tokio::join!(clients, accepted) })
                .await
                .expect("healthy consumer must not wait for stalled first stream");
        assert_eq!(frame.as_ref(), FRAME);
    }
}
