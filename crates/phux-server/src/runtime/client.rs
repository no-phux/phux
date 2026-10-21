//! Client connections: accept, HELLO, dispatch, metadata, and the writer.
//!
//! Pane event drains, the pane exit lifecycle, and the socket's filesystem
//! guard live in the submodules.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use bytes::BytesMut;
use phux_dial::window::SendWindow;
use phux_protocol::PROTOCOL_VERSION;
#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ClientCapabilities, Compression, LayerSet, QUIC_STREAMS,
    ServerCapabilities, ServerFeature, ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet,
    select_bootstrap_profile,
};
use phux_protocol::ids::{ResourceId as WireResourceId, StreamId};
use phux_protocol::policy::TransportType;
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::Scope;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, DetachReason, ErrorCode, FrameKind, RESOURCE_AGENT_KEY,
};
use phux_protocol::wire::framing::FramingError;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use super::input_lane::{InputCredits, InputLaneHandle, InputStalled, RoutedInput};
use super::{
    ServerError, SpawnRequest, bootstrap_attach_terminal, handle_attach, handle_detach_terminal,
    handle_frame_ack, handle_move_terminal, handle_terminal_input, handle_terminal_reply,
    handle_terminal_resize, handle_viewport_resize, subscribe_attach_terminal,
};
use crate::auth::Standing;
use crate::hooks::HookEvent;
use crate::policy::{Goodbye, Revocation};
use crate::state::{
    ClientId, DEFAULT_CLIENT_MAILBOX, Outbound, ServerInterceptedKey, ServerState, SharedState,
    TerminalInput,
};
use crate::terminal_actor::ConsumerDetachRequest;
use crate::transport::quic::{
    QuicStreamEvent, QuicStreamFailure, QuicWriter, pump_terminal_stream, refuse_terminal_stream,
};
use crate::transport::{
    AcceptErrorDisposition, FrameOrigin, FrameReader, FrameWriter, Incoming,
    WS_REJECTION_WARN_INTERVAL,
};

pub mod pane_events;
pub mod pane_exit;
pub mod socket;

use pane_events::invalidate_agent_detector;
pub(crate) use pane_events::{
    PaneEvents, ensure_event_pump, reap_pane_journaling_close, spawn_agent_state_drain,
};
pub(super) use pane_exit::journal_pane_closed;
pub(crate) use pane_exit::{broadcast_terminal_closed, spawn_terminal_exit_watcher};
pub(crate) use socket::{handle_existing_socket, prepare_socket_dir, secure_socket_file};

pub mod accept;
pub mod connection_state;
mod dispatch;
mod frame_handlers;
mod hello;
pub mod metadata_read;
pub mod metadata_set;
mod outbound;
mod plumbing;
mod session_create;
mod session_frames;
pub mod streams;
pub mod writer;

pub(crate) use accept::accept_loop;
use accept::{
    AttachIdVerdict, WRITER_DRAIN_TIMEOUT, classify_attach_id, close_client_writer,
    detach_on_request, dispatch_terminal_reply, framing_violation, reply_pong,
};
use connection_state::{
    ConnectionCap, NegotiatedConnection, dispatch_stream_event, runtime_server_features,
    wait_initial_hello,
};
pub(crate) use connection_state::{
    detach_and_release_consumer_state, fire_client_detached, release_connection_state,
    release_revoked_consumer_state,
};
use dispatch::{
    CommandDispatch, CommandDispatchOutcome, MAX_PENDING_INPUT_RECEIPTS, Undispatched, admit_frame,
};
pub(super) use dispatch::{Route, route};
#[cfg(test)]
use dispatch::{spawn_input_receipt, validate_dispatch_frame};
use hello::{
    HelloRequest, before_hello_close, decode_client_frame, framing_violation_close,
    is_pre_hello_type, negotiate_hello, reject_frame_before_hello,
};
pub(crate) use metadata_read::{
    broadcast_event, handle_get_metadata, handle_subscribe_events, handle_subscribe_metadata,
};
use metadata_read::{is_agent_session, protected_key_refusal};
pub(crate) use metadata_set::{handle_delete_metadata, handle_list_metadata, handle_set_metadata};
use outbound::{OutboundGenerationFence, drain_ready_into_batch, encode_into_batch};
#[cfg(test)]
use plumbing::StreamBinding;
use plumbing::{ClientPlumbing, ConnectionClose};
pub(super) use session_create::detach_clients_of_killed_session;
#[cfg(test)]
use session_create::session_create_result_key;
use session_create::{
    apply_session_keep_empty, handle_session_create_metadata, is_reserved_session_create_result,
};
use session_frames::{
    AttachFrame, HistoryPageRequest, direction_invalid, is_input_frame, is_metadata_frame,
    is_stream_frame, serve_history_request,
};
pub(crate) use streams::abort_output_pumps;
use streams::{handle_stream_event, route_client_input};
pub(crate) use writer::RevocationWatch;
use writer::writer_task;

#[cfg(test)]
mod input_receipt_capacity_tests;

#[cfg(test)]
mod negotiated_feature_tests;

/// Per-client task: reads frames in a loop and dispatches each one.
/// Outbound frames go through one per-client mailbox drained by a sibling
/// writer task, so every send shares one ordering domain.
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::significant_drop_tightening,
    reason = "one read loop: select, validate, guard, then FrameDispatch; the connection context comes verbatim from the accept loop"
)]
pub(crate) async fn handle_client<R, W>(
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
    let mut command_tasks = super::command_tasks::CommandTasks::new(token.clone());
    let mut input_receipts = JoinSet::new();
    let mut held_commands = JoinSet::new();
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
        };
        if let std::ops::ControlFlow::Break(ending) = dispatch.dispatch(frame).await {
            break 'conn ending;
        }
    };
    // A closing connection withdraws its holds before any await (ADR-0128):
    // no decision can land in the teardown window.
    super::approvals::withdraw(&state, client_id);
    held_commands.shutdown().await;
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
    ConnectionEnding::Violation(ConnectionClose {
        attached_reason: None,
        detach_reason: DetachReason::ProtocolError,
        code: ErrorCode::VersionIncompatible,
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
enum ConnectionEnding {
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
struct FrameDispatch<'a, R> {
    reader: &'a mut R,
    state: &'a SharedState,
    client_id: ClientId,
    token: &'a CancellationToken,
    root_token: &'a CancellationToken,
    input_lane: Option<&'a InputLaneHandle>,
    transport: TransportType,
    route_supports_quic_streams: bool,
    plumbing: &'a mut ClientPlumbing,
    negotiated: &'a mut Option<NegotiatedConnection>,
    stream_events: &'a mut Option<tokio::sync::mpsc::Receiver<QuicStreamEvent>>,
    used_attach_ids: &'a mut HashSet<u32>,
    command_tasks: &'a mut super::command_tasks::CommandTasks,
    input_receipts: &'a mut JoinSet<()>,
    input_receipt_slots: &'a std::sync::Arc<tokio::sync::Semaphore>,
    input_credits: &'a mut InputCredits,
    held_commands: &'a mut JoinSet<()>,
}

type FrameFlow = std::ops::ControlFlow<ConnectionEnding>;

impl<R: FrameReader> FrameDispatch<'_, R> {
    /// Dispatch one admitted frame: one arm per client-to-server kind.
    async fn dispatch(self, frame: FrameKind) -> FrameFlow {
        match frame {
            FrameKind::Hello {
                client_name,
                protocol_major,
                protocol_minor,
                protocol_patch,
                client_caps,
            } => {
                let hello = HelloRequest {
                    client_name,
                    protocol_major,
                    protocol_minor,
                    protocol_patch,
                    client_caps,
                };
                return self.hello(hello).await;
            }
            FrameKind::Ping { nonce } => {
                reply_pong(&self.plumbing.out_tx, self.client_id, nonce).await;
            }
            FrameKind::Attach {
                attach_id,
                target,
                viewport,
                request_scrollback,
                scrollback_limit_lines,
                role_policy,
            } => {
                let request = AttachFrame {
                    attach_id,
                    target,
                    viewport,
                    request_scrollback,
                    scrollback_limit_lines,
                    role_policy,
                };
                return self.attach(request).await;
            }
            FrameKind::Detach => self.detach().await,
            FrameKind::Command {
                request_id,
                command,
            } => return self.command(request_id, command).await,
            frame if is_input_frame(&frame) => return self.input(frame).await,
            frame if is_stream_frame(&frame) => return self.stream_frame(frame).await,
            frame if is_metadata_frame(&frame) => return self.metadata(frame).await,
            frame => return self.resource(frame).await,
        }
        FrameFlow::Continue(())
    }
}

// L3 metadata dispatch (SPEC §7.4 / §11.L3). Replies are gated on
// `client_speaks_l3` (SPEC §16.4): a non-L3 consumer gets silence.

#[cfg(test)]
mod outbound_generation_fence_tests;

#[cfg(all(test, feature = "native-engine", not(target_arch = "wasm32")))]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "tests"
)]
mod fatal_preflight_close_tests;
#[cfg(test)]
mod writer_close_tests;

#[cfg(test)]
mod metadata_tests;
