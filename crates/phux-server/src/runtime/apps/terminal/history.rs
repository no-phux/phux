use crate::runtime::connections::hello::NegotiatedConnection;
use crate::state::{ClientId, Outbound, SharedState};
use phux_protocol::caps::BootstrapProfile;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::FrameKind;
use phux_terminal::actor::TerminalResource as _;
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

/// The `HISTORY_REQUEST` frame's payload.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(in crate::runtime) struct HistoryPageRequest {
    pub(in crate::runtime) terminal_id: WireResourceId,
    pub(in crate::runtime) stream_id: phux_protocol::ids::StreamId,
    pub(in crate::runtime) bootstrap_id: phux_protocol::ids::BootstrapId,
    pub(in crate::runtime) cursor: bytes::Bytes,
    pub(in crate::runtime) max_bytes: u32,
    pub(in crate::runtime) max_rows: u32,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
/// The Terminal facet a `HISTORY_REQUEST` pages against: `None` for an
/// unknown terminal or a resource of another kind.
fn history_terminal(
    state: &SharedState,
    terminal_id: &WireResourceId,
) -> Option<phux_terminal::actor::TerminalHandle> {
    let handle = state.with(|server| {
        server
            .resource_from_wire(terminal_id)
            .and_then(|pane| server.resource_handle(pane).cloned())
    });
    let Some(handle) = handle else {
        warn!(?terminal_id, "HISTORY_REQUEST for an unknown terminal");
        return None;
    };
    match handle.terminal() {
        Ok(terminal) => Some(terminal.clone()),
        Err(error) => {
            warn!(?terminal_id, %error, "HISTORY_REQUEST for a non-Terminal resource");
            None
        }
    }
}

/// Answer one `HISTORY_REQUEST` from the pane's replica. SPEC L1 §4.5: every
/// failure tombstones the cursor rather than sending an uncorrelated `ERROR`.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(in crate::runtime) async fn serve_history_request(
    state: &SharedState,
    client_id: ClientId,
    out_tx: &mpsc::Sender<Outbound>,
    selection: NegotiatedConnection,
    request: HistoryPageRequest,
) {
    use phux_protocol::wire::frame::HistoryTombstoneReason;

    let HistoryPageRequest {
        terminal_id,
        stream_id,
        bootstrap_id,
        cursor,
        max_bytes,
        max_rows,
    } = request;
    let tombstone = |reason| {
        Outbound::Frame(FrameKind::HistoryTombstone {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: cursor.clone(),
            reason,
        })
    };
    if !matches!(
        selection.profile,
        BootstrapProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
            ..
        }
    ) {
        warn!(
            ?terminal_id,
            "HISTORY_REQUEST requires negotiated native snapshot v1"
        );
        let _ = out_tx
            .send(tombstone(HistoryTombstoneReason::CodecFailure))
            .await;
        return;
    }
    let Some(terminal) = history_terminal(state, &terminal_id) else {
        let _ = out_tx
            .send(tombstone(HistoryTombstoneReason::Released))
            .await;
        return;
    };
    let Ok(permit) = out_tx.clone().reserve_owned().await else {
        return;
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    if terminal
        .native_history
        .send(phux_terminal::actor::NativeHistoryRequest {
            permit,
            owner: client_id.0,
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: cursor.clone(),
            max_bytes,
            max_rows,
            limits: selection.limits,
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return;
    }
    let Ok(reply) = reply_rx.await else {
        return;
    };
    match reply.result {
        Ok(frame) => {
            reply.permit.send(Outbound::Frame(frame));
        }
        Err(error) => {
            // Mirrors the actor's own in-band mapping.
            let reason = match error {
                phux_terminal::native_state::NativeStateError::OutOfMemory
                | phux_terminal::native_state::NativeStateError::OutOfSpace { .. }
                | phux_terminal::native_state::NativeStateError::LimitExceeded => {
                    HistoryTombstoneReason::Limit
                }
                _ => HistoryTombstoneReason::CodecFailure,
            };
            warn!(
                %error,
                ?terminal_id,
                "native history request failed; tombstoning the cursor"
            );
            reply.permit.send(tombstone(reason));
        }
    }
}
