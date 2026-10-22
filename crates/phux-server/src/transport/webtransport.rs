//! WebTransport transport: HTTP/3 over QUIC for browsers.
//!
//! Browsers cannot open raw QUIC. The consumer opens
//! one bidirectional stream per session and the identical length-prefixed
//! phux frames (`docs/protocol.md#framing`) flow over it.
//!
//! Routable consumers authenticate with the pairing token (ADR-0031) on the
//! `CONNECT` request, inside TLS: an `Authorization: Bearer` header (native)
//! or a `token=` query parameter (browsers cannot set headers). A refused
//! session gets HTTP 403 before it exists. Duplicate `Authorization` fields
//! are refused on the raw QPACK field list, before a header map can collapse
//! them. The listener shares the certificate and token store with `wss://`
//! and QUIC, but binds its own UDP socket because browsers offer only the
//! `h3` ALPN.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use phux_dial::window::{SendWindow, TrackedSend};
use phux_protocol::policy::{PeerIdentity, TransportType};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt};
use tracing::{debug, warn};

use super::quic::QuicBindError as WtBindError;
use super::{FrameReader, FrameWriter, Incoming};

mod admission;
mod connect_headers;
mod h3;

use admission::admit_connect;

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
    async fn write_frames(&mut self, batch: &mut BytesMut, _ends: &[usize]) -> io::Result<()> {
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

#[cfg(test)]
mod tests;
