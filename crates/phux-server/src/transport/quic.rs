//! QUIC transport ([ADR-0007]): the identical length-prefixed phux frames
//! (`docs/spec/proto.md` §5) over one bidirectional QUIC stream per
//! connection, plus the per-Terminal stream multiplexer (§4.2).
//!
//! TLS 1.3 is intrinsic to QUIC. Routable consumers authenticate with a
//! bearer-token **preamble** (`len: u32 BE` + token) as the first bytes of
//! the stream, validated inside `Incoming::accept` before any frame is read:
//! the QUIC analogue of the WebSocket `Authorization` header (ADR-0031).
//!
//! [ADR-0007]: ../../../docs/adr/0007-mosh-class-transport-and-satellites.md

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::BytesMut;
use phux_dial::window::{SendWindow, TrackedSend};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::framing;
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use super::{FrameReader, FrameWriter, Incoming, LENGTH_PREFIX};

/// Upper bound on the token preamble body: room for a longer future token,
/// never a large allocation.
const MAX_TOKEN_PREAMBLE: usize = 256;

/// Idle timeout for a connection with no traffic and no keep-alive.
pub(super) const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep-alive interval, well under [`IDLE_TIMEOUT`], so a quiet attached
/// client survives NATs.
pub(super) const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// QUIC application close code for a connection refused at admission.
const AUTH_FAILED_CODE: u32 = 0x01;

/// Cap on client-opened bidi streams per connection (proto.md §4.2): far
/// above panes-per-attach, far below task exhaustion. quinn enforces it.
const MAX_CONCURRENT_BIDI_STREAMS: u64 = 128;

/// Bound on each admission step (handshake, first stream, preamble), so a
/// stalled peer frees its admission slot.
const ADMISSION_DEADLINE: Duration = super::HANDSHAKE_DEADLINE;

type QuicAccepted = (QuicMuxReader, QuicWriter, crate::auth::ConnectionIdentity);

/// A QUIC listener: a quinn endpoint bound to a UDP socket.
pub(crate) struct QuicListener {
    endpoint: quinn::Endpoint,
    admission: QuicAdmission,
    workload_registry: Option<Arc<crate::workload::ReloadingWorkloadRegistry>>,
    /// Where the enrollment ALPN issues into, when this listener offers it
    /// (`workload-auth.md` §8.2): the configured listener only.
    enrollment: Option<Arc<crate::workload::WorkloadPaths>>,
    /// Where satellites that dial in under the satellite ALPN go (ADR-0182):
    /// the configured listener only.
    satellites: Option<crate::hub::adopt::SatelliteAdopter>,
    admissions: super::Admissions<QuicAccepted>,
    /// Refused preambles, warned about at a bounded rate.
    refusals: Arc<super::RefusalWarnings>,
}

/// Whom a [`QuicListener`] admits.
#[derive(Clone)]
pub(crate) enum QuicAdmission {
    /// Anyone who completes the TLS handshake; no preamble (loopback/dev).
    Open,
    /// A bearer token from the pairing store (ADR-0031).
    Store(Arc<crate::auth::ReloadingTokenStore>),
    /// Only the token this listener was opened with (`OPEN_LISTENER`,
    /// ADR-0120); pairing-store tokens are refused.
    Listener(Arc<crate::auth::ListenerToken>),
}

impl QuicListener {
    /// Bind a listener that requires a pairing-store token when `tokens` is
    /// `Some`, and, with `client_ca`, a client certificate that maps to an
    /// active credential in `workload_registry`. With `enrollment` it also
    /// offers the enrollment ALPN, issuing from that authority.
    pub(crate) fn from_pem_with_client_ca_and_registry(
        addr: SocketAddr,
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
        tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
        client_ca: Option<&rustls::pki_types::CertificateDer<'static>>,
        workload_registry: Option<Arc<crate::workload::ReloadingWorkloadRegistry>>,
        enrollment: Option<crate::workload::WorkloadPaths>,
    ) -> Result<Self, QuicBindError> {
        let tls = super::tls::quic_server_config_with_client_ca(
            cert_path,
            key_path,
            client_ca,
            enrollment.is_some(),
        )?;
        Ok(Self {
            endpoint: server_endpoint(addr, tls, Some(MAX_CONCURRENT_BIDI_STREAMS))?,
            admission: tokens.map_or(QuicAdmission::Open, QuicAdmission::Store),
            workload_registry,
            enrollment: enrollment.map(Arc::new),
            satellites: None,
            admissions: super::Admissions::new(),
            refusals: Arc::new(super::RefusalWarnings::new()),
        })
    }

    /// Bind a listener that admits whoever `admission` names, with the same
    /// workload-CA verification as the configured listener when `workload`
    /// is given.
    pub(crate) fn with_admission(
        addr: SocketAddr,
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
        admission: QuicAdmission,
        workload: Option<(
            &rustls::pki_types::CertificateDer<'static>,
            Arc<crate::workload::ReloadingWorkloadRegistry>,
        )>,
    ) -> Result<Self, QuicBindError> {
        let (client_ca, registry) = workload.unzip();
        let mut listener = Self::from_pem_with_client_ca_and_registry(
            addr, cert_path, key_path, None, client_ca, registry, None,
        )?;
        listener.admission = admission;
        Ok(listener)
    }

    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Adopt satellites that dial in under the satellite ALPN into `adopter`
    /// (ADR-0182). The ALPN is offered only when this listener verifies
    /// workload certificates; without this call it is refused.
    #[must_use]
    pub(crate) fn with_satellites(mut self, adopter: crate::hub::adopt::SatelliteAdopter) -> Self {
        self.satellites = Some(adopter);
        self
    }
}

/// Errors from constructing a QUIC-based listener.
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

/// A quinn server endpoint with phux's idle/keep-alive policy and, when
/// given, a cap on client-opened bidi streams.
pub(super) fn server_endpoint(
    addr: SocketAddr,
    tls: rustls::ServerConfig,
    max_bidi_streams: Option<u64>,
) -> Result<quinn::Endpoint, QuicBindError> {
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    if let Ok(idle) = IDLE_TIMEOUT.try_into() {
        transport.max_idle_timeout(Some(idle));
    }
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    if let Some(cap) = max_bidi_streams.and_then(|cap| quinn::VarInt::from_u64(cap).ok()) {
        transport.max_concurrent_bidi_streams(cap);
    }
    server_config.transport_config(Arc::new(transport));
    Ok(quinn::Endpoint::server(server_config, addr)?)
}

/// The ALPN the handshake settled on.
fn negotiated_alpn(conn: &quinn::Connection) -> Option<Vec<u8>> {
    conn.handshake_data()?
        .downcast::<quinn::crypto::rustls::HandshakeData>()
        .ok()?
        .protocol
}

/// The registry credential for a connection's client certificate, looked up
/// in the registry generation current right now (rustls already verified
/// the chain), so enrollment and revocation apply to the next connection.
fn workload_credential(
    conn: &quinn::Connection,
    registry: &crate::workload::ReloadingWorkloadRegistry,
) -> Option<crate::auth::AuthenticatedCredential> {
    let identity = conn.peer_identity()?;
    let certs = identity.downcast_ref::<Vec<rustls::pki_types::CertificateDer<'static>>>()?;
    registry.lookup_certificate(certs.first()?.as_ref())
}

/// QUIC read half: length-prefixed frames off one receive stream.
pub(crate) struct QuicReader {
    recv: quinn::RecvStream,
    frames: super::FrameAssembler,
}

impl QuicReader {
    /// Wrap one already-authenticated QUIC receive stream in phux framing.
    pub(crate) fn from_stream(recv: quinn::RecvStream) -> Self {
        Self {
            recv,
            frames: super::FrameAssembler::default(),
        }
    }
}

impl FrameReader for QuicReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        self.frames.read_frame(&mut self.recv).await
    }
}

/// QUIC write half. Writes go through [`TrackedSend`], which holds quinn's
/// send window near the congestion window so a slow path backs up into the
/// mailbox and the output pump resyncs instead of replaying a backlog.
pub(crate) struct QuicWriter {
    send: TrackedSend<quinn::SendStream>,
    finished: bool,
    diagnostics: crate::stream_diagnostics::StreamRegistration,
}

impl QuicWriter {
    /// Wrap one already-authenticated send stream. `window` belongs to the
    /// connection: build it once and clone it per stream.
    pub(crate) fn from_stream(send: quinn::SendStream, window: SendWindow) -> Self {
        Self::with_lane(send, window, crate::stream_diagnostics::StreamLane::Control)
    }

    pub(crate) fn from_terminal_stream(send: quinn::SendStream, window: SendWindow) -> Self {
        Self::with_lane(
            send,
            window,
            crate::stream_diagnostics::StreamLane::Terminal,
        )
    }

    fn with_lane(
        send: quinn::SendStream,
        window: SendWindow,
        lane: crate::stream_diagnostics::StreamLane,
    ) -> Self {
        use std::hash::BuildHasher;
        // Quinn's stable id is address-derived; hash it with a process-random
        // key so snapshots correlate streams without exposing an address.
        static IDS: std::sync::OnceLock<std::collections::hash_map::RandomState> =
            std::sync::OnceLock::new();
        let connection_id = IDS
            .get_or_init(std::collections::hash_map::RandomState::new)
            .hash_one(window.connection().stable_id());
        let diagnostics =
            crate::stream_diagnostics::register(crate::stream_diagnostics::StreamContext {
                connection_id,
                stream_id: u64::from(send.id()),
                lane,
            });
        diagnostics.tracker().set_active(true);
        Self {
            send: TrackedSend::new(send, window),
            finished: false,
            diagnostics,
        }
    }

    pub(crate) fn diagnostic_tracker(&self) -> crate::stream_diagnostics::StreamTracker {
        self.diagnostics.tracker()
    }
}

impl FrameWriter for QuicWriter {
    fn stream_tracker(&self) -> Option<crate::stream_diagnostics::StreamTracker> {
        Some(self.diagnostic_tracker())
    }

    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        let _write = self.diagnostics.tracker().begin_write();
        self.send.write_all(frame).await
    }

    /// One `write_all` for the whole batch: a byte stream whose frames carry
    /// their own length prefix, so the bytes are identical and a burst costs
    /// one write instead of one per frame.
    async fn write_frames(&mut self, batch: &mut BytesMut, _ends: &[usize]) -> io::Result<()> {
        let _write = self.diagnostics.tracker().begin_write();
        self.send.write_all(batch).await
    }

    #[allow(
        clippy::unused_async_trait_impl,
        reason = "FrameWriter requires an async close operation, while Quinn's finish is synchronous"
    )]
    async fn close(&mut self) -> io::Result<()> {
        self.send.get_mut().finish().map_err(io::Error::other)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for QuicWriter {
    fn drop(&mut self) {
        if !self.finished {
            // Cancellation may interrupt a length-prefixed frame. An explicit
            // reset distinguishes that abandoned write from an orderly FIN.
            let _ = self.send.get_mut().reset(0x10_u32.into());
        }
    }
}

impl Incoming for QuicListener {
    type Reader = QuicMuxReader;
    type Writer = QuicWriter;

    fn transport_type(&self) -> TransportType {
        TransportType::Quic
    }

    fn supports_quic_streams(&self) -> bool {
        true
    }

    /// One endpoint multiplexes many connections, so a failed or refused
    /// connection is logged and skipped; only endpoint closure ends the loop.
    async fn accept(&self) -> io::Result<QuicAccepted> {
        self.admissions
            .next(|| async {
                let incoming = self.endpoint.accept().await.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotConnected, "quic endpoint closed")
                })?;
                let admission = admit_connection(
                    incoming,
                    self.admission.clone(),
                    self.workload_registry.clone(),
                    self.enrollment.clone(),
                    Arc::clone(&self.refusals),
                    self.satellites
                        .clone()
                        .map(|adopter| (adopter, self.endpoint.clone())),
                );
                Ok(Box::pin(async move { admission.await.map(Ok) })
                    as super::Admission<QuicAccepted>)
            })
            .await
    }

    fn max_connections(&self) -> Option<usize> {
        matches!(self.admission, QuicAdmission::Open).then_some(super::ANONYMOUS_MAX_CONNECTIONS)
    }

    fn kind(&self) -> &'static str {
        "quic"
    }
}

pub mod admission;
pub mod mux;
pub mod streams;

use admission::admit_connection;
pub(crate) use mux::*;
pub(crate) use streams::*;

mod enroll;

#[cfg(test)]
mod enroll_adversarial;

#[cfg(test)]
mod workload_adversarial;

#[cfg(test)]
mod authority_pin;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "tests hold stream events and permits to the end of each scripted step"
)]
mod tests;
