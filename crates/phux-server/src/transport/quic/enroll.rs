//! The enrollment ALPN on a QUIC listener (`docs/remote.md#pairing-with-a-link`,
//! ADR-0154): one request, one reply, then the connection closes. Nothing
//! here reads a phux frame or mints a grant.

use phux_protocol::enroll::{MAX_REQUEST, Reply, Request};
use tracing::{debug, info};

use super::{ADMISSION_DEADLINE, AUTH_FAILED_CODE};
use crate::workload::WorkloadPaths;

mod diagnostics;
use diagnostics::{Phase, Trace};

/// How long the reply may take to reach the device before the connection
/// closes anyway.
const REPLY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Serve one enrollment exchange on `conn`, then close it.
pub(super) async fn serve(conn: &quinn::Connection, paths: &WorkloadPaths) {
    let trace = Trace::new();
    let remote = conn.remote_address();
    let exchange = async {
        let (send, mut recv) = conn.accept_bi().await.ok()?;
        let bytes = recv.read_to_end(MAX_REQUEST).await.ok()?;
        Some((send, bytes))
    };
    let Ok(Some((mut send, bytes))) = tokio::time::timeout(ADMISSION_DEADLINE, exchange).await
    else {
        debug!(%remote, "enrollment request missing, oversized, or late");
        conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
        return;
    };
    trace.record(Phase::RequestReceived);
    let reply = match Request::decode(&bytes) {
        Ok(request) => issue(paths.clone(), request, trace).await,
        Err(error) => {
            debug!(%remote, %error, "enrollment request malformed");
            Reply::Refused
        }
    };
    let refused = matches!(reply, Reply::Refused);
    let delivered = async {
        send.write_all(&reply.encode()).await.map_err(|_| ())?;
        send.finish().map_err(|_| ())?;
        send.stopped().await.map_err(|_| ())
    };
    trace.record(diagnostics::delivery_phase(
        &tokio::time::timeout(REPLY_DEADLINE, delivered).await,
    ));
    if refused {
        conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
    } else {
        conn.close(0_u32.into(), b"enrolled");
    }
}

/// Redeem, sign, and record off the runtime thread: file locks, fsyncs, and
/// signing all block.
async fn issue(paths: WorkloadPaths, request: Request, trace: Trace) -> Reply {
    trace.record(Phase::WorkQueued);
    let joined = tokio::task::spawn_blocking(move || {
        trace.record(Phase::WorkStarted);
        let result =
            crate::workload::tickets::enroll_with_ticket(&paths, &request.ticket, &request.csr);
        trace.record(if result.is_ok() {
            Phase::Issued
        } else {
            Phase::Refused
        });
        result
    })
    .await;
    trace.record(if joined.is_ok() {
        Phase::WorkJoined
    } else {
        Phase::TaskFailed
    });
    match joined {
        Ok(Ok(enrolled)) => {
            info!(
                ticket = %enrolled.ticket,
                credential = %enrolled.credential,
                "workload credential enrolled with a ticket; `phux workload revoke` it if this was not your device"
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
    }
}
