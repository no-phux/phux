use super::commands::{MAX_PENDING_INPUT_RECEIPTS, admit_frame};
use super::hello::NegotiatedConnection;
use super::hello::framing_violation_close;
use super::plumbing::{ClientPlumbing, ConnectionClose};
use super::terminal_streams::handle_stream_event;
use crate::runtime::input_lane::{InputCredits, InputLaneHandle};
use crate::state::{ClientId, SharedState};
use crate::transport::quic::QuicStreamEvent;
use crate::transport::{FrameReader, FrameWriter};
use bytes::BytesMut;
use phux_protocol::policy::TransportType;
use phux_protocol::wire::frame::{DetachReason, ErrorCode};
use std::collections::HashSet;
use std::io;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Per-client task: reads frames in a loop and dispatches each one.
/// Outbound frames go through one per-client mailbox drained by a sibling
/// writer task, so every send shares one ordering domain.
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::significant_drop_tightening,
    reason = "one read loop: select, validate, guard, then FrameDispatch; the connection context comes verbatim from the accept loop"
)]
pub(in crate::runtime) async fn handle_client<R, W>(
    mut reader: R,
    writer: W,
    state: SharedState,
    client_id: ClientId,
    token: CancellationToken,
    root_token: CancellationToken,
    input_lane: Option<InputLaneHandle>,
    transport: TransportType,
    route_supports_quic_streams: bool,
) -> io::Result<()>
where
    R: FrameReader + 'static,
    W: FrameWriter + 'static,
{
    debug!(?client_id, "client task started");
    // Set when the connection's authority is withdrawn (workload-auth §7).
    let (revocation_tx, revocation_rx) = tokio::sync::watch::channel(None);
    state.with_mut(|server| {
        server.set_client_connection_cancellation(client_id, token.clone());
        server.set_revocation_signal(client_id, revocation_tx);
    });
    // However this task ends, no per-connection task outlives it.
    let _cancel_on_exit = token.clone().drop_guard();

    let mut plumbing = ClientPlumbing::spawn(writer, client_id, revocation_rx);
    let mut command_tasks = crate::runtime::command_tasks::CommandTasks::new(token.clone());
    let mut input_receipts = JoinSet::new();
    let mut held_commands = JoinSet::new();
    let service_http = crate::runtime::service_http::Exchanges::default();
    let input_receipt_slots =
        std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_INPUT_RECEIPTS));
    let mut input_credits = InputCredits::default();
    let mut used_attach_ids = HashSet::new();
    // Written once by HELLO; a duplicate HELLO is fatal.
    let mut negotiated: Option<NegotiatedConnection> = None;
    // QUIC multi-stream only, taken once HELLO negotiates it.
    let mut stream_events: Option<tokio::sync::mpsc::Receiver<QuicStreamEvent>> = None;
    // Absolute from admission: pre-HELLO PINGs must not keep a peer alive.
    let hello_deadline = tokio::time::sleep(crate::transport::HANDSHAKE_DEADLINE);
    tokio::pin!(hello_deadline);

    let ending = 'conn: loop {
        // Cancellation preempts a slow read. The frame arm precedes the
        // stream-event arm: a stream pump queues every complete frame before
        // its `Ended`, so this preserves input-before-FIN causality.
        let (framed, frame_origin) = tokio::select! {
            biased;
            () = token.cancelled() => {
                debug!(?client_id, "client task cancelled");
                break 'conn ConnectionEnding::Cancelled;
            }
            () = wait_initial_hello(hello_deadline.as_mut(), negotiated.is_none()) => {
                break 'conn hello_deadline_elapsed(client_id);
            }
            res = reader.read_frame() => match admit_read(res, client_id) {
                Ok(framed) => (framed, reader.frame_origin()),
                Err(ending) => break 'conn ending,
            },
            () = command_tasks.stopped() => break 'conn bulk_worker_stopped(client_id),
            Some(_) = input_receipts.join_next(), if !input_receipts.is_empty() => continue,
            Some(_) = held_commands.join_next(), if !held_commands.is_empty() => continue,
            event = next_stream_event(stream_events.as_mut()) => {
                dispatch_stream_event(
                    event,
                    &mut stream_events,
                    &state,
                    client_id,
                    &mut plumbing,
                    negotiated.as_ref(),
                    &token,
                ).await;
                continue;
            }
        };

        // A frame newer than this build is answered `Unsupported` and the
        // frame guard (workload-auth §6) runs before any routing or handler.
        let admitted = admit_frame(
            &state,
            &framed,
            negotiated.as_ref(),
            frame_origin,
            client_id,
            &plumbing.out_tx,
        )
        .await;
        let frame = match admitted {
            Ok(Some(frame)) => frame,
            Ok(None) => continue,
            Err(close) => break 'conn ConnectionEnding::Violation(close),
        };

        let dispatch = FrameDispatch {
            reader: &mut reader,
            state: &state,
            client_id,
            token: &token,
            root_token: &root_token,
            input_lane: input_lane.as_ref(),
            transport,
            route_supports_quic_streams,
            plumbing: &mut plumbing,
            negotiated: &mut negotiated,
            stream_events: &mut stream_events,
            used_attach_ids: &mut used_attach_ids,
            command_tasks: &mut command_tasks,
            input_receipts: &mut input_receipts,
            input_receipt_slots: &input_receipt_slots,
            input_credits: &mut input_credits,
            held_commands: &mut held_commands,
            service_http: &service_http,
        };
        if let std::ops::ControlFlow::Break(ending) = dispatch.dispatch(frame).await {
            break 'conn ending;
        }
    };
    // A closing connection withdraws its holds before any await (ADR-0128):
    // no decision can land in the teardown window.
    crate::runtime::approvals::withdraw(&state, client_id);
    held_commands.shutdown().await;
    crate::runtime::service_http::close_all(&state, &service_http);
    input_receipts.shutdown().await;
    command_tasks.shutdown().await;
    plumbing.end(ending, &state, client_id, &root_token).await;
    Ok(())
}

/// The next QUIC Terminal-stream event; pending forever when the
/// connection has no stream receiver (yet, or any more).
async fn next_stream_event(
    stream_events: Option<&mut tokio::sync::mpsc::Receiver<QuicStreamEvent>>,
) -> Option<QuicStreamEvent> {
    match stream_events {
        Some(rx) => rx.recv().await,
        None => core::future::pending().await,
    }
}

/// A read's frame, or how the connection ends: EOF and transport errors
/// leave nobody to tell; a framing violation is owed its close.
fn admit_read(
    res: io::Result<Option<BytesMut>>,
    client_id: ClientId,
) -> Result<BytesMut, ConnectionEnding> {
    match res {
        Ok(Some(framed)) => Ok(framed),
        Ok(None) => {
            debug!("client disconnected (eof)");
            Err(ConnectionEnding::TransportGone)
        }
        Err(err) => {
            let Some(close) = framing_violation_close(&err, client_id) else {
                debug!(error = %err, "client read error; closing");
                return Err(ConnectionEnding::TransportGone);
            };
            Err(ConnectionEnding::Violation(close))
        }
    }
}

/// The peer did not complete HELLO within the handshake deadline.
fn hello_deadline_elapsed(client_id: ClientId) -> ConnectionEnding {
    warn!(
        ?client_id,
        "client did not complete HELLO before deadline; closing"
    );
    // Not `VERSION_INCOMPATIBLE`: a slow link is no version verdict, and
    // clients stop redialing on that code.
    ConnectionEnding::Violation(ConnectionClose {
        attached_reason: None,
        detach_reason: DetachReason::ProtocolError,
        code: ErrorCode::MalformedMessage,
        message: "HELLO deadline elapsed".to_owned(),
    })
}

/// The connection's bulk command worker died under it.
fn bulk_worker_stopped(client_id: ClientId) -> ConnectionEnding {
    warn!(
        ?client_id,
        "bulk command worker stopped unexpectedly; closing"
    );
    ConnectionEnding::Violation(ConnectionClose {
        attached_reason: Some("bulk command worker stopped"),
        detach_reason: DetachReason::ProtocolError,
        code: ErrorCode::InternalError,
        message: "bulk command worker stopped unexpectedly".to_owned(),
    })
}

/// How one connection's frame loop ended.
pub(super) enum ConnectionEnding {
    /// EOF or a transport error: nobody is left to tell.
    TransportGone,
    /// The peer broke the protocol; it is owed `ERROR` then `DETACHED`.
    Violation(ConnectionClose),
    /// The connection (or the server) was cancelled.
    Cancelled,
}

/// One decoded client frame's dispatch context: the connection's state,
/// borrowed from [`handle_client`] for the length of one frame.
/// `Break` ends the connection with that [`ConnectionEnding`].
pub(super) struct FrameDispatch<'a, R> {
    pub(super) reader: &'a mut R,
    pub(super) state: &'a SharedState,
    pub(super) client_id: ClientId,
    pub(super) token: &'a CancellationToken,
    pub(super) root_token: &'a CancellationToken,
    pub(super) input_lane: Option<&'a InputLaneHandle>,
    pub(super) transport: TransportType,
    pub(super) route_supports_quic_streams: bool,
    pub(super) plumbing: &'a mut ClientPlumbing,
    pub(super) negotiated: &'a mut Option<NegotiatedConnection>,
    pub(super) stream_events: &'a mut Option<tokio::sync::mpsc::Receiver<QuicStreamEvent>>,
    pub(super) used_attach_ids: &'a mut HashSet<u32>,
    pub(super) command_tasks: &'a mut crate::runtime::command_tasks::CommandTasks,
    pub(super) input_receipts: &'a mut JoinSet<()>,
    pub(super) input_receipt_slots: &'a std::sync::Arc<tokio::sync::Semaphore>,
    pub(super) input_credits: &'a mut InputCredits,
    pub(super) held_commands: &'a mut JoinSet<()>,
    pub(super) service_http: &'a crate::runtime::service_http::Exchanges,
}

pub(super) type FrameFlow = std::ops::ControlFlow<ConnectionEnding>;

async fn dispatch_stream_event(
    event: Option<QuicStreamEvent>,
    stream_events: &mut Option<tokio::sync::mpsc::Receiver<QuicStreamEvent>>,
    state: &SharedState,
    client_id: ClientId,
    plumbing: &mut ClientPlumbing,
    negotiated: Option<&NegotiatedConnection>,
    token: &CancellationToken,
) {
    let Some(event) = event else {
        // Disable the select arm permanently. Polling a closed receiver is
        // immediately ready and would starve control EOF.
        *stream_events = None;
        return;
    };
    handle_stream_event(state, client_id, event, plumbing, negotiated, token).await;
}

async fn wait_initial_hello(deadline: std::pin::Pin<&mut tokio::time::Sleep>, waiting: bool) {
    if waiting {
        deadline.await;
    } else {
        core::future::pending::<()>().await;
    }
}
