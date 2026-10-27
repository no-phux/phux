//! QUIC connection admission: the handshake, the first stream, the bearer preamble, and the workload certificate.

use super::{
    ADMISSION_DEADLINE, AUTH_FAILED_CODE, Arc, LENGTH_PREFIX, MAX_TOKEN_PREAMBLE, PeerIdentity,
    QuicAccepted, QuicAdmission, QuicMuxReader, QuicWriter, SendWindow, TransportType, debug,
    enroll, negotiated_alpn, read_exact_quic, warn, workload_credential,
};

/// Handshake, first stream, preamble, then workload certificate, each
/// bounded by [`ADMISSION_DEADLINE`]. `None` means refused or failed.
pub(super) async fn admit_connection(
    incoming: quinn::Incoming,
    admission: QuicAdmission,
    workload_registry: Option<Arc<crate::workload::ReloadingWorkloadRegistry>>,
    enrollment: Option<Arc<crate::workload::WorkloadPaths>>,
    refusals: Arc<super::super::RefusalWarnings>,
    satellites: Option<(crate::hub::adopt::SatelliteAdopter, quinn::Endpoint)>,
) -> Option<QuicAccepted> {
    let remote = incoming.remote_address();
    let conn = match tokio::time::timeout(ADMISSION_DEADLINE, incoming).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(err)) => {
            debug!(%remote, error = %err, "quic handshake failed");
            return None;
        }
        Err(_) => {
            debug!(%remote, "quic handshake timed out");
            return None;
        }
    };
    let conn = serve_other_alpn(conn, enrollment, workload_registry.as_ref(), satellites).await?;

    // One refusal for every workload failure; the peer learns only that it
    // was refused (`docs/remote.md#managing-credentials`). The handshake asked for, but did
    // not require, a certificate (ADR-0154), so this is the check that
    // refuses a certificate-less or unenrolled peer, before any stream is
    // read: no bearer preamble and no phux byte.
    let workload_credential = match &workload_registry {
        Some(registry) => {
            let Some(credential) = workload_credential(&conn, registry) else {
                debug!(%remote, "quic mTLS client identity refused");
                conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
                return None;
            };
            Some(credential)
        }
        None => None,
    };

    let (send, mut recv) = match tokio::time::timeout(ADMISSION_DEADLINE, conn.accept_bi()).await {
        Ok(Ok(pair)) => pair,
        Ok(Err(err)) => {
            debug!(%remote, error = %err, "quic stream accept failed");
            return None;
        }
        Err(_) => {
            debug!(%remote, "quic stream accept timed out");
            conn.close(AUTH_FAILED_CODE.into(), b"stream timeout");
            return None;
        }
    };

    let credential = match &admission {
        QuicAdmission::Open => None,
        admission => {
            let preamble =
                tokio::time::timeout(ADMISSION_DEADLINE, admit(&mut recv, admission)).await;
            let Ok(Some(credential)) = preamble else {
                if preamble.is_err() {
                    debug!(%remote, "quic auth preamble timed out");
                } else if let Some(suppressed) = refusals.due() {
                    warn!(
                        %remote,
                        suppressed,
                        "quic consumer refused: missing or invalid token"
                    );
                } else {
                    debug!(%remote, "quic consumer refused: missing or invalid token");
                }
                conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
                return None;
            };
            Some(credential)
        }
    };

    let bearer = bearer_admission(&admission, credential.as_ref());
    let credential = workload_credential.or(credential);
    let peer = PeerIdentity {
        uid: 0,
        pid: None,
        exe_path: None,
        mcp_host_key: credential.as_ref().map(|credential| credential.id.clone()),
        transport: TransportType::Quic,
        source_addr: Some(remote.ip()),
    };

    let window = SendWindow::new(conn.clone());
    let writer = QuicWriter::from_stream(send, window.clone());
    let mut reader = QuicMuxReader::new(recv, conn, window);
    reader.diagnostics = Some(writer.diagnostic_tracker());
    Some((
        reader,
        writer,
        crate::auth::ConnectionIdentity {
            peer,
            credential,
            ssh_origin: None,
            bearer,
        },
    ))
}

/// Hand a connection that negotiated anything but the terminal ALPN to what
/// serves it, and return only a terminal connection.
async fn serve_other_alpn(
    conn: quinn::Connection,
    enrollment: Option<Arc<crate::workload::WorkloadPaths>>,
    workload_registry: Option<&Arc<crate::workload::ReloadingWorkloadRegistry>>,
    satellites: Option<(crate::hub::adopt::SatelliteAdopter, quinn::Endpoint)>,
) -> Option<quinn::Connection> {
    use phux_protocol::policy::{COORD_ALPN, ENROLL_ALPN, SATELLITE_ALPN};
    let alpn = negotiated_alpn(&conn);
    match (alpn.as_deref(), enrollment, satellites) {
        // Never a phux connection: one exchange, then closed.
        (Some(ENROLL_ALPN), Some(paths), _) => enroll::serve(&conn, &paths).await,
        // Never a consumer: a satellite's mail and claims, answered by a hub.
        (Some(COORD_ALPN), _, Some((adopter, _))) => {
            crate::hub::adopt::coord::admit(conn, workload_registry, adopter.work_store());
        }
        // Never a consumer: the hub's link supervisor adopts it, or it is
        // closed before any stream (ADR-0182).
        (Some(SATELLITE_ALPN), _, Some((adopter, endpoint))) => {
            crate::hub::adopt::admit(conn, endpoint, workload_registry, &adopter);
        }
        // Each of those, on a server that does not serve it.
        (Some(ENROLL_ALPN | COORD_ALPN | SATELLITE_ALPN), _, _) => {
            conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
        }
        _ => return Some(conn),
    }
    None
}

/// Read the token preamble and verify it against the pairing store.
pub(crate) async fn authorize_preamble(
    recv: &mut quinn::RecvStream,
    store: &crate::auth::ReloadingTokenStore,
) -> Option<crate::auth::AuthenticatedCredential> {
    let (_, token) = read_length_prefixed(recv, MAX_TOKEN_PREAMBLE).await?;
    store.authenticate_and_touch(&token)
}

/// Read the token preamble and verify it against whoever `admission` names.
pub(super) async fn admit(
    recv: &mut quinn::RecvStream,
    admission: &QuicAdmission,
) -> Option<crate::auth::AuthenticatedCredential> {
    match admission {
        QuicAdmission::Open => None,
        QuicAdmission::Store(store) => authorize_preamble(recv, store).await,
        QuicAdmission::Listener(token) => {
            let (_, preamble) = read_length_prefixed(recv, MAX_TOKEN_PREAMBLE).await?;
            token.authenticate(&preamble)
        }
    }
}

/// The pairing-store admission retained so its bearer's revocation ends the
/// connection live. A listener token dies with its listener (ADR-0120).
pub(super) fn bearer_admission(
    admission: &QuicAdmission,
    credential: Option<&crate::auth::AuthenticatedCredential>,
) -> Option<crate::auth::BearerAdmission> {
    match (admission, credential) {
        (QuicAdmission::Store(store), Some(credential)) => Some(crate::auth::BearerAdmission::new(
            Arc::clone(store),
            credential,
        )),
        _ => None,
    }
}

/// Read `len: u32 BE` then that many bytes, or `None` when missing, empty,
/// longer than `max`, or truncated.
pub(super) async fn read_length_prefixed(
    recv: &mut quinn::RecvStream,
    max: usize,
) -> Option<([u8; LENGTH_PREFIX], Vec<u8>)> {
    let mut len_buf = [0u8; LENGTH_PREFIX];
    if !read_exact_quic(recv, &mut len_buf).await.ok()? {
        return None;
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > max {
        return None;
    }
    let mut body = vec![0u8; len];
    if !read_exact_quic(recv, &mut body).await.ok()? {
        return None;
    }
    Some((len_buf, body))
}
