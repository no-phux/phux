//! QUIC transport (`phux-y8v6`, [ADR-0007]).
//!
//! QUIC carries the **identical** length-prefixed phux frames the UDS and
//! WebSocket transports do (`docs/spec/proto.md` §5); only the byte stream
//! underneath differs. Each accepted connection opens exactly one
//! bidirectional QUIC stream — a reliable, ordered, octet stream, which is all
//! the wire contract requires — and frames flow over it exactly as over a Unix
//! socket. quinn's `RecvStream`/`SendStream` are the byte plumbing; the
//! `FrameReader`/`FrameWriter`/`Incoming` impls below are thin reframing.
//!
//! Why QUIC at all (ADR-0007): connection migration (roaming across networks),
//! 0-RTT resumption (sub-second reconnect), and mandatory TLS 1.3 — the
//! Mosh-class UX properties — without reimplementing SSP. quinn is the stack
//! ADR-0007 names; it rides the same rustls 0.23 + `ring` provider as the
//! `wss://` path (`transport::tls`), so QUIC adds no new crypto toolchain.
//!
//! **Auth.** TLS 1.3 is intrinsic to QUIC, so confidentiality is never
//! optional here. For *authentication* of routable (non-loopback) consumers
//! this transport mirrors the WebSocket bearer-token model (ADR-0031): the
//! dialer sends a length-prefixed token **preamble** as the very first bytes of
//! the bidi stream, validated against the [`TokenStore`](crate::auth) inside
//! `Incoming::accept` before any phux frame is read — the QUIC analogue of
//! the `Authorization: Bearer` header the WebSocket path validates during its
//! upgrade. The preamble is a transport-establishment detail, not a phux wire
//! frame, so the `FrameKind` codec is untouched. On a loopback (unauthenticated)
//! listener no preamble is expected and frames start immediately.
//!
//! [ADR-0007]: ../../../ADR/0007-mosh-class-transport-and-satellites.md

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::frame::MAX_FRAME_LEN;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use super::tls::quic_server_config;
use super::{FrameReader, FrameWriter, Incoming, LENGTH_PREFIX};

/// Upper bound on the token preamble body, in bytes. Generous relative to the
/// fixed token length so a forward-compatible longer token still parses, but
/// small enough that a malformed/hostile length can never allocate much.
const MAX_TOKEN_PREAMBLE: usize = 256;

/// QUIC idle timeout. A connection with no traffic and no keep-alive for this
/// long is dropped; migration/roaming reconnects re-establish within it.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep-alive interval. Comfortably under [`IDLE_TIMEOUT`] so an attached but
/// quiet client (no keystrokes, no output) holds its connection open across
/// NATs rather than being reaped.
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// QUIC application close code for a connection refused at the auth preamble.
const AUTH_FAILED_CODE: u32 = 0x01;

/// A QUIC listener: a quinn [`Endpoint`](quinn::Endpoint) bound to a UDP
/// socket, optionally token-authenticated for routable consumers.
pub(crate) struct QuicListener {
    endpoint: quinn::Endpoint,
    tokens: Option<Arc<crate::auth::TokenStore>>,
}

impl QuicListener {
    /// Bind a QUIC listener: build the (always-TLS) rustls config from the
    /// persisted cert + key, then open the endpoint. `tokens` selects the auth
    /// mode — `Some` requires a valid bearer-token preamble from each dialer
    /// (routable consumers, ADR-0031 parity with `wss://`); `None` is the
    /// loopback/dev path that expects no preamble. QUIC is TLS-encrypted in
    /// both modes (the protocol mandates it).
    pub(crate) fn from_pem(
        addr: SocketAddr,
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
        tokens: Option<Arc<crate::auth::TokenStore>>,
    ) -> Result<Self, QuicBindError> {
        let tls = quic_server_config(cert_path, key_path)?;
        Ok(Self {
            endpoint: build_endpoint(addr, tls)?,
            tokens,
        })
    }

    /// The local address the endpoint is bound to (for logging the OS-assigned
    /// port when binding to `:0`).
    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }
}

/// Errors from constructing a [`QuicListener`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum QuicBindError {
    /// Building the rustls/QUIC crypto config failed.
    #[error("quic tls: {0}")]
    Tls(#[from] super::tls::TlsError),
    /// The QUIC crypto config had no usable initial cipher suite.
    #[error("quic crypto: {0}")]
    Crypto(#[from] quinn::crypto::rustls::NoInitialCipherSuite),
    /// Binding the UDP endpoint failed.
    #[error("quic bind: {0}")]
    Io(#[from] io::Error),
}

/// Assemble a quinn server [`Endpoint`](quinn::Endpoint) from a rustls config.
fn build_endpoint(
    addr: SocketAddr,
    tls: rustls::ServerConfig,
) -> Result<quinn::Endpoint, QuicBindError> {
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(crypto));

    let mut transport = quinn::TransportConfig::default();
    // `try_into` only fails on a duration that overflows QUIC's varint idle
    // encoding; our constants are well within range.
    if let Ok(idle) = IDLE_TIMEOUT.try_into() {
        transport.max_idle_timeout(Some(idle));
    }
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    server_config.transport_config(Arc::new(transport));

    Ok(quinn::Endpoint::server(server_config, addr)?)
}

/// QUIC read half: reassembles length-prefixed frames off the bidi stream,
/// byte-for-byte the same framing as the UDS path.
pub(crate) struct QuicReader {
    recv: quinn::RecvStream,
    header: [u8; LENGTH_PREFIX],
}
impl QuicReader {
    /// Wrap one already-authenticated QUIC receive stream in phux framing.
    pub(crate) const fn from_stream(recv: quinn::RecvStream) -> Self {
        Self {
            recv,
            header: [0u8; LENGTH_PREFIX],
        }
    }
}

impl FrameReader for QuicReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        if !read_exact_quic(&mut self.recv, &mut self.header).await? {
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
        if !read_exact_quic(&mut self.recv, &mut framed[LENGTH_PREFIX..]).await? {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stream finished mid-frame",
            ));
        }
        Ok(Some(framed))
    }
}

/// QUIC write half.
pub(crate) struct QuicWriter {
    send: quinn::SendStream,
}
impl QuicWriter {
    /// Wrap one already-authenticated QUIC send stream in phux framing.
    pub(crate) const fn from_stream(send: quinn::SendStream) -> Self {
        Self { send }
    }
}

impl FrameWriter for QuicWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.send.write_all(frame).await.map_err(io::Error::other)
    }

    async fn close(&mut self) -> io::Result<()> {
        self.send.finish().map_err(io::Error::other)
    }
}

impl Incoming for QuicListener {
    type Reader = QuicReader;
    type Writer = QuicWriter;

    async fn accept(&self) -> io::Result<(QuicReader, QuicWriter, PeerIdentity)> {
        // One QUIC endpoint multiplexes many connections; a single bad
        // handshake or refused token must not tear the listener down, so
        // per-connection failures `continue` (logged) and only an endpoint
        // closure ends the loop. This is the multiplexed-endpoint analogue of
        // the per-`accept()` TCP loop the WebSocket path runs.
        loop {
            let incoming = self.endpoint.accept().await.ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "quic endpoint closed")
            })?;
            let remote = incoming.remote_address();

            let conn = match incoming.await {
                Ok(conn) => conn,
                Err(err) => {
                    debug!(%remote, error = %err, "quic handshake failed");
                    continue;
                }
            };

            // The consumer opens one bidi stream and immediately writes its
            // first bytes (token preamble, then frames), so `accept_bi`
            // resolves promptly.
            let (send, mut recv) = match conn.accept_bi().await {
                Ok(pair) => pair,
                Err(err) => {
                    debug!(%remote, error = %err, "quic stream accept failed");
                    continue;
                }
            };

            let device_id = match &self.tokens {
                Some(store) => {
                    let Some(id) = authorize_preamble(&mut recv, store).await else {
                        warn!(%remote, "quic consumer refused: missing or invalid token");
                        conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
                        continue;
                    };
                    Some(id)
                }
                None => None,
            };

            let peer_identity = PeerIdentity {
                uid: 0,
                pid: None,
                exe_path: None,
                mcp_host_key: device_id,
                transport: TransportType::Quic,
                source_addr: Some(remote.ip()),
            };

            return Ok((
                QuicReader::from_stream(recv),
                QuicWriter::from_stream(send),
                peer_identity,
            ));
        }
    }

    fn kind(&self) -> &'static str {
        "quic"
    }
}

/// Read the token preamble (`len: u32 BE` + `len` token bytes) off the stream
/// and verify it against the store. Returns a non-reversible device id (a short
/// SHA-256 prefix of the *presented* token, matching the WebSocket path) on
/// success, or `None` on a missing, oversized, malformed, or unrecognized
/// token. Deriving the id from the presented token (not the matched stored one)
/// keeps it off the constant-time comparison and never logs the secret.
pub(crate) async fn authorize_preamble(
    recv: &mut quinn::RecvStream,
    store: &crate::auth::TokenStore,
) -> Option<String> {
    let mut len_buf = [0u8; LENGTH_PREFIX];
    if !read_exact_quic(recv, &mut len_buf).await.ok()? {
        return None;
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > MAX_TOKEN_PREAMBLE {
        return None;
    }
    let mut token = vec![0u8; len];
    if !read_exact_quic(recv, &mut token).await.ok()? {
        return None;
    }
    if !store.verify(&token) {
        return None;
    }
    let digest = Sha256::digest(&token);
    Some(hex::encode(&digest[..8]))
}

/// Fill `buf` from the QUIC stream. Returns `Ok(true)` when `buf` is filled,
/// `Ok(false)` on a clean stream finish before any byte was read (end of the
/// connection at a frame boundary), and `Err` on a partial-then-finished read
/// (a truncated frame) or a transport error.
async fn read_exact_quic(recv: &mut quinn::RecvStream, buf: &mut [u8]) -> io::Result<bool> {
    match recv.read_exact(buf).await {
        Ok(()) => Ok(true),
        // Zero bytes before the stream finished is a clean EOF at a frame
        // boundary; any other shortfall is a truncated frame.
        Err(quinn::ReadExactError::FinishedEarly(0)) => Ok(false),
        Err(err) => Err(io::Error::other(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::time::Duration;

    use super::super::tls::{QUIC_ALPN, ensure_self_signed};

    const TEST_TOKEN: [u8; crate::auth::TOKEN_LEN] = [0x11; crate::auth::TOKEN_LEN];
    /// One complete framed message: 4-byte length prefix (body = 3) + body.
    const FRAME: [u8; 7] = [0, 0, 0, 3, 0xde, 0xad, 0xbe];

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

    /// Test-only cert verifier: QUIC mandates ALPN + TLS so the handshake is
    /// still exercised end-to-end, but the self-signed leaf is trusted blindly
    /// rather than pinned (the dialer's fingerprint-pinning is out of scope for
    /// the listener under test).
    #[derive(Debug)]
    struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

    impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &rustls::pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &rustls::pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    /// A quinn client endpoint that offers the phux ALPN and trusts the
    /// listener's self-signed cert.
    fn client_endpoint() -> quinn::Endpoint {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut crypto = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification(provider)))
            .with_no_client_auth();
        crypto.alpn_protocols = vec![QUIC_ALPN.to_vec()];
        let client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
        ));
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(client_config);
        endpoint
    }

    /// Frame a token as the auth preamble: `len: u32 BE` + token bytes.
    fn token_preamble(token: &[u8]) -> Vec<u8> {
        let mut buf = (u32::try_from(token.len()).unwrap()).to_be_bytes().to_vec();
        buf.extend_from_slice(token);
        buf
    }

    #[tokio::test]
    async fn round_trips_a_frame_unauthenticated() {
        let (_dir, cert, key) = cert_pair();
        let listener =
            QuicListener::from_pem("127.0.0.1:0".parse().unwrap(), &cert, &key, None).unwrap();
        let addr = listener.local_addr().unwrap();

        let server = async {
            let (mut reader, _writer, peer) = listener.accept().await.unwrap();
            let got = reader.read_frame().await.unwrap();
            (got, peer)
        };
        let client = async {
            let endpoint = client_endpoint();
            let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
            let (mut send, _recv) = conn.open_bi().await.unwrap();
            send.write_all(&FRAME).await.unwrap();
            // Hold the connection open until the server has read the frame.
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        let ((got, peer), ()) = tokio::join!(server, client);
        assert_eq!(got.unwrap().as_ref(), &FRAME, "frame round-trips over QUIC");
        assert_eq!(peer.transport, TransportType::Quic);
        assert!(
            peer.mcp_host_key.is_none(),
            "an unauthenticated loopback peer carries no device id"
        );
    }

    #[tokio::test]
    async fn valid_token_preamble_authenticates_and_round_trips() {
        let (_dir, cert, key) = cert_pair();
        let (_tok_file, store) = token_store();
        let listener =
            QuicListener::from_pem("127.0.0.1:0".parse().unwrap(), &cert, &key, Some(store))
                .unwrap();
        let addr = listener.local_addr().unwrap();

        let server = async {
            let (mut reader, _writer, peer) = listener.accept().await.unwrap();
            let got = reader.read_frame().await.unwrap();
            (got, peer)
        };
        let client = async {
            let endpoint = client_endpoint();
            let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
            let (mut send, _recv) = conn.open_bi().await.unwrap();
            send.write_all(&token_preamble(&TEST_TOKEN)).await.unwrap();
            send.write_all(&FRAME).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        let ((got, peer), ()) = tokio::join!(server, client);
        assert_eq!(
            got.unwrap().as_ref(),
            &FRAME,
            "frame round-trips after auth"
        );
        assert_eq!(peer.transport, TransportType::Quic);
        assert!(
            peer.mcp_host_key.is_some(),
            "an authenticated remote peer is non-anonymous"
        );
    }

    #[tokio::test]
    async fn invalid_token_preamble_is_refused() {
        let (_dir, cert, key) = cert_pair();
        let (_tok_file, store) = token_store();
        let listener =
            QuicListener::from_pem("127.0.0.1:0".parse().unwrap(), &cert, &key, Some(store))
                .unwrap();
        let addr = listener.local_addr().unwrap();

        // The listener loops internally on a refused connection (it serves a
        // multiplexed endpoint), so drive `accept` on a detached task and
        // assert the refusal from the *client* side: the server closes the
        // connection with the auth error code before any frame is read.
        let server = tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let endpoint = client_endpoint();
        let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
        let (mut send, _recv) = conn.open_bi().await.unwrap();
        let wrong = [0x22u8; crate::auth::TOKEN_LEN];
        let _ = send.write_all(&token_preamble(&wrong)).await;

        let closed = tokio::time::timeout(Duration::from_secs(5), conn.closed())
            .await
            .expect("server must refuse promptly, not hang");
        match closed {
            quinn::ConnectionError::ApplicationClosed(close) => {
                assert_eq!(u64::from(AUTH_FAILED_CODE), close.error_code.into_inner());
            }
            other => panic!("expected application close on auth failure, got {other:?}"),
        }
        server.abort();
    }
}
