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
use futures_util::future::{FutureExt as _, LocalBoxFuture};
use futures_util::stream::{FuturesUnordered, StreamExt as _};
use phux_dial::window::{SendWindow, TrackedSend};
use phux_protocol::policy::{PeerIdentity, TransportType};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{debug, warn};
use wtransport_proto::qpack::Decoder;

use super::quic::QuicBindError as WtBindError;
use super::{FrameReader, FrameWriter, Incoming};

mod connect_headers;
mod h3;

/// Bound on the HTTP/3 request, token gate, session accept, and first stream.
const ESTABLISH_DEADLINE: Duration = super::HANDSHAKE_DEADLINE;

/// Sessions establishing concurrently: finite, but more than one so a slow
/// peer cannot serialize the listener.
const MAX_PENDING_ESTABLISHMENTS: usize = 32;

type Accepted = (WtReader, WtWriter, crate::auth::ConnectionIdentity);
type PendingEstablishments = FuturesUnordered<LocalBoxFuture<'static, Option<Accepted>>>;

/// A WebTransport listener, optionally token-authenticated.
pub(crate) struct WtListener {
    endpoint: quinn::Endpoint,
    tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
    pending: Mutex<PendingEstablishments>,
}

impl WtListener {
    /// Bind a listener; `tokens` requires a bearer token on every `CONNECT`.
    pub(crate) fn from_pem(
        addr: SocketAddr,
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
        tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
    ) -> Result<Self, WtBindError> {
        let tls = super::tls::webtransport_server_config(cert_path, key_path)?;
        Ok(Self {
            endpoint: super::quic::server_endpoint(addr, tls, None)?,
            tokens,
            pending: Mutex::new(FuturesUnordered::new()),
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
    ) -> Option<Accepted> {
        let connection = match incoming.await {
            Ok(connection) => connection,
            Err(err) => {
                debug!(error = %err, "webtransport session handshake failed");
                return None;
            }
        };
        let remote = connection.remote_address();

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
                warn!(%remote, "webtransport consumer refused: {reason}");
                let _ = h3::send_connect_status(&mut connect_send, false).await;
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

        let peer_identity = PeerIdentity {
            uid: 0,
            pid: None,
            exe_path: None,
            mcp_host_key: credential.as_ref().map(|credential| credential.id.clone()),
            transport: TransportType::WebTransport,
            source_addr: Some(remote.ip()),
        };
        let bearer = tokens
            .as_ref()
            .zip(credential.as_ref())
            .map(|(store, credential)| {
                crate::auth::BearerAdmission::new(Arc::clone(store), credential)
            });

        Some((
            WtReader {
                _session: h3::SessionStreams {
                    _connection: connection.clone(),
                    _connect_send: connect_send,
                    _connect_recv: connect_recv,
                    _settings_send: settings_send,
                },
                recv,
            },
            WtWriter {
                send: TrackedSend::new(send, SendWindow::new(connection.clone())),
                _connection: connection,
            },
            crate::auth::ConnectionIdentity {
                peer: peer_identity,
                credential,
                ssh_origin: None,
                bearer,
            },
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

/// WebTransport read half: the same length-prefixed framing as QUIC.
pub(crate) struct WtReader {
    /// Keeps the HTTP/3 CONNECT session and control stream alive.
    _session: h3::SessionStreams,
    recv: quinn::RecvStream,
}

impl FrameReader for WtReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        super::quic::read_framed(&mut self.recv).await
    }
}

/// WebTransport write half, through the same congestion-tracked
/// [`TrackedSend`] as `QuicWriter`, so a slow browser blocks within about a
/// round trip instead of queueing megabytes.
pub(crate) struct WtWriter {
    send: TrackedSend<quinn::SendStream>,
    /// Keeps the WebTransport session alive for the stream's lifetime.
    _connection: quinn::Connection,
}

impl FrameWriter for WtWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.send.write_all(frame).await
    }

    /// One write for the whole batch, as `QuicWriter::write_frames`.
    async fn write_frames(&mut self, batch: &[u8], _ends: &[usize]) -> io::Result<()> {
        self.send.write_all(batch).await
    }

    #[allow(
        clippy::unused_async_trait_impl,
        reason = "FrameWriter requires an async close; Quinn's finish is synchronous"
    )]
    async fn close(&mut self) -> io::Result<()> {
        self.send.get_mut().finish().map_err(io::Error::other)
    }
}

impl Incoming for WtListener {
    type Reader = WtReader;
    type Writer = WtWriter;

    fn transport_type(&self) -> TransportType {
        TransportType::WebTransport
    }

    async fn accept(&self) -> io::Result<(WtReader, WtWriter, crate::auth::ConnectionIdentity)> {
        loop {
            let mut pending = self.pending.lock().await;
            let incoming = tokio::select! {
                completed = pending.next(), if !pending.is_empty() => {
                    if let Some(Some(accepted)) = completed {
                        return Ok(accepted);
                    }
                    None
                }
                incoming = self.endpoint.accept(), if pending.len() < MAX_PENDING_ESTABLISHMENTS => {
                    Some(incoming.ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::NotConnected,
                            "webtransport endpoint closed",
                        )
                    })?)
                }
            };
            drop(pending);
            if let Some(incoming) = incoming {
                let establish = tokio::time::timeout(
                    ESTABLISH_DEADLINE,
                    Self::establish(incoming, self.tokens.clone()),
                )
                .map(|result| {
                    result.unwrap_or_else(|_| {
                        debug!("webtransport establishment timed out");
                        None
                    })
                });
                self.pending.lock().await.push(establish.boxed_local());
            }
        }
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
    fn client_endpoint() -> wtransport::Endpoint<wtransport::endpoint::endpoint_side::Client> {
        let config = ClientConfig::builder()
            .with_bind_default()
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
            WtListener::from_pem("127.0.0.1:0".parse().unwrap(), &cert, &key, tokens).unwrap();
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
        let chunk = vec![0x5a_u8; 64 * 1024];
        let mut accepted = 0_usize;
        while accepted < 8 * 1024 * 1024 {
            match tokio::time::timeout(Duration::from_millis(300), writer.send.write(&chunk)).await
            {
                Ok(Ok(written)) => accepted += written,
                Ok(Err(err)) => panic!("write failed: {err}"),
                Err(_) => break,
            }
        }
        let cwnd = writer.send.window().connection().stats().path.cwnd;
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
            let status = raw_connect_status(
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
        };
        tokio::select! {
            () = client => {}
            _ = listener.accept() => panic!("duplicate Authorization was accepted"),
        }
    }

    /// Drive a CONNECT whose QPACK payload is `headers` (preserving repeats)
    /// and return the `:status` the listener answers with.
    async fn raw_connect_status(addr: SocketAddr, headers: Vec<(&str, &str)>) -> u16 {
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
        Decoder::decode(frame.payload())
            .expect("response QPACK")
            .get(":status")
            .expect("response :status")
            .parse()
            .expect("numeric :status")
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
