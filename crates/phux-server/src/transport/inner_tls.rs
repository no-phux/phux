//! The end-to-end TLS session a consumer runs inside a relayed stream
//! (ADR-0154 item 5, `docs/spec/workload-auth.md` §3).
//!
//! A relay terminates the consumer's QUIC TLS to route it, so the consumer's
//! certificate never reached this server. A consumer that pins this server's
//! CA instead runs TLS 1.3 inside the spliced stream: this server presents its
//! own certificate and verifies the consumer's against the workload CA, end to
//! end, and the relay forwards ciphertext. The first byte tells the two
//! shapes apart: a TLS record starts `0x16`, a bearer preamble's big-endian
//! length starts `0x00`.
//!
//! The inner session offers the terminal ALPN and the enrollment ALPN, so a
//! device behind a relay enrolls the same way (§8.2).

use std::io;
use std::sync::Arc;

use phux_protocol::enroll::{MAX_CSR, MAX_TICKET, Reply, Request};
use phux_protocol::policy::{ENROLL_ALPN, QUIC_ALPN};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::{debug, info};

use crate::workload::{ReloadingWorkloadRegistry, WorkloadPaths};

/// The first byte of a TLS handshake record.
pub const TLS_HANDSHAKE: u8 = 0x16;

/// A byte stream the inner session runs over and yields.
pub trait InnerIo: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> InnerIo for T {}

/// What this server accepts inside a relayed stream.
pub struct InnerTls {
    acceptor: tokio_rustls::TlsAcceptor,
    /// Under `paired`: the registry every inner certificate must be active
    /// in. `None` admits an inner session with or without a certificate,
    /// as the bearer alone admits a plain one.
    workload: Option<Arc<ReloadingWorkloadRegistry>>,
    /// Where an inner enrollment issues into.
    enrollment: WorkloadPaths,
}

impl std::fmt::Debug for InnerTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InnerTls")
            .field("paired", &self.workload.is_some())
            .finish_non_exhaustive()
    }
}

/// One inner session's outcome.
pub enum InnerAccepted {
    /// A terminal session: the stream, and the workload credential its
    /// certificate maps to (always `Some` under `paired`).
    Terminal {
        /// The decrypted stream, its handshake done.
        stream: Box<dyn InnerIo>,
        /// The registry credential its certificate maps to.
        workload: Option<crate::auth::AuthenticatedCredential>,
    },
    /// An enrollment was answered; nothing else rides this stream.
    Enrolled,
    /// Refused before any phux byte.
    Refused,
}

impl std::fmt::Debug for InnerAccepted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Terminal { .. } => "InnerAccepted::Terminal",
            Self::Enrolled => "InnerAccepted::Enrolled",
            Self::Refused => "InnerAccepted::Refused",
        })
    }
}

impl InnerTls {
    /// The inner acceptor for the server's own certificate at `cert`/`key`.
    /// With `workload`, a client certificate is required and verified against
    /// its CA; otherwise one is not asked for.
    ///
    /// # Errors
    ///
    /// The certificate, key, or CA cannot build a TLS config.
    pub fn new(
        cert: &std::path::Path,
        key: &std::path::Path,
        workload: Option<(
            &rustls::pki_types::CertificateDer<'static>,
            Arc<ReloadingWorkloadRegistry>,
        )>,
        enrollment: WorkloadPaths,
    ) -> Result<Self, super::tls::TlsError> {
        let (ca, registry) = workload.unzip();
        let config = super::tls::inner_server_config(cert, key, ca)?;
        Ok(Self {
            acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(config)),
            workload: registry,
            enrollment,
        })
    }

    /// Whether a plain (pre-ADR-0154) bridged consumer may still be admitted
    /// by its bearer: only outside `paired`.
    #[must_use]
    pub const fn admits_plain(&self) -> bool {
        self.workload.is_none()
    }

    /// Run the inner handshake over `io` and settle what it is for.
    pub async fn accept<S>(&self, io: S) -> InnerAccepted
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let stream = match self.acceptor.accept(io).await {
            Ok(stream) => stream,
            Err(error) => {
                debug!(%error, "inner TLS handshake failed");
                return InnerAccepted::Refused;
            }
        };
        let (_, session) = stream.get_ref();
        let alpn = session.alpn_protocol().map(<[u8]>::to_vec);
        let leaf = session
            .peer_certificates()
            .and_then(|certs| certs.first())
            .map(|leaf| leaf.as_ref().to_vec());
        if alpn.as_deref() == Some(ENROLL_ALPN) {
            serve_enrollment(stream, self.enrollment.clone()).await;
            return InnerAccepted::Enrolled;
        }
        if alpn.as_deref() != Some(QUIC_ALPN) {
            return InnerAccepted::Refused;
        }
        let workload = match &self.workload {
            Some(registry) => {
                let Some(credential) = leaf.and_then(|leaf| registry.lookup_certificate(&leaf))
                else {
                    debug!("inner TLS client identity refused");
                    return InnerAccepted::Refused;
                };
                Some(credential)
            }
            None => None,
        };
        InnerAccepted::Terminal {
            stream: Box::new(stream),
            workload,
        }
    }
}

/// One enrollment exchange over an inner session: the request is
/// self-delimiting, so no half-close is needed.
async fn serve_enrollment<S: AsyncRead + AsyncWrite + Unpin + Send>(
    mut stream: S,
    paths: WorkloadPaths,
) {
    let deadline = super::HANDSHAKE_DEADLINE;
    let Ok(Ok(request)) = tokio::time::timeout(deadline, read_request(&mut stream)).await else {
        debug!("inner enrollment request missing, malformed, or late");
        let _ = stream.write_all(&Reply::Refused.encode()).await;
        let _ = stream.flush().await;
        return;
    };
    let joined = tokio::task::spawn_blocking(move || {
        crate::workload::tickets::enroll_with_ticket(&paths, &request.ticket, &request.csr)
    })
    .await;
    let reply = match joined {
        Ok(Ok(enrolled)) => {
            info!(
                ticket = %enrolled.ticket,
                credential = %enrolled.credential,
                "workload credential enrolled with a ticket through a relay; `phux workload revoke` it if this was not your device"
            );
            Reply::Issued(enrolled.chain_pem)
        }
        Ok(Err(error)) => {
            info!(%error, "enrollment refused");
            Reply::Refused
        }
        Err(error) => {
            debug!(%error, "enrollment task failed");
            Reply::Refused
        }
    };
    // The reply is self-delimiting; the client closes once it has read it.
    // Finishing first would end a relay's splice, and with it the client's
    // connection, before the reply crossed.
    let _ = tokio::time::timeout(deadline, async {
        stream.write_all(&reply.encode()).await?;
        stream.flush().await?;
        let mut rest = [0_u8; 1];
        stream.read(&mut rest).await
    })
    .await;
}

/// Read one request field by field, bounded as the decoder bounds it.
async fn read_request<R: AsyncRead + Unpin + Send>(reader: &mut R) -> io::Result<Request> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "malformed enrollment request");
    let mut head = [0_u8; 2];
    reader.read_exact(&mut head).await?;
    let ticket_len = usize::from(head[1]);
    if head[0] != phux_protocol::enroll::VERSION || ticket_len == 0 || ticket_len > MAX_TICKET {
        return Err(invalid());
    }
    let mut ticket = vec![0_u8; ticket_len];
    reader.read_exact(&mut ticket).await?;
    let mut len = [0_u8; 4];
    reader.read_exact(&mut len).await?;
    let csr_len = usize::try_from(u32::from_be_bytes(len)).map_err(|_| invalid())?;
    if csr_len == 0 || csr_len > MAX_CSR {
        return Err(invalid());
    }
    let mut csr = vec![0_u8; csr_len];
    reader.read_exact(&mut csr).await?;
    Ok(Request { ticket, csr })
}
