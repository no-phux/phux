//! The accept loop, and the small per-frame answers: PONG, DETACH, attach-id vetting, and terminal replies.

use super::{
    AcceptErrorDisposition, CancellationToken, ClientId, ConnectionCap, DetachReason, ErrorCode,
    FrameKind, FramingError, HashSet, Incoming, InputLaneHandle, JoinSet, NegotiatedConnection,
    Outbound, ServerError, ServerState, SharedState, WS_REJECTION_WARN_INTERVAL, WireResourceId,
    abort_output_pumps, debug, detach_and_release_consumer_state, error, handle_client,
    handle_terminal_reply, info, io, mpsc, release_connection_state, trace, warn,
};

/// Accept connections and spawn a local client task for each (ADR-0014).
/// Root cancellation stops admission, then waits (bounded) for every client
/// task to flush its shutdown `DETACHED`.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: the server runs on a LocalSet; per-connection transports (L::Reader/Writer) are !Send by design"
)]
pub(crate) async fn accept_loop<L: Incoming>(
    listener: &L,
    state: SharedState,
    root_token: CancellationToken,
    // ADR-0044 input lane; `None` routes input inline.
    input_lane: Option<InputLaneHandle>,
) -> Result<(), ServerError> {
    let mut clients: JoinSet<()> = JoinSet::new();
    let cap = ConnectionCap::new(listener.max_connections());
    loop {
        tokio::select! {
            () = root_token.cancelled() => {
                info!("root cancellation token fired; draining client tasks");
                if tokio::time::timeout(CLIENT_SHUTDOWN_DRAIN_TIMEOUT, async {
                    while clients.join_next().await.is_some() {}
                })
                .await
                .is_err()
                {
                    warn!("client shutdown drain timed out; aborting remaining tasks");
                    clients.shutdown().await;
                }
                return Ok(());
            }
            // Reap finished connections, or the set keeps one entry per
            // connection for the server's lifetime.
            Some(_) = clients.join_next(), if !clients.is_empty() => {}
            accept = listener.accept() => {
                match accept {
                    Ok((reader, writer, connection_identity)) => {
                        // Over the cap: dropping the transport closes it.
                        let Some(live) = cap.admit(listener.kind()) else {
                            continue;
                        };
                        debug!(transport = listener.kind(), "client connected");
                        // Counted before the task exists so the idle watchdog
                        // never sees an unattended gap.
                        state.with_mut(ServerState::note_connection_opened);
                        let client_id = state.with_mut(ServerState::new_client_id);
                        state.with_mut(|s| s.set_connection_identity(client_id, connection_identity));
                        let task_state = state.clone();
                        let client_token = root_token.child_token();
                        let task_root_token = root_token.clone();
                        let task_input_lane = input_lane.clone();
                        let task_transport = listener.transport_type();
                        let task_supports_quic_streams = listener.supports_quic_streams();
                        clients.spawn_local(async move {
                            let _live = live;
                            if let Err(err) = handle_client(reader, writer, task_state.clone(), client_id, client_token, task_root_token.clone(), task_input_lane, task_transport, task_supports_quic_streams).await {
                                warn!(error = %err, "client task ended with error");
                            }
                            // The one site that forgets connection state.
                            release_connection_state(&task_state, client_id);
                            // R13: the last connection closing on a server
                            // that runs nothing ends it.
                            let due = task_state.with_mut(|s| {
                                s.note_connection_closed();
                                s.self_exit_due()
                            });
                            if due && !task_root_token.is_cancelled() {
                                info!("last client left a server running nothing; server self-exit");
                                task_root_token.cancel();
                            }
                        });
                    }
                    Err(err) => {
                        if listener.accept_errors_are_fatal() {
                            return Err(err.into());
                        }
                        match listener.accept_error_disposition(&err) {
                            AcceptErrorDisposition::Default => {
                                // Typically transient (EMFILE, ECONNABORTED).
                                error!(error = %err, "accept failed");
                            }
                            AcceptErrorDisposition::PeerRejected {
                                stage,
                                source_ip,
                                warn_suppressed,
                            } => {
                                // WARN is rate-limited so a peer cannot flood logs.
                                debug!(
                                    transport = listener.kind(),
                                    stage,
                                    %source_ip,
                                    "peer connection rejected"
                                );
                                if let Some(suppressed_count) = warn_suppressed {
                                    warn!(
                                        transport = listener.kind(),
                                        stage,
                                        %source_ip,
                                        suppressed_count,
                                        interval_seconds = WS_REJECTION_WARN_INTERVAL.as_secs(),
                                        "peer connection rejections observed"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub(super) const WRITER_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Bound the server-wide wait for clients to flush their shutdown ending.
pub(super) const CLIENT_SHUTDOWN_DRAIN_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(2);

pub(super) async fn close_client_writer(
    out_tx: mpsc::Sender<Outbound>,
    writer_close: &tokio::sync::watch::Sender<bool>,
    sibling_tasks: &mut JoinSet<()>,
) {
    // The writer drains every frame ordered before this, then closes.
    let _ = writer_close.send(true);
    drop(out_tx);
    if tokio::time::timeout(WRITER_DRAIN_TIMEOUT, async {
        while sibling_tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        sibling_tasks.abort_all();
        while sibling_tasks.join_next().await.is_some() {}
    }
}

/// The typed SPEC §5 framing violation inside a transport read error, if
/// any: every reader keeps the [`FramingError`] as the `io::Error` source.
pub(super) fn framing_violation(err: &io::Error) -> Option<FramingError> {
    err.get_ref()
        .and_then(|source| source.downcast_ref::<FramingError>())
        .copied()
}

/// SPEC §7.4: echo the nonce in PONG.
pub(super) async fn reply_pong(out_tx: &mpsc::Sender<Outbound>, client_id: ClientId, nonce: u64) {
    debug!(nonce, "PING -> PONG");
    if out_tx
        .send(Outbound::Frame(FrameKind::Pong { nonce }))
        .await
        .is_err()
    {
        trace!(?client_id, nonce, "PONG send dropped: writer gone");
    }
}

/// SPEC §7.3: answer DETACH with DETACHED and keep reading; the same
/// connection may attach again.
pub(super) async fn detach_on_request(
    state: &SharedState,
    client_id: ClientId,
    out_tx: &mpsc::Sender<Outbound>,
    output_pumps: &mut JoinSet<()>,
) {
    info!(?client_id, "DETACH");
    abort_output_pumps(output_pumps, client_id, "DETACH").await;
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::Detached {
            reason: Some(DetachReason::Requested),
            message: String::new(),
        }))
        .await;
    detach_and_release_consumer_state(state, client_id);
}

/// The verdict on one ATTACH's `attach_id`.
pub(super) enum AttachIdVerdict {
    /// Unused on this connection: the attach proceeds.
    Fresh,
    /// Already used here: answered with an `ERROR`, the connection survives.
    Reused,
    /// The reserved zero id: a malformed frame, so the connection ends.
    Reserved,
}

/// Classify an ATTACH's `attach_id`: each names one aggregate generation for
/// the life of the connection, so reuse would collide with a completed
/// stream/bootstrap key.
pub(super) fn classify_attach_id(
    attach_id: u32,
    used_attach_ids: &mut HashSet<u32>,
    client_id: ClientId,
) -> AttachIdVerdict {
    if attach_id == 0 {
        warn!(?client_id, "ATTACH used reserved zero attach_id; closing");
        return AttachIdVerdict::Reserved;
    }
    if used_attach_ids.insert(attach_id) {
        AttachIdVerdict::Fresh
    } else {
        AttachIdVerdict::Reused
    }
}

/// Hand one `INPUT_TERMINAL_REPLY` to the pane if the connection advertised
/// it; otherwise report the type without ending the connection.
pub(super) async fn dispatch_terminal_reply(
    client_id: ClientId,
    selection: NegotiatedConnection,
    terminal_id: &WireResourceId,
    bytes: bytes::Bytes,
    out_tx: &mpsc::Sender<Outbound>,
) {
    if !selection.accepts_terminal_reply() {
        crate::runtime::send_error(
            out_tx,
            ErrorCode::UnknownMessageType,
            "INPUT_TERMINAL_REPLY was not advertised for this connection",
        )
        .await;
        return;
    }
    handle_terminal_reply(client_id, terminal_id, &bytes);
}
