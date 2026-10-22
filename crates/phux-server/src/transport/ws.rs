//! The WebSocket listener and its frame reader and writer.
#![allow(
    clippy::redundant_pub_crate,
    reason = "private transport module; the accept loop uses these through the transport re-export"
)]

use std::io;
use std::net::SocketAddr;

use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt};
use phux_protocol::policy::TransportType;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::error::CapacityError;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

use phux_protocol::wire::framing::{self, LENGTH_PREFIX_LEN as LENGTH_PREFIX};

use super::{
    ANONYMOUS_MAX_CONNECTIONS, AcceptErrorDisposition, Admission, Admissions, AllowedOrigins,
    FrameReader, FrameWriter, Incoming, RefusalWarnings, WsPeerRejection, ws_admit,
};

// ── WebSocket ────────────────────────────────────────────────────────────────

/// The byte stream under a WebSocket: plaintext TCP (loopback only) or TLS
/// (`wss://`, ADR-0031), boxed because `TlsStream` is large.
pub(super) type ServerStream =
    tokio_util::either::Either<TcpStream, Box<tokio_rustls::server::TlsStream<TcpStream>>>;

pub(super) type Ws = WebSocketStream<ServerStream>;

/// WebSocket listener: TCP + RFC 6455 upgrade, one binary message per frame.
/// With `tls`, each connection is TLS-wrapped first; with `tokens`, the
/// upgrade must carry a valid pairing token or is refused with HTTP 401.
pub(crate) struct WsListener {
    tcp: TcpListener,
    tls: Option<tokio_rustls::TlsAcceptor>,
    tokens: Option<std::sync::Arc<crate::auth::ReloadingTokenStore>>,
    /// Present only with a workload CA; every admission maps the client
    /// certificate through it, as QUIC does.
    workload: Option<std::sync::Arc<crate::workload::ReloadingWorkloadRegistry>>,
    rejection_warnings: RefusalWarnings,
    admissions: Admissions<WsAccepted>,
    /// Browser origins the anonymous listener admits; unused with tokens.
    origins: std::sync::Arc<AllowedOrigins>,
    /// Live-connection cap: [`ANONYMOUS_MAX_CONNECTIONS`] for the anonymous
    /// listener, none with tokens.
    connection_cap: Option<usize>,
}

pub(super) type WsAccepted = (WsReader, WsWriter, crate::auth::ConnectionIdentity);

impl WsListener {
    fn from_parts(
        tcp: TcpListener,
        tls: Option<tokio_rustls::TlsAcceptor>,
        tokens: Option<std::sync::Arc<crate::auth::ReloadingTokenStore>>,
        workload: Option<std::sync::Arc<crate::workload::ReloadingWorkloadRegistry>>,
    ) -> Self {
        Self {
            tcp,
            tls,
            tokens,
            workload,
            rejection_warnings: RefusalWarnings::new(),
            admissions: Admissions::new(),
            origins: std::sync::Arc::new(AllowedOrigins::default()),
            connection_cap: None,
        }
    }

    /// This listener with its live-connection cap at `cap` (tests).
    #[cfg(test)]
    pub(crate) const fn with_connection_cap(mut self, cap: usize) -> Self {
        self.connection_cap = Some(cap);
        self
    }

    /// A plaintext loopback listener that still demands a pairing token, for
    /// tests that drive the bearer path without TLS.
    #[cfg(test)]
    pub(crate) async fn loopback_with_tokens(
        tokens: std::sync::Arc<crate::auth::ReloadingTokenStore>,
    ) -> io::Result<Self> {
        let tcp = TcpListener::bind("127.0.0.1:0").await?;
        Ok(Self::from_parts(tcp, None, Some(tokens), None))
    }

    /// Bind a plaintext, unauthenticated listener (loopback browser client)
    /// that admits browser pages only from `origins`.
    pub(crate) async fn bind(addr: SocketAddr, origins: AllowedOrigins) -> io::Result<Self> {
        let mut listener = Self::from_parts(TcpListener::bind(addr).await?, None, None, None);
        listener.origins = std::sync::Arc::new(origins);
        listener.connection_cap = Some(ANONYMOUS_MAX_CONNECTIONS);
        Ok(listener)
    }

    /// Bind a TLS-terminated, token-authenticated listener. The only
    /// production way to attach a token store, so a token never crosses
    /// plaintext (ADR-0031). `workload` is `Some` exactly when `tls` verifies
    /// a workload CA (ADR-0116); the token stays outer admission.
    pub(crate) async fn bind_secure(
        addr: SocketAddr,
        tls: tokio_rustls::TlsAcceptor,
        tokens: std::sync::Arc<crate::auth::ReloadingTokenStore>,
        workload: Option<std::sync::Arc<crate::workload::ReloadingWorkloadRegistry>>,
    ) -> io::Result<Self> {
        Ok(Self::from_parts(
            TcpListener::bind(addr).await?,
            Some(tls),
            Some(tokens),
            workload,
        ))
    }

    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.local_addr()
    }
}

/// WebSocket read half: each binary message is one complete encoded frame.
pub(crate) struct WsReader {
    pub(super) rx: futures_util::stream::SplitStream<Ws>,
}

impl FrameReader for WsReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        loop {
            match self.rx.next().await {
                None | Some(Ok(Message::Close(_))) => return Ok(None),
                Some(Ok(Message::Binary(data))) => {
                    framing::check_frame(&data)?;
                    return Ok(Some(BytesMut::from(&data[..])));
                }
                // Over the size cap: the same §5 violation as an oversized
                // length header, so the peer gets FRAME_TOO_LARGE.
                Some(Err(WebSocketError::Capacity(CapacityError::MessageTooLong {
                    size, ..
                }))) => {
                    let length =
                        u32::try_from(size.saturating_sub(LENGTH_PREFIX)).unwrap_or(u32::MAX);
                    return Err(framing::FramingError::LengthOutOfRange { length }.into());
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
    pub(super) tx: futures_util::stream::SplitSink<Ws, Message>,
}

impl WsWriter {
    async fn feed(&mut self, frame: Bytes) -> io::Result<()> {
        self.tx
            .feed(Message::Binary(frame))
            .await
            .map_err(io::Error::other)
    }
}

impl FrameWriter for WsWriter {
    /// Queue one frame as one binary message without flushing; the writer
    /// task flushes once per mailbox drain, so a burst leaves as full TCP
    /// segments instead of one partial segment per frame.
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.feed(Bytes::copy_from_slice(frame)).await
    }

    /// One message per frame, each a zero-copy slice of the batch. The
    /// batch's allocation comes back to it once the sink has written them.
    async fn write_frames(&mut self, batch: &mut BytesMut, ends: &[usize]) -> io::Result<()> {
        let mut frames = batch.split().freeze();
        let mut start = 0;
        for &end in ends {
            self.feed(frames.split_to(end - start)).await?;
            start = end;
        }
        Ok(())
    }

    async fn flush(&mut self) -> io::Result<()> {
        SinkExt::flush(&mut self.tx).await.map_err(io::Error::other)
    }

    async fn close(&mut self) -> io::Result<()> {
        self.tx.close().await.map_err(io::Error::other)
    }
}

impl Incoming for WsListener {
    type Reader = WsReader;
    type Writer = WsWriter;

    fn transport_type(&self) -> TransportType {
        TransportType::WebSocket
    }

    async fn accept(&self) -> io::Result<WsAccepted> {
        self.admissions
            .next(|| async {
                let (tcp, peer) = self.tcp.accept().await?;
                let admission = ws_admit(
                    tcp,
                    peer,
                    self.tls.clone(),
                    self.tokens.clone(),
                    self.workload.clone(),
                    std::sync::Arc::clone(&self.origins),
                );
                Ok(Box::pin(async move { Some(admission.await) }) as Admission<WsAccepted>)
            })
            .await
    }

    fn accept_error_disposition(&self, error: &io::Error) -> AcceptErrorDisposition {
        let Some(rejection) = error
            .get_ref()
            .and_then(|source| source.downcast_ref::<WsPeerRejection>())
        else {
            return AcceptErrorDisposition::Default;
        };

        AcceptErrorDisposition::PeerRejected {
            stage: rejection.stage.as_str(),
            source_ip: rejection.source_ip,
            warn_suppressed: self.rejection_warnings.due(),
        }
    }

    fn max_connections(&self) -> Option<usize> {
        self.connection_cap
    }

    fn kind(&self) -> &'static str {
        "ws"
    }
}
