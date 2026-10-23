//! Server control: extracted from the shared module.
use super::{
    CancellationToken, ClientId, CommandResult, ErrorCode, Outbound, SharedState, error, info,
    send_result, warn,
};

/// Handle `SHUTDOWN`: cancel the root token, the same shutdown idle-exit and
/// signals take, after acking. Accepted on the Unix socket only (L1 §5.1):
/// whether a remote peer may stop the host is an unanswered policy question.
pub(super) async fn handle_shutdown(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
) {
    let transport = state.with(|s| s.peer_identity(client_id).map(|peer| peer.transport));
    if !matches!(
        transport,
        Some(phux_protocol::policy::TransportType::UnixSocket)
    ) {
        warn!(
            ?client_id,
            ?transport,
            "SHUTDOWN refused: local socket only"
        );
        let refusal = CommandResult::Error {
            code: ErrorCode::PermissionDenied,
            message: "SHUTDOWN is accepted on the local socket only".to_owned(),
        };
        send_result(out_tx, request_id, refusal).await;
        return;
    }

    info!(?client_id, "SHUTDOWN requested; stopping the server");
    // ADR-0104 §4: record the close reason before the exit watchers run.
    state.with_mut(|s| {
        s.flush_reports_before_shutdown();
        for resource in s.resource_ids() {
            s.mark_resource_closing(
                resource,
                phux_protocol::wire::frame::CloseReason::ServerShutdown,
            );
        }
    });
    send_result(out_tx, request_id, CommandResult::Ok).await;
    // Let the ack reach the writer before the teardown races it.
    tokio::task::yield_now().await;
    root_token.cancel();
}

/// Handle `UPGRADE` (ADR-0032): prepare the graceful re-exec, ack the client,
/// then replace the process. Acks itself (rather than returning a
/// `CommandResult`) because on success it never returns.
pub(super) async fn handle_upgrade(
    state: &SharedState,
    request_id: u32,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    let result = match super::super::upgrade::prepare_upgrade(state).await {
        Ok(plan) => {
            // Best-effort ack flushed before exec; the client reconnects.
            send_result(out_tx, request_id, CommandResult::Ok).await;
            tokio::task::yield_now().await;
            info!("UPGRADE: re-exec'ing the new binary");
            let err = plan.exec().await;
            // Only reached if exec failed: the old image keeps serving.
            error!(error = %err, "UPGRADE exec failed; continuing on the current image");
            return;
        }
        Err(err) => {
            warn!(error = %err, "UPGRADE preparation failed");
            CommandResult::Error {
                code: ErrorCode::InternalError,
                message: format!("upgrade failed: {err}"),
            }
        }
    };
    send_result(out_tx, request_id, result).await;
}
