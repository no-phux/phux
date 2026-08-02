//! Transport abstraction for the accept loop (`phux-486.4`).
//!
//! The server speaks one wire — length-prefixed phux frames (`docs/spec/proto.md`
//! §5) — over more than one transport. UDS is the default local transport; a
//! WebSocket transport lets browser consumers (the `phux-web` client) speak the
//! *identical* frames. We abstract at the **frame** level: each transport yields
//! complete encoded frames, so the per-client dispatch loop in [`crate::runtime`]
//! and the `FrameKind` codec are transport-agnostic and reused verbatim.
//!
//! Wire contract per transport:
//! * **UDS** — frames are length-prefixed on the byte stream, exactly as today.
//! * **WebSocket** — one binary message carries one complete encoded frame
//!   (the 4-byte length prefix is included, so the same `FrameKind::decode`
//!   path works on both ends). Text/ping/pong frames are ignored; a Close
//!   message is EOF.

#![allow(
    clippy::future_not_send,
    reason = "single-threaded tokio runtime per ADR-0003; the token-auth accept path captures !Send Rc state and never crosses threads"
)]

pub mod quic;
pub mod tls;
#[cfg(feature = "webtransport")]
pub mod webtransport;

use std::io;
use std::net::SocketAddr;

use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use phux_protocol::policy::{PeerIdentity, TransportType};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream, UnixListener};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request};

use phux_protocol::wire::frame::MAX_FRAME_LEN;

const LENGTH_PREFIX: usize = 4;

/// Read side of a client connection: yields one complete encoded frame (length
/// prefix included) per call, or `None` at end-of-stream.
pub(crate) trait FrameReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>>;
}

/// Write side: writes one complete pre-encoded frame.
pub(crate) trait FrameWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()>;

    /// Finish the server-to-client stream after its final frame.
    async fn close(&mut self) -> io::Result<()>;
}

/// A listener that accepts connections, each split into a frame reader + writer.
pub(crate) trait Incoming {
    type Reader: FrameReader + 'static;
    type Writer: FrameWriter + 'static;
    async fn accept(&self) -> io::Result<(Self::Reader, Self::Writer, PeerIdentity)>;
    /// Whether an accept error means the whole incoming source is gone.
    ///
    /// Socket listeners keep serving after transient per-connection errors.
    /// A dial-out connector wraps one established QUIC connection, so an
    /// `accept_bi` error means the relay leg is lost and its supervisor must
    /// redial.
    fn accept_errors_are_fatal(&self) -> bool {
        false
    }
    /// Short transport label for logs (`"uds"` / `"ws"`).
    fn kind(&self) -> &'static str;
}

// ── Unix domain socket ───────────────────────────────────────────────────────

/// UDS read half: reassembles length-prefixed frames off the byte stream.
pub(crate) struct UdsReader {
    reader: OwnedReadHalf,
    header: [u8; LENGTH_PREFIX],
}

impl FrameReader for UdsReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        match self.reader.read_exact(&mut self.header).await {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(err),
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
        self.reader.read_exact(&mut framed[LENGTH_PREFIX..]).await?;
        Ok(Some(framed))
    }
}

/// UDS write half.
pub(crate) struct UdsWriter {
    writer: OwnedWriteHalf,
}

impl FrameWriter for UdsWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.writer.write_all(frame).await
    }

    async fn close(&mut self) -> io::Result<()> {
        self.writer.shutdown().await
    }
}

/// UDS listener: a thin newtype around [`UnixListener`] so the `Incoming::accept`
/// impl doesn't shadow the inherent `UnixListener::accept`.
pub(crate) struct UdsListener(UnixListener);

impl UdsListener {
    pub(crate) const fn new(listener: UnixListener) -> Self {
        Self(listener)
    }

    /// The raw listening-socket descriptor, captured at startup for the
    /// graceful-upgrade handoff (ADR-0032): cleared of `FD_CLOEXEC` and
    /// inherited by the re-exec'd image so the socket stays bound.
    pub(crate) fn as_raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.0.as_raw_fd()
    }
}

impl Incoming for UdsListener {
    type Reader = UdsReader;
    type Writer = UdsWriter;

    async fn accept(&self) -> io::Result<(UdsReader, UdsWriter, PeerIdentity)> {
        let (stream, _addr) = self.0.accept().await?;
        let peer_identity = peer_identity_from_uds(&stream)?;
        let (reader, writer) = stream.into_split();
        Ok((
            UdsReader {
                reader,
                header: [0u8; LENGTH_PREFIX],
            },
            UdsWriter { writer },
            peer_identity,
        ))
    }

    fn kind(&self) -> &'static str {
        "uds"
    }
}

fn peer_identity_from_credentials(
    credentials: io::Result<(u32, Option<u32>)>,
) -> io::Result<PeerIdentity> {
    let (uid, pid) = credentials?;
    Ok(PeerIdentity {
        uid,
        pid,
        exe_path: None,
        mcp_host_key: None,
        transport: TransportType::UnixSocket,
        source_addr: None,
    })
}

/// Extract peer identity from a Unix domain socket.
#[cfg(target_os = "linux")]
fn peer_identity_from_uds(stream: &tokio::net::UnixStream) -> io::Result<PeerIdentity> {
    peer_identity_from_credentials(stream.peer_cred().map(|cred| {
        (
            cred.uid(),
            cred.pid().and_then(|pid| u32::try_from(pid).ok()),
        )
    }))
}

/// Extract peer identity from a Unix domain socket on Darwin.
#[cfg(target_os = "macos")]
fn peer_identity_from_uds(stream: &tokio::net::UnixStream) -> io::Result<PeerIdentity> {
    use std::os::fd::AsRawFd as _;

    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: both output pointers are valid for writes for the duration of
    // the call, and `stream` owns a live Unix-domain socket descriptor.
    let status = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    peer_identity_from_credentials(Ok((uid, None)))
}

/// Reject UDS transports on targets without an authenticated peer credential API.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_identity_from_uds(_stream: &tokio::net::UnixStream) -> io::Result<PeerIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "authenticated Unix peer credentials are unavailable",
    ))
}

// ── WebSocket ────────────────────────────────────────────────────────────────

/// The byte stream under a WebSocket: plaintext TCP (local browser client,
/// loopback only) or TLS (remote consumer over `wss://`, ADR-0031). Both ends
/// are `Unpin`, so the `AsyncRead`/`AsyncWrite` forwarding below projects with
/// `Pin::new` and needs no `unsafe`.
pub(crate) enum ServerStream {
    /// Plaintext TCP — the loopback browser-client path.
    Plain(TcpStream),
    /// TLS-terminated — the authenticated remote-consumer path. Boxed because
    /// `TlsStream` is large and the `Plain` variant should stay cheap.
    Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
}

impl tokio::io::AsyncRead for ServerStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for ServerStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

type Ws = WebSocketStream<ServerStream>;

/// WebSocket listener: TCP + RFC 6455 upgrade, then one binary message per frame.
///
/// Optionally TLS-terminated and token-authenticated for remote consumers
/// (ADR-0031). When `tls` is set, each connection is wrapped in TLS before the
/// upgrade; when `tokens` is set, the upgrade request must carry a valid
/// `Authorization: Bearer <hex>` or the handshake is refused with HTTP 401.
/// Both unset is the historical loopback browser-client path.
pub(crate) struct WsListener {
    tcp: TcpListener,
    tls: Option<tokio_rustls::TlsAcceptor>,
    tokens: Option<std::sync::Arc<crate::auth::TokenStore>>,
}

impl WsListener {
    /// Bind a plaintext, unauthenticated listener (loopback browser client).
    pub(crate) async fn bind(addr: SocketAddr) -> io::Result<Self> {
        Ok(Self {
            tcp: TcpListener::bind(addr).await?,
            tls: None,
            tokens: None,
        })
    }

    /// Bind a TLS-terminated, token-authenticated listener for remote consumers.
    ///
    /// TLS is mandatory here: the bearer token is sent in the (TLS-protected)
    /// handshake, so there is no token-over-plaintext path. ADR-0031's
    /// no-plaintext-remote invariant is enforced by this constructor being the
    /// only way to attach a token store.
    pub(crate) async fn bind_secure(
        addr: SocketAddr,
        tls: tokio_rustls::TlsAcceptor,
        tokens: std::sync::Arc<crate::auth::TokenStore>,
    ) -> io::Result<Self> {
        Ok(Self {
            tcp: TcpListener::bind(addr).await?,
            tls: Some(tls),
            tokens: Some(tokens),
        })
    }

    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.local_addr()
    }
}

/// WebSocket read half: each binary message is one complete encoded frame.
pub(crate) struct WsReader {
    rx: futures_util::stream::SplitStream<Ws>,
}

impl FrameReader for WsReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        loop {
            match self.rx.next().await {
                None | Some(Ok(Message::Close(_))) => return Ok(None),
                Some(Ok(Message::Binary(data))) => {
                    let len = data.len();
                    if len < LENGTH_PREFIX || len > LENGTH_PREFIX + MAX_FRAME_LEN as usize {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "websocket frame out of bounds",
                        ));
                    }
                    return Ok(Some(BytesMut::from(&data[..])));
                }
                Some(Err(err)) => return Err(io::Error::other(err)),
                // Ignore text / ping / pong / raw — the wire is binary frames only.
                Some(Ok(_)) => {}
            }
        }
    }
}

/// WebSocket write half.
pub(crate) struct WsWriter {
    tx: futures_util::stream::SplitSink<Ws, Message>,
}

impl FrameWriter for WsWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.tx
            .send(Message::Binary(frame.to_vec()))
            .await
            .map_err(io::Error::other)
    }

    async fn close(&mut self) -> io::Result<()> {
        self.tx.close().await.map_err(io::Error::other)
    }
}

impl Incoming for WsListener {
    type Reader = WsReader;
    type Writer = WsWriter;

    async fn accept(&self) -> io::Result<(WsReader, WsWriter, PeerIdentity)> {
        let (tcp, peer) = self.tcp.accept().await?;

        // TLS handshake first (if configured), so the bearer token in the
        // upgrade request is already encrypted when we read it.
        let stream = match &self.tls {
            Some(acceptor) => ServerStream::Tls(Box::new(acceptor.accept(tcp).await?)),
            None => ServerStream::Plain(tcp),
        };

        // WebSocket upgrade. With a token store, validate the
        // `Authorization: Bearer` header during the handshake and refuse with
        // HTTP 401 before any phux frame is read; the matched device's
        // (non-reversible) id is captured for the peer identity. Without one,
        // this is the historical anonymous browser-client path.
        let (ws, device_id) = match &self.tokens {
            Some(store) => {
                let store = store.clone();
                let captured: std::rc::Rc<std::cell::RefCell<Option<String>>> =
                    std::rc::Rc::new(std::cell::RefCell::new(None));
                let sink = captured.clone();
                let ws = tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, resp| {
                    authorize_request(req, &store).map_or_else(
                        || Err(unauthorized_response()),
                        |id| {
                            *sink.borrow_mut() = Some(id);
                            Ok(resp)
                        },
                    )
                })
                .await
                .map_err(io::Error::other)?;
                let id = captured.borrow_mut().take();
                (ws, id)
            }
            None => (
                tokio_tungstenite::accept_async(stream)
                    .await
                    .map_err(io::Error::other)?,
                None,
            ),
        };

        // An authenticated remote consumer is a first-class peer: its
        // device id rides `mcp_host_key` (the existing attestation slot), so
        // policy and audit see a non-anonymous identity rather than the
        // `uid: 0` stamp the plaintext browser path carries.
        let peer_identity = PeerIdentity {
            uid: 0,
            pid: None,
            exe_path: None,
            mcp_host_key: device_id,
            transport: TransportType::WebSocket,
            source_addr: Some(peer.ip()),
        };

        let (tx, rx) = ws.split();
        Ok((WsReader { rx }, WsWriter { tx }, peer_identity))
    }

    fn kind(&self) -> &'static str {
        "ws"
    }
}

/// Extract and verify the `Authorization: Bearer <hex>` pairing token from a
/// WebSocket upgrade request. Returns a non-reversible device id (a short
/// SHA-256 prefix of the *presented* token) on success, `None` on a missing,
/// malformed, or unrecognized token.
fn authorize_request(req: &Request, store: &crate::auth::TokenStore) -> Option<String> {
    let header = req.headers().get("authorization")?.to_str().ok()?;
    let token_hex = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))?
        .trim();
    let token = hex::decode(token_hex).ok()?;
    if !store.verify(&token) {
        return None;
    }
    // Device id is derived from the presented token (not from which stored
    // token matched), so deriving it never branches on the constant-time
    // comparison and never logs the secret itself.
    let digest = Sha256::digest(&token);
    Some(hex::encode(&digest[..8]))
}

/// The HTTP 401 the handshake returns when the pairing token is absent or
/// invalid. The body is deliberately generic — it does not distinguish
/// "missing" from "wrong" so it leaks nothing about the token namespace.
fn unauthorized_response() -> ErrorResponse {
    use tokio_tungstenite::tungstenite::http::StatusCode;
    let mut resp = ErrorResponse::new(Some("missing or invalid pairing token".to_owned()));
    *resp.status_mut() = StatusCode::UNAUTHORIZED;
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::sync::Arc;

    use tokio::net::TcpStream;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    const TEST_TOKEN: [u8; crate::auth::TOKEN_LEN] = [0x11; crate::auth::TOKEN_LEN];

    /// A token-gated listener bound to an ephemeral loopback port, with one
    /// known token. TLS is off so the test exercises the token handshake and
    /// frame path without the TLS machinery (covered in `tls`'s own tests).
    async fn token_listener() -> (WsListener, SocketAddr, String) {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let token_hex = hex::encode(TEST_TOKEN);
        writeln!(file, "{token_hex}").unwrap();
        let store = crate::auth::TokenStore::load(file.path()).unwrap();

        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let listener = WsListener {
            tcp,
            tls: None,
            tokens: Some(Arc::new(store)),
        };
        (listener, addr, token_hex)
    }

    /// A `ws://` client upgrade request carrying `Authorization: Bearer <hex>`.
    fn bearer_request(addr: SocketAddr, token_hex: &str) -> Request {
        let mut req = format!("ws://{addr}/").into_client_request().unwrap();
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {token_hex}").parse().unwrap(),
        );
        req
    }

    #[tokio::test]
    async fn valid_token_upgrades_and_round_trips_a_frame() {
        let (listener, addr, token_hex) = token_listener().await;

        // One complete framed message: 4-byte length prefix (body = 3) + body.
        let frame: Vec<u8> = vec![0, 0, 0, 3, 0xde, 0xad, 0xbe];

        let server = async {
            let (mut reader, _writer, peer) = listener.accept().await.unwrap();
            let got = reader.read_frame().await.unwrap();
            (got, peer)
        };
        let client = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let (mut ws, _resp) =
                tokio_tungstenite::client_async(bearer_request(addr, &token_hex), tcp)
                    .await
                    .expect("valid token must upgrade");
            ws.send(Message::Binary(frame.clone())).await.unwrap();
            // Hold the connection open until the server has read.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };

        let ((got, peer), ()) = tokio::join!(server, client);
        assert_eq!(got.unwrap().as_ref(), frame.as_slice(), "frame round-trips");
        assert_eq!(peer.transport, TransportType::WebSocket);
        assert!(
            peer.mcp_host_key.is_some(),
            "an authenticated remote peer is non-anonymous"
        );
    }

    #[tokio::test]
    async fn invalid_token_is_refused_at_the_handshake() {
        let (listener, addr, _token_hex) = token_listener().await;
        let wrong = hex::encode([0x22u8; crate::auth::TOKEN_LEN]);

        let server = async { listener.accept().await };
        let client = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            tokio_tungstenite::client_async(bearer_request(addr, &wrong), tcp).await
        };

        let (server_res, client_res) = tokio::join!(server, client);
        assert!(server_res.is_err(), "server rejects the unknown token");
        assert!(client_res.is_err(), "client upgrade fails with HTTP 401");
    }

    #[tokio::test]
    async fn missing_authorization_header_is_refused() {
        let (listener, addr, _token_hex) = token_listener().await;

        let server = async { listener.accept().await };
        let client = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let req = format!("ws://{addr}/").into_client_request().unwrap();
            tokio_tungstenite::client_async(req, tcp).await
        };

        let (server_res, client_res) = tokio::join!(server, client);
        assert!(server_res.is_err());
        assert!(client_res.is_err());
    }

    #[test]
    fn uds_credential_failure_is_never_root_fallback() {
        let error = peer_identity_from_credentials(Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "simulated peer credential failure",
        )))
        .expect_err("missing authenticated credentials must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}
