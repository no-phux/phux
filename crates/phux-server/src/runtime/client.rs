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

const MAX_PENDING_INPUT_RECEIPTS: usize = 128;

fn spawn_input_receipt(
    receipts: &mut JoinSet<()>,
    slot: tokio::sync::OwnedSemaphorePermit,
    out_tx: mpsc::Sender<Outbound>,
    request_id: u32,
    receipt: super::input_lane::InputReceipt,
) {
    receipts.spawn_local(async move {
        let _slot = slot;
        let result = receipt.await;
        let _ = out_tx
            .send(Outbound::Frame(FrameKind::CommandResult {
                request_id,
                result,
            }))
            .await;
    });
}

const fn requires_terminal_stream(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::InputKey { .. }
            | FrameKind::ResizeTerminal { .. }
            | FrameKind::InputMouse { .. }
            | FrameKind::InputFocus { .. }
            | FrameKind::InputPaste { .. }
            | FrameKind::InputTerminalReply { .. }
            | FrameKind::FrameAck { .. }
            | FrameKind::HistoryRequest { .. }
    )
}

fn validate_dispatch_frame(
    framed: &BytesMut,
    negotiated: Option<&NegotiatedConnection>,
    origin: FrameOrigin,
    client_id: ClientId,
) -> Result<FrameKind, ConnectionClose> {
    // Before HELLO only HELLO and PING are legal. Refuse anything else by its
    // type byte, so an unauthenticated peer never reaches the other body
    // decoders (server-to-client snapshots and lists included).
    if negotiated.is_none() && !is_pre_hello_type(framed) {
        return Err(before_hello_close(client_id));
    }
    let frame = decode_client_frame(framed, negotiated)?;
    if negotiated.is_some_and(|selection| selection.quic_streams())
        && origin == FrameOrigin::Control
        && requires_terminal_stream(&frame)
    {
        return Err(ConnectionClose {
            attached_reason: Some("Terminal frame sent on QUIC control stream"),
            detach_reason: DetachReason::ProtocolError,
            code: ErrorCode::MalformedMessage,
            message: "Terminal-scoped frame requires a bound QUIC stream".to_owned(),
        });
    }
    if let Some(close) = reject_frame_before_hello(&frame, negotiated.is_some(), client_id) {
        return Err(close);
    }
    Ok(frame)
}

/// Where an admitted command is dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Route {
    /// The input lane: acknowledged or routed input to a local Terminal.
    InputLane,
    /// The connection's bulk worker, retaining this many payload bytes.
    Bulk(usize),
    /// The command handler.
    Handler,
}

/// The route an admitted command takes. An approved held command asks the
/// same question (ADR-0128), so it routes as it would have unheld.
pub(super) fn route(command: &Command, has_input_lane: bool) -> Route {
    let local_input = matches!(
        command,
        Command::ApplyInput { terminal_id, .. } | Command::RouteInput { terminal_id, .. }
            if matches!(terminal_id, WireResourceId::Local { .. })
    );
    if has_input_lane && local_input {
        return Route::InputLane;
    }
    super::command_tasks::CommandTasks::retained_bytes(command).map_or(Route::Handler, Route::Bulk)
}

struct CommandDispatch<'a> {
    state: &'a SharedState,
    client_id: ClientId,
    out_tx: &'a mpsc::Sender<Outbound>,
    input_lane: Option<&'a InputLaneHandle>,
    token: &'a CancellationToken,
    root_token: &'a CancellationToken,
    selection: NegotiatedConnection,
    command_tasks: &'a mut super::command_tasks::CommandTasks,
    input_receipts: &'a mut JoinSet<()>,
    input_receipt_slots: &'a std::sync::Arc<tokio::sync::Semaphore>,
    /// This connection's pane input credits (ADR-0144).
    input_credits: &'a mut InputCredits,
    /// The waiters of this connection's held commands (ADR-0128). Aborted
    /// with the connection, which withdraws their approvals.
    held_commands: &'a mut JoinSet<()>,
}

enum CommandDispatchOutcome {
    Completed(Option<WireResourceId>),
    Cancelled,
}

impl CommandDispatch<'_> {
    async fn run(self, request_id: u32, command: Command) -> CommandDispatchOutcome {
        let token = self.token.clone();
        tokio::select! {
            biased;
            () = token.cancelled() => CommandDispatchOutcome::Cancelled,
            detached = self.run_inner(request_id, command) => {
                CommandDispatchOutcome::Completed(detached)
            }
        }
    }

    async fn run_inner(mut self, request_id: u32, command: Command) -> Option<WireResourceId> {
        let command_started = std::time::Instant::now();
        // workload-auth §6: the command guard, above the input lane, the bulk
        // worker, and every handler and satellite relay below.
        match super::dispatch_guard::guard_command(
            self.state,
            self.client_id,
            request_id,
            &command,
            self.out_tx,
        )
        .await
        {
            super::dispatch_guard::Guarded::Admitted => {}
            super::dispatch_guard::Guarded::Refused => return None,
            super::dispatch_guard::Guarded::Held => {
                self.hold(request_id, command).await;
                return None;
            }
        }
        let defer_subscription = self.selection.quic_streams();
        let detached_stream = defer_subscription
            .then(|| match &command {
                Command::DetachResource { terminal_id } => Some(terminal_id.clone()),
                _ => None,
            })
            .flatten();

        match (route(&command, self.input_lane.is_some()), self.input_lane) {
            (Route::InputLane, Some(lane)) => {
                super::commands::note_local_use(self.state, self.client_id, &command);
                self.submit_input(lane, request_id, command).await;
                return None;
            }
            (Route::Bulk(retained), _) => {
                self.submit_bulk(request_id, command, retained, command_started)
                    .await;
                return None;
            }
            _ => {}
        }
        run_handler(&self.context(), request_id, command, command_started).await;
        detached_stream
    }

    /// This connection's dispatch context, owned, for work that outlives
    /// the dispatch.
    fn context(&self) -> super::commands::CommandContext {
        super::commands::CommandContext {
            state: self.state.clone(),
            client_id: self.client_id,
            out_tx: self.out_tx.clone(),
            client_caps: self.selection.client_caps,
            profile: self.selection.profile,
            limits: self.selection.limits,
            input_lane: self.input_lane.cloned(),
            token: self.token.clone(),
            root_token: self.root_token.clone(),
            defer_subscription: self.selection.quic_streams(),
        }
    }

    /// Hold a command the guard held (ADR-0128). Its waiter runs it later
    /// in this connection's context, exactly as this dispatch would have.
    async fn hold(&mut self, request_id: u32, command: Command) {
        super::approvals::hold_command(self.context(), self.held_commands, request_id, command)
            .await;
    }

    async fn submit_input(&mut self, lane: &InputLaneHandle, request_id: u32, command: Command) {
        let Ok(slot) = self.input_receipt_slots.clone().try_acquire_owned() else {
            let _ = self
                .out_tx
                .send(Outbound::Frame(FrameKind::CommandResult {
                    request_id,
                    result: CommandResult::Error {
                        code: ErrorCode::ResourceExhausted,
                        message: "input completion capacity exhausted".to_owned(),
                    },
                }))
                .await;
            return;
        };
        let receipt = match command {
            Command::ApplyInput {
                operation_id,
                terminal_id,
                events,
            } => lane.begin_apply(
                self.state,
                self.client_id,
                operation_id,
                terminal_id,
                events,
            ),
            Command::RouteInput { terminal_id, event } => {
                // Same FIFO and the same wait as `INPUT_*` (ADR-0144).
                let Ok(credit) = self.input_credits.acquire(self.state, &terminal_id).await else {
                    let _ = self
                        .out_tx
                        .send(Outbound::Frame(FrameKind::CommandResult {
                            request_id,
                            result: InputStalled::result(&terminal_id),
                        }))
                        .await;
                    return;
                };
                lane.begin_route(self.state, self.client_id, terminal_id, event, credit)
            }
            _ => unreachable!("guarded input command"),
        };
        spawn_input_receipt(
            self.input_receipts,
            slot,
            self.out_tx.clone(),
            request_id,
            receipt,
        );
    }

    async fn submit_bulk(
        &self,
        request_id: u32,
        command: Command,
        retained: usize,
        command_started: std::time::Instant,
    ) {
        let ctx = self.context();
        let task = async move { run_handler(&ctx, request_id, command, command_started).await };
        if let Err(result) = self.command_tasks.try_submit(retained, task) {
            let _ = self
                .out_tx
                .send(Outbound::Frame(FrameKind::CommandResult {
                    request_id,
                    result,
                }))
                .await;
        }
    }
}

/// Run one admitted command through the handler.
async fn run_handler(
    ctx: &super::commands::CommandContext,
    request_id: u32,
    command: Command,
    started: std::time::Instant,
) {
    super::commands::handle_command(ctx, request_id, command).await;
    crate::perf::CMD_HANDLE.record_elapsed(started);
}

#[cfg(test)]
mod input_receipt_capacity_tests {
    use super::*;
    use phux_protocol::ids::InputOperationId;
    use phux_protocol::input::InputEvent;
    use phux_protocol::input::paste::{PasteEvent, PasteTrust};

    fn paste(data: &[u8]) -> InputEvent {
        InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Trusted,
            data: data.to_vec(),
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn receipt_refusal_precedes_operation_cache_admission() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let state = SharedState::new();
                let lane_owner = super::super::input_lane::spawn_input_lane(state.clone()).unwrap();
                let lane = lane_owner.handle();
                let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(1);
                let token = CancellationToken::new();
                let mut command_tasks =
                    super::super::command_tasks::CommandTasks::new(token.clone());
                let mut receipts = JoinSet::new();
                let slots = Arc::new(tokio::sync::Semaphore::new(0));
                let operation_id = InputOperationId::new([0x44; 16]).unwrap();
                CommandDispatch {
                    state: &state,
                    client_id: ClientId(1),
                    out_tx: &out_tx,
                    input_lane: Some(&lane),
                    token: &token,
                    root_token: &token,
                    selection: NegotiatedConnection {
                        client_caps: ClientCapabilities::default(),
                        profile: BootstrapProfile::SynthesizedVtRaw,
                        limits: BootstrapLimits::default(),
                        server_features: ServerFeatureSet::new(),
                        compression: Compression::None,
                    },
                    command_tasks: &mut command_tasks,
                    input_receipts: &mut receipts,
                    input_receipt_slots: &slots,
                    input_credits: &mut InputCredits::default(),
                    held_commands: &mut JoinSet::new(),
                }
                .submit_input(
                    &lane,
                    41,
                    Command::ApplyInput {
                        operation_id,
                        terminal_id: WireResourceId::local(1),
                        events: vec![paste(b"original")],
                    },
                )
                .await;
                assert!(matches!(
                    out_rx.recv().await,
                    Some(Outbound::Frame(FrameKind::CommandResult {
                        request_id: 41,
                        result: CommandResult::Error {
                            code: ErrorCode::ResourceExhausted,
                            ..
                        },
                    }))
                ));
                assert!(receipts.is_empty());
                // A different payload with the refused id must reach destination
                // validation, rather than conflict with a prematurely claimed id.
                let result = lane
                    .begin_apply(
                        &state,
                        ClientId(1),
                        operation_id,
                        WireResourceId::local(2),
                        vec![paste(b"replacement")],
                    )
                    .await;
                assert!(
                    matches!(
                        result,
                        CommandResult::Error {
                            code: ErrorCode::TerminalNotFound,
                            ..
                        }
                    ),
                    "{result:?}"
                );
                drop(lane);
                drop(lane_owner);
                command_tasks.shutdown().await;
                drop(command_tasks);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn aborted_blocked_receipt_releases_completion_capacity() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let state = SharedState::new();
                let lane_owner = super::super::input_lane::spawn_input_lane(state.clone()).unwrap();
                let lane = lane_owner.handle();
                let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(1);
                out_tx
                    .try_send(Outbound::Frame(FrameKind::Pong { nonce: 7 }))
                    .unwrap();
                let slots = Arc::new(tokio::sync::Semaphore::new(1));
                let receipt = lane.begin_apply(
                    &state,
                    ClientId(1),
                    InputOperationId::new([0x45; 16]).unwrap(),
                    WireResourceId::local(1),
                    vec![],
                );
                let mut receipts = JoinSet::new();
                spawn_input_receipt(
                    &mut receipts,
                    slots.clone().try_acquire_owned().unwrap(),
                    out_tx.clone(),
                    42,
                    receipt,
                );
                tokio::task::yield_now().await;
                assert_eq!(slots.available_permits(), 0);
                receipts.shutdown().await;
                assert_eq!(slots.available_permits(), 1);
                assert!(matches!(
                    out_rx.try_recv(),
                    Ok(Outbound::Frame(FrameKind::Pong { nonce: 7 }))
                ));
                assert!(
                    out_rx.try_recv().is_err(),
                    "aborted receipt must not publish later"
                );
                drop(lane);
                drop(lane_owner);
            })
            .await;
    }
}

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

#[derive(Debug, Clone, Copy)]
struct NegotiatedConnection {
    client_caps: ClientCapabilities,
    profile: BootstrapProfile,
    limits: BootstrapLimits,
    server_features: ServerFeatureSet,
    /// The frame compression HELLO selected (`docs/spec/proto.md` §6.4).
    /// [`Compression::None`] for every consumer that offered nothing, which
    /// is every local one.
    compression: Compression,
}

impl NegotiatedConnection {
    /// QUIC multi-stream: Terminal content rides per-Terminal streams, and
    /// subscriptions start at `STREAM_BIND`.
    const fn quic_streams(self) -> bool {
        self.server_features.contains(ServerFeature::QuicStreams)
    }

    const fn accepts_terminal_reply(self) -> bool {
        self.server_features.contains(ServerFeature::TerminalReply)
    }
}
const fn runtime_server_features() -> ServerFeatureSet {
    // QUIC_STREAMS is transport-gated at HELLO (see negotiate_hello); every
    // other known bit is advertised on every connection.
    ServerFeatureSet::all().without(ServerFeature::QuicStreams)
}

#[cfg(test)]
mod negotiated_feature_tests {
    use super::*;

    fn connection(server_features: ServerFeatureSet) -> NegotiatedConnection {
        NegotiatedConnection {
            client_caps: ClientCapabilities::default(),
            profile: BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
            server_features,
            compression: Compression::None,
        }
    }

    #[test]
    fn negotiated_resize_uses_the_terminal_stream() {
        let client_id = ClientId(1);
        let mut bytes = BytesMut::new();
        FrameKind::ResizeTerminal {
            terminal_id: WireResourceId::local(1),
            cols: 80,
            rows: 24,
            cell_px: None,
        }
        .encode(&mut bytes);
        let multistream = connection(ServerFeatureSet::with(&[ServerFeature::QuicStreams]));
        let legacy = connection(ServerFeatureSet::new());
        let validate = |selection, origin| {
            validate_dispatch_frame(&bytes, Some(selection), origin, client_id).is_ok()
        };
        assert!(!validate(&multistream, FrameOrigin::Control));
        assert!(validate(&multistream, FrameOrigin::Terminal));
        assert!(validate(&legacy, FrameOrigin::Control));
    }

    /// Before HELLO a frame is refused by its type byte, before its body is
    /// decoded: a peer that has not said HELLO never reaches the other body
    /// decoders (here a server-to-client snapshot declaring a huge list).
    #[test]
    fn pre_hello_frames_are_refused_before_body_decode() {
        let client_id = ClientId(1);
        let mut attached = BytesMut::new();
        attached.extend_from_slice(&6_u32.to_be_bytes());
        attached.extend_from_slice(&[phux_protocol::wire::frame::TYPE_ATTACHED, 1, 4, 4]);
        attached.extend_from_slice(&[0xFF, 0xFF]);
        let code = |bytes: &BytesMut| {
            validate_dispatch_frame(bytes, None, FrameOrigin::Control, client_id)
                .err()
                .map(|close| close.code)
        };
        assert_eq!(code(&attached), Some(ErrorCode::VersionIncompatible));

        let mut ping = BytesMut::new();
        FrameKind::Ping { nonce: 1 }.encode(&mut ping);
        assert_eq!(code(&ping), None, "PING stays legal before HELLO");
    }

    /// Every known bit but `QUIC_STREAMS` is advertised; an old peer without
    /// the `TERMINAL_REPLY` bit gets its replies refused.
    #[test]
    fn runtime_advertises_every_known_bit_except_quic_streams() {
        let advertised = runtime_server_features();
        assert!(!advertised.contains(ServerFeature::QuicStreams));
        assert_eq!(advertised.iter().count(), ServerFeature::ALL.len() - 1);
        assert!(connection(advertised).accepts_terminal_reply());
        assert!(!connection(ServerFeatureSet::new()).accepts_terminal_reply());
    }

    /// ADR-0115: `QUIC_STREAMS` is advertised only on QUIC, and only to a
    /// client that opted in.
    #[tokio::test]
    async fn quic_streams_bit_advertised_on_quic_only() {
        async fn negotiate(transport: TransportType, quic_streams: bool) -> bool {
            let state = SharedState::new();
            let client_id = state.with_mut(crate::state::ServerState::new_client_id);
            state.with_mut(|s| {
                s.set_connection_identity(
                    client_id,
                    crate::auth::ConnectionIdentity {
                        peer: phux_protocol::policy::PeerIdentity {
                            uid: 0,
                            pid: None,
                            exe_path: None,
                            mcp_host_key: None,
                            transport,
                            source_addr: None,
                        },
                        credential: None,
                        ssh_origin: None,
                        bearer: None,
                    },
                );
            });
            let (out_tx, _rx) = tokio::sync::mpsc::channel(8);
            let mut negotiated = None;
            negotiate_hello(
                &state,
                client_id,
                &out_tx,
                HelloRequest {
                    client_name: "test".to_owned(),
                    protocol_major: PROTOCOL_VERSION.major,
                    protocol_minor: PROTOCOL_VERSION.minor,
                    protocol_patch: PROTOCOL_VERSION.patch,
                    client_caps: ClientCapabilities::default().with_quic_streams(quic_streams),
                },
                &mut negotiated,
                transport,
                matches!(transport, TransportType::Quic),
            )
            .await
            .map_err(|close| close.message)
            .expect("compatible HELLO negotiates");
            negotiated
                .expect("negotiation caches the selection")
                .server_features
                .contains(ServerFeature::QuicStreams)
        }

        assert!(negotiate(TransportType::Quic, true).await);
        assert!(!negotiate(TransportType::Quic, false).await);
        for transport in [
            TransportType::UnixSocket,
            TransportType::SshTunnel,
            TransportType::WebSocket,
            TransportType::WebTransport,
        ] {
            assert!(!negotiate(transport, true).await, "{transport:?}");
        }
    }
}

/// Attachment teardown: free the per-consumer state every subscribed pane
/// holds for this client (ADR-0018), release its leases and relay state,
/// and detach it. The connection itself stays open; HELLO-negotiated state
/// is left to [`release_connection_state`]. A dropped `consumer_detach`
/// is self-healing: the actor reaps the entry once the mailbox closes.
pub(crate) fn detach_and_release_consumer_state(state: &SharedState, client_id: ClientId) {
    // Captured before teardown, for the `client-detached` hook.
    let attached_session = state.with(|s| attached_session_name(s, client_id));
    state.with(|s| release_actor_consumers(s, client_id));
    // The `Released` transitions name this client as their actor.
    state.with(|s| announce_lease_releases(s, client_id, Some(super::wire_client(client_id))));
    state.with(|s| release_relay_state(s, client_id));
    state.with_mut(|s| s.detach(client_id));
    fire_client_detached(state, client_id, attached_session);
}

/// `workload-auth.md` §7 step 2: [`detach_and_release_consumer_state`] in
/// the caller's lock, so nothing outlives the revocation. Returns the
/// attached session for [`fire_client_detached`], run off-lock.
pub(crate) fn release_revoked_consumer_state(
    s: &mut ServerState,
    client_id: ClientId,
) -> Option<DetachedFrom> {
    let attached_session = attached_session_name(s, client_id);
    // A revoked connection's held actions are withdrawn in the same
    // critical section: nothing it held can be approved (ADR-0128).
    let _ = s.withdraw_approvals(client_id);
    release_actor_consumers(s, client_id);
    announce_lease_releases(s, client_id, None);
    release_relay_state(s, client_id);
    s.detach(client_id);
    attached_session
}

/// Fire the `client-detached` hook for a client that was attached.
pub(crate) fn fire_client_detached(
    state: &SharedState,
    client_id: ClientId,
    detached_from: Option<DetachedFrom>,
) {
    if let Some(from) = detached_from {
        crate::hooks::fire_hook(
            state,
            HookEvent::client_detached(client_id, from.session_name.as_deref()),
        );
    }
}

/// The session an attached client was attached to, captured before its
/// teardown for the `client-detached` hook.
pub(crate) struct DetachedFrom {
    /// The session's name; `None` once the session itself is gone.
    session_name: Option<String>,
}

/// Where the client is attached, or `None` when it never attached.
fn attached_session_name(s: &ServerState, client_id: ClientId) -> Option<DetachedFrom> {
    s.attached().get(&client_id).map(|client| DetachedFrom {
        session_name: s
            .registry()
            .session(client.session)
            .map(|session| session.name.clone()),
    })
}

/// Free the per-consumer state-sync entries every pane this client
/// subscribes to allocated for it. `try_send` is non-blocking and
/// best-effort, so this is safe under the state lock.
fn release_actor_consumers(s: &ServerState, client_id: ClientId) {
    let wire_client_id = super::wire_client(client_id);
    for handle in s.subscribed_resource_handles(client_id) {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if let Ok(terminal) = handle.terminal() {
            let _ = terminal
                .native_release
                .try_send(crate::terminal_actor::NativeReleaseRequest { owner: client_id.0 });
        }
        let (reply_tx, _reply_rx) = oneshot::channel();
        match handle.consumer_detach.try_send(ConsumerDetachRequest {
            client_id: wire_client_id,
            reply: reply_tx,
        }) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                trace!(
                    ?client_id,
                    "consumer_detach mailbox full; entry reaped by tick_emit when its mailbox closes",
                );
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                trace!(
                    ?client_id,
                    "consumer_detach: pane actor gone; nothing to free"
                );
            }
        }
    }
}

/// Announce the `Released` transition for every input lease this client
/// holds (ADR-0033). `actor` is `None` when the server took it back.
/// Event-only: `detach` clears the lease state regardless.
fn announce_lease_releases(
    s: &ServerState,
    client_id: ClientId,
    actor: Option<phux_protocol::ids::ClientId>,
) {
    for pane in s.leases_held_by(client_id) {
        let Some(handle) = s.resource_handle(pane) else {
            continue;
        };
        let _ = handle
            .control
            .try_send(crate::terminal_actor::ControlRequest::LeaseChanged {
                input_holder: None,
                action: phux_protocol::wire::frame::ControlAction::Released,
                actor,
            });
    }
}

/// Drop this client's federation relay state. Empty (no-op) on a non-hub
/// server.
fn release_relay_state(s: &ServerState, client_id: ClientId) {
    // Rides an unbounded channel, so a saturated relay cannot keep a stale
    // subscriber alive.
    for relay in s.hub_relays_all() {
        relay.unsubscribe_client(client_id);
    }
    // Relay a RELEASE_INPUT per satellite lease this client held.
    for (host, terminal) in s.satellite_leases_held_by(client_id) {
        if let Some(relay) = s.hub_relay(&host) {
            relay.command_detached(phux_protocol::wire::frame::Command::ReleaseInput {
                terminal_id: WireResourceId::local(terminal),
            });
        }
    }
}

/// Transport-close teardown: [`detach_and_release_consumer_state`] plus the
/// connection-scoped state HELLO negotiated. Only the accept loop calls it:
/// a `DETACH` keeps the connection, its layers, and its peer identity.
pub(crate) fn release_connection_state(state: &SharedState, client_id: ClientId) {
    // A closing connection withdraws the actions it holds (ADR-0128).
    super::approvals::withdraw(state, client_id);
    detach_and_release_consumer_state(state, client_id);
    state.with_mut(|s| s.forget_connection(client_id));
}

/// A listener's live-connection cap ([`Incoming::max_connections`]), counted
/// on the accept loop's thread.
struct ConnectionCap {
    cap: Option<usize>,
    live: std::rc::Rc<std::cell::Cell<usize>>,
    refusals: crate::transport::RefusalWarnings,
}

/// One connection's place under a [`ConnectionCap`], returned when its task
/// ends.
struct LiveConnection(std::rc::Rc<std::cell::Cell<usize>>);

impl Drop for LiveConnection {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

impl ConnectionCap {
    fn new(cap: Option<usize>) -> Self {
        Self {
            cap,
            live: std::rc::Rc::default(),
            refusals: crate::transport::RefusalWarnings::new(),
        }
    }

    /// A place for one more connection, or `None` (logged, at a bounded
    /// rate) when the listener is at its cap.
    fn admit(&self, transport: &'static str) -> Option<LiveConnection> {
        if let Some(cap) = self.cap
            && self.live.get() >= cap
        {
            if let Some(suppressed) = self.refusals.due() {
                warn!(
                    transport,
                    cap, suppressed, "connection refused: listener at its connection cap"
                );
            } else {
                debug!(
                    transport,
                    cap, "connection refused: listener at its connection cap"
                );
            }
            return None;
        }
        self.live.set(self.live.get() + 1);
        Some(LiveConnection(std::rc::Rc::clone(&self.live)))
    }
}

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

/// The wire carries one concrete version rather than a range. A patch release
/// is editorial/behavior-preserving, while a major or minor change may alter
/// the wire contract; therefore only `major.minor` must match.
const fn protocol_is_compatible(client_major: u16, client_minor: u16) -> bool {
    client_major == PROTOCOL_VERSION.major && client_minor == PROTOCOL_VERSION.minor
}

fn incompatible_protocol_message(
    client_major: u16,
    client_minor: u16,
    client_patch: u16,
) -> String {
    let client = (client_major, client_minor, client_patch);
    let server = (
        PROTOCOL_VERSION.major,
        PROTOCOL_VERSION.minor,
        PROTOCOL_VERSION.patch,
    );
    let remediation = if client < server {
        "update the phux app/client"
    } else {
        "update the phux server"
    };
    format!(
        "incompatible protocol: client offered {client_major}.{client_minor}.{client_patch}, \
         server requires {}.{}.x; {remediation} so protocol major.minor match",
        PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor,
    )
}

const WRITER_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
/// Bound the server-wide wait for clients to flush their shutdown ending.
const CLIENT_SHUTDOWN_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

async fn close_client_writer(
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
fn framing_violation(err: &io::Error) -> Option<FramingError> {
    err.get_ref()
        .and_then(|source| source.downcast_ref::<FramingError>())
        .copied()
}

/// SPEC §7.4: echo the nonce in PONG.
async fn reply_pong(out_tx: &mpsc::Sender<Outbound>, client_id: ClientId, nonce: u64) {
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
async fn detach_on_request(
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
enum AttachIdVerdict {
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
fn classify_attach_id(
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
async fn dispatch_terminal_reply(
    client_id: ClientId,
    selection: NegotiatedConnection,
    terminal_id: &WireResourceId,
    bytes: bytes::Bytes,
    out_tx: &mpsc::Sender<Outbound>,
) {
    if !selection.accepts_terminal_reply() {
        super::send_error(
            out_tx,
            ErrorCode::UnknownMessageType,
            "INPUT_TERMINAL_REPLY was not advertised for this connection",
        )
        .await;
        return;
    }
    handle_terminal_reply(client_id, terminal_id, &bytes);
}

/// Why one connection must end, and what the peer is owed on the way out
/// (SPEC §9: `ERROR`, then `DETACHED`, then close).
struct ConnectionClose {
    /// `Some(reason)` when the connection may be attached, so its pumps and
    /// consumer state are torn down first; `None` in the handshake phase.
    attached_reason: Option<&'static str>,
    /// The `DETACHED` reason: `PROTOCOL_ERROR` for a violation,
    /// `AUTHENTICATION_FAILED` when HELLO's authentication outcome refused
    /// the peer (workload-auth §7).
    detach_reason: DetachReason,
    code: ErrorCode,
    message: String,
}

/// One bound Terminal stream's outbound half (proto.md §4.2): a mailbox
/// drained by the same writer task every transport uses. Retirement closes
/// admission, then drains with a bound.
struct StreamBinding {
    tx: mpsc::Sender<Outbound>,
    stream_id: StreamId,
    writer: JoinSet<()>,
    writer_close: tokio::sync::watch::Sender<bool>,
    ingress_active: Arc<std::sync::atomic::AtomicBool>,
    ingress_cancel: CancellationToken,
    diagnostics: Option<crate::stream_diagnostics::StreamTracker>,
}

impl StreamBinding {
    fn begin_retirement(&self) {
        if let Some(tracker) = &self.diagnostics {
            tracker.set_active(false);
        }
        self.ingress_active.store(false, Ordering::Release);
        self.ingress_cancel.cancel();
        let _ = self.writer_close.send(true);
    }

    async fn retire(&mut self) {
        self.begin_retirement();
        if tokio::time::timeout(WRITER_DRAIN_TIMEOUT, async {
            while self.writer.join_next().await.is_some() {}
        })
        .await
        .is_err()
        {
            self.writer.shutdown().await;
        }
    }
}

impl Drop for StreamBinding {
    fn drop(&mut self) {
        self.ingress_active.store(false, Ordering::Release);
        self.ingress_cancel.cancel();
    }
}

/// One connection's outbound plumbing. The output pumps are separate from
/// the writer so DETACH can abort them while the writer still emits
/// DETACHED and serves a later ATTACH.
struct ClientPlumbing {
    out_tx: mpsc::Sender<Outbound>,
    writer_close: tokio::sync::watch::Sender<bool>,
    /// Sibling tasks (today: just the writer).
    sibling_tasks: JoinSet<()>,
    /// Per-attach raw-output pumps.
    output_pumps: JoinSet<()>,
    /// Bound Terminal streams (QUIC multi-stream only): terminal → binding.
    /// Empty on every other transport, where all frames share `out_tx`.
    stream_bindings: HashMap<WireResourceId, StreamBinding>,
    retired_streams: JoinSet<()>,
    /// The frame compression the writer applies, published by HELLO after
    /// the writer is already running.
    compression: Arc<AtomicU8>,
    /// The connection's revocation signal, handed to every writer it spawns.
    revocation: tokio::sync::watch::Receiver<Option<Goodbye>>,
}

impl ClientPlumbing {
    /// Allocate the per-client outbound mailbox and spawn its writer task.
    fn spawn<W: FrameWriter + 'static>(
        writer: W,
        client_id: ClientId,
        revocation: tokio::sync::watch::Receiver<Option<Goodbye>>,
    ) -> Self {
        let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
        let (writer_close, writer_close_rx) = tokio::sync::watch::channel(false);
        let mut sibling_tasks: JoinSet<()> = JoinSet::new();
        let compression = Arc::new(AtomicU8::new(Compression::None.as_u8()));
        sibling_tasks.spawn_local(writer_task(
            writer,
            out_rx,
            writer_close_rx,
            Arc::clone(&compression),
            client_id,
            RevocationWatch::control(revocation.clone()),
        ));
        Self {
            out_tx,
            writer_close,
            sibling_tasks,
            output_pumps: JoinSet::new(),
            stream_bindings: HashMap::new(),
            retired_streams: JoinSet::new(),
            compression,
            revocation,
        }
    }

    /// Publish the compression HELLO selected to the writer task. Called
    /// after `HELLO_OK` is queued, so that frame is never compressed.
    fn set_compression(&self, compression: Compression) {
        self.compression
            .store(compression.as_u8(), Ordering::Relaxed);
    }

    /// The mailbox for Terminal-addressed outbound: the bound stream's sender
    /// when this connection bound the Terminal, else the control mailbox.
    fn sender_for(&self, terminal_id: &WireResourceId) -> mpsc::Sender<Outbound> {
        self.stream_bindings
            .get(terminal_id)
            .map_or_else(|| self.out_tx.clone(), |binding| binding.tx.clone())
    }

    /// Bind a Terminal stream: create its mailbox, spawn its writer task
    /// draining into the stream's send half, and register both. Replaces any
    /// live binding for the Terminal, reaping its superseded writer.
    async fn bind_stream(
        &mut self,
        client_id: ClientId,
        terminal_id: WireResourceId,
        stream_id: StreamId,
        writer: QuicWriter,
    ) -> (
        mpsc::Sender<Outbound>,
        Arc<std::sync::atomic::AtomicBool>,
        CancellationToken,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
        let (writer_close, writer_close_rx) = tokio::sync::watch::channel(false);
        let mut writer_tasks: JoinSet<()> = JoinSet::new();
        let diagnostics = Some(writer.diagnostic_tracker());
        writer_tasks.spawn_local(writer_task(
            writer,
            rx,
            writer_close_rx,
            Arc::clone(&self.compression),
            client_id,
            RevocationWatch::stream(self.revocation.clone()),
        ));
        let sender = tx.clone();
        let ingress_active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ingress_cancel = CancellationToken::new();
        if let Some(replaced) = self.stream_bindings.insert(
            terminal_id,
            StreamBinding {
                tx,
                stream_id,
                writer: writer_tasks,
                writer_close,
                ingress_active: Arc::clone(&ingress_active),
                ingress_cancel: ingress_cancel.clone(),
                diagnostics,
            },
        ) {
            self.retire_stream(replaced).await;
        }
        (sender, ingress_active, ingress_cancel)
    }

    /// Whether `stream_id` is the Terminal's live binding, not a stale one.
    fn is_current_stream(&self, terminal_id: &WireResourceId, stream_id: StreamId) -> bool {
        self.stream_bindings
            .get(terminal_id)
            .is_some_and(|binding| binding.stream_id == stream_id)
    }

    /// Drop one stream binding; its retirement runs without blocking others.
    async fn drop_stream_binding(&mut self, terminal_id: &WireResourceId) {
        if let Some(binding) = self.stream_bindings.remove(terminal_id) {
            self.retire_stream(binding).await;
        }
    }

    async fn retire_stream(&mut self, mut binding: StreamBinding) {
        // Bound the draining writers churn can retain.
        binding.begin_retirement();
        while self.retired_streams.try_join_next().is_some() {}
        if self.retired_streams.len() >= 127 {
            self.retired_streams.shutdown().await;
        }
        self.retired_streams
            .spawn_local(async move { binding.retire().await });
    }

    /// Drop every stream binding (session DETACH, disconnect). Pumps are
    /// aborted by the caller first; draining writers remain connection-owned.
    async fn drop_all_stream_bindings(&mut self) {
        for (_, binding) in std::mem::take(&mut self.stream_bindings) {
            self.retire_stream(binding).await;
        }
    }

    /// Close the connection as its frame loop `ending` requires.
    async fn end(
        self,
        ending: ConnectionEnding,
        state: &SharedState,
        client_id: ClientId,
        root_token: &CancellationToken,
    ) {
        match ending {
            ConnectionEnding::TransportGone => {}
            ConnectionEnding::Violation(close) => self.close(close, state, client_id).await,
            ConnectionEnding::Cancelled => {
                self.close_for_cancellation(state, client_id, root_token)
                    .await;
            }
        }
    }

    /// End one connection for a protocol violation, in the order §9
    /// requires: tear down what may be attached, then `ERROR`, `DETACHED`,
    /// and close.
    async fn close(mut self, close: ConnectionClose, state: &SharedState, client_id: ClientId) {
        self.drop_all_stream_bindings().await;
        while self.retired_streams.join_next().await.is_some() {}
        if let Some(reason) = close.attached_reason {
            abort_output_pumps(&mut self.output_pumps, client_id, reason).await;
            detach_and_release_consumer_state(state, client_id);
        }
        let ConnectionClose {
            detach_reason,
            code,
            message,
            ..
        } = close;
        let goodbye = [
            FrameKind::Error {
                request_id: None,
                code,
                message: message.clone(),
            },
            FrameKind::Detached {
                reason: Some(detach_reason),
                message,
            },
        ];
        for frame in goodbye {
            let _ = self.out_tx.send(Outbound::Frame(frame)).await;
        }
        close_client_writer(self.out_tx, &self.writer_close, &mut self.sibling_tasks).await;
    }

    /// End one connection because it was cancelled rather than because the
    /// peer misbehaved: no `ERROR` is owed, and a server-wide shutdown says so
    /// in the `DETACHED` reason before the writer drains.
    async fn close_for_cancellation(
        mut self,
        state: &SharedState,
        client_id: ClientId,
        root_token: &CancellationToken,
    ) {
        abort_output_pumps(&mut self.output_pumps, client_id, "connection cancellation").await;
        self.drop_all_stream_bindings().await;
        while self.retired_streams.join_next().await.is_some() {}
        if root_token.is_cancelled() {
            let _ = self
                .out_tx
                .send(Outbound::Frame(FrameKind::Detached {
                    reason: Some(DetachReason::ServerShutdown),
                    message: "server is shutting down".to_owned(),
                }))
                .await;
        }
        detach_and_release_consumer_state(state, client_id);
        close_client_writer(self.out_tx, &self.writer_close, &mut self.sibling_tasks).await;
    }
}

/// SPEC §5: a framing violation is answered with `FRAME_TOO_LARGE` before
/// closing. `None` for every other read error: the transport died.
fn framing_violation_close(err: &io::Error, client_id: ClientId) -> Option<ConnectionClose> {
    let framing = framing_violation(err)?;
    warn!(?client_id, error = %framing, "client framing violation; closing");
    Some(ConnectionClose {
        attached_reason: Some("framing violation"),
        detach_reason: DetachReason::ProtocolError,
        code: ErrorCode::FrameTooLarge,
        message: framing.wire_message(),
    })
}

/// Decode one framed message. Once HELLO has been negotiated the connection's
/// selected limits apply, so an oversized borrowed payload is rejected before
/// the decoder copies it into owned storage.
fn decode_client_frame(
    framed: &BytesMut,
    negotiated: Option<&NegotiatedConnection>,
) -> Result<FrameKind, ConnectionClose> {
    // FRAME_COMPRESSED is server-to-client only (proto.md §6.4); refuse the
    // envelope before decode. The type byte follows the u32 length header.
    if framed.get(4) == Some(&phux_protocol::wire::frame::TYPE_FRAME_COMPRESSED) {
        warn!("client sent FRAME_COMPRESSED; closing");
        return Err(ConnectionClose {
            attached_reason: Some("client-sent FRAME_COMPRESSED"),
            detach_reason: DetachReason::ProtocolError,
            code: ErrorCode::MalformedMessage,
            message: "FRAME_COMPRESSED is server-to-client only".to_owned(),
        });
    }
    let decoded = negotiated.map_or_else(
        || FrameKind::decode(framed),
        |selection| FrameKind::decode_with_limits(framed, selection.limits),
    );
    match decoded {
        Ok((frame, _rest)) => Ok(frame),
        Err(err) => {
            warn!(error = ?err, "client sent undecodable frame; closing");
            Err(ConnectionClose {
                attached_reason: Some("undecodable frame"),
                detach_reason: DetachReason::ProtocolError,
                code: ErrorCode::MalformedMessage,
                message: format!("could not decode client frame: {err:?}"),
            })
        }
    }
}

/// Nothing stateful may precede HELLO; PING is a stateless liveness probe.
fn reject_frame_before_hello(
    frame: &FrameKind,
    negotiated: bool,
    client_id: ClientId,
) -> Option<ConnectionClose> {
    if negotiated || matches!(frame, FrameKind::Hello { .. } | FrameKind::Ping { .. }) {
        return None;
    }
    Some(before_hello_close(client_id))
}

/// Whether a framed message's type byte is one `PRE_HELLO` admits.
fn is_pre_hello_type(framed: &[u8]) -> bool {
    use phux_protocol::wire::frame::{TYPE_HELLO, TYPE_PING};
    matches!(
        framed.get(phux_protocol::wire::LENGTH_PREFIX_LEN),
        Some(&(TYPE_HELLO | TYPE_PING))
    )
}

fn before_hello_close(client_id: ClientId) -> ConnectionClose {
    warn!(?client_id, "stateful frame before HELLO; closing");
    ConnectionClose {
        attached_reason: None,
        detach_reason: DetachReason::ProtocolError,
        code: ErrorCode::VersionIncompatible,
        message: "HELLO required before any stateful frame".to_owned(),
    }
}

/// The HELLO frame's payload.
struct HelloRequest {
    client_name: String,
    protocol_major: u16,
    protocol_minor: u16,
    protocol_patch: u16,
    client_caps: ClientCapabilities,
}

/// Negotiate the connection: version compatibility, the policy engine's
/// verdict on the authenticated peer, and the bootstrap profile both sides can
/// speak. All of it is cached in `negotiated` exactly once — before any
/// stateful frame can be processed — and acknowledged with `HELLO_OK`.
async fn negotiate_hello(
    state: &SharedState,
    client_id: ClientId,
    out_tx: &mpsc::Sender<Outbound>,
    hello: HelloRequest,
    negotiated: &mut Option<NegotiatedConnection>,
    transport: TransportType,
    route_supports_quic_streams: bool,
) -> Result<(), ConnectionClose> {
    let HelloRequest {
        client_name,
        protocol_major,
        protocol_minor,
        protocol_patch,
        client_caps,
    } = hello;
    if negotiated.is_some() {
        warn!(?client_id, "duplicate HELLO; closing");
        return Err(ConnectionClose {
            attached_reason: Some("duplicate HELLO"),
            detach_reason: DetachReason::ProtocolError,
            code: ErrorCode::InvalidCommand,
            message: "HELLO already completed on this connection".to_owned(),
        });
    }
    debug!(
        ?client_id,
        %client_name,
        protocol_major,
        protocol_minor,
        protocol_patch,
        color_support = ?client_caps.color_support,
        "HELLO",
    );
    if !protocol_is_compatible(protocol_major, protocol_minor) {
        let message = incompatible_protocol_message(protocol_major, protocol_minor, protocol_patch);
        warn!(?client_id, %message, "HELLO protocol mismatch");
        return Err(ConnectionClose {
            attached_reason: None,
            detach_reason: DetachReason::ProtocolError,
            code: ErrorCode::VersionIncompatible,
            message,
        });
    }
    authorize_hello(state, client_id).await?;
    // After authorization, so the announcement cannot influence it.
    state.with_mut(|s| super::whoami::admit_ssh_origin(s, client_id, client_caps.ssh_origin));
    let (selected_profile, bootstrap_limits) = select_hello_profile(&client_caps, client_id)?;

    let mut effective_client_caps = client_caps;
    effective_client_caps.output_mode =
        if matches!(selected_profile, BootstrapProfile::SynthesizedVtStateSync) {
            phux_protocol::caps::OutputMode::StateSync
        } else {
            phux_protocol::caps::OutputMode::Raw
        };
    let mut server_features = runtime_server_features();
    // ADR-0115: advertised only where streams exist to back it.
    if matches!(transport, TransportType::Quic)
        && route_supports_quic_streams
        && client_caps.quic_streams
    {
        server_features = ServerFeatureSet::from_wire(server_features.as_wire() | QUIC_STREAMS);
    }
    // Never compress toward a consumer that did not offer to inflate.
    let compression = client_caps.compression.select();

    // Cached once, before any stateful frame; decoding uses these limits.
    *negotiated = Some(NegotiatedConnection {
        client_caps: effective_client_caps,
        profile: selected_profile,
        limits: bootstrap_limits,
        server_features,
        compression,
    });
    state.with_mut(|s| {
        // SPEC §6.2: the L3 arms gate on the negotiated layer set.
        s.set_client_layers(client_id, client_caps.layers);
        // ADR-0123: an actor label, not an authenticated fact.
        s.set_client_name(client_id, client_name);
    });
    let hello_ok = FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new()
            .with_layers(LayerSet::all())
            .with_features(server_features)
            .with_features_ext(ServerFeatureExtSet::with(&[
                ServerFeatureExt::PathQuery,
                ServerFeatureExt::ResizeCellPx,
                ServerFeatureExt::CommittedTeardown,
                ServerFeatureExt::AgentStreamResume,
                // ADR-0161: every organization command commits and is enforced.
                ServerFeatureExt::Organization,
                ServerFeatureExt::SessionKill,
                ServerFeatureExt::NativeAgent,
            ]))
            .with_compression(compression),
        server_id: state.with(|server| server.server_incarnation().as_bytes().to_vec()),
        selected_profile,
        bootstrap_limits,
    };
    if out_tx.send(Outbound::Frame(hello_ok)).await.is_err() {
        trace!(?client_id, "HELLO_OK send dropped: writer gone");
    }
    Ok(())
}

/// Policy check: authorize HELLO only against the identity authenticated by
/// the accepting transport. A missing registry entry is never equivalent to a
/// local root peer. Every refusal here is HELLO's authentication outcome, so
/// the peer gets `ERROR { PERMISSION_DENIED }` then `DETACHED {
/// AUTHENTICATION_FAILED }` (workload-auth §7).
async fn authorize_hello(state: &SharedState, client_id: ClientId) -> Result<(), ConnectionClose> {
    let identity = state.with(|s| {
        let peer = s.peer_identity(client_id).cloned()?;
        Some((peer, s.authenticated_credential(client_id).cloned()))
    });
    let Some((peer, credential)) = identity else {
        warn!(
            ?client_id,
            "HELLO denied: authenticated peer identity missing"
        );
        return Err(ConnectionClose {
            attached_reason: None,
            detach_reason: DetachReason::AuthenticationFailed,
            code: ErrorCode::PermissionDenied,
            message: "authenticated peer identity missing".to_owned(),
        });
    };
    // A bearer revoked or expired since its upgrade mints nothing
    // (workload-auth §7).
    let bearer = state.with(|s| s.bearer_admission(client_id).cloned());
    if bearer.is_some_and(|bearer| !matches!(bearer.standing(), Some(Standing::Active { .. }))) {
        warn!(
            ?client_id,
            "HELLO denied: the admitting bearer no longer stands"
        );
        return Err(ConnectionClose {
            attached_reason: None,
            detach_reason: DetachReason::AuthenticationFailed,
            code: ErrorCode::PermissionDenied,
            message: "policy denied: unauthorized: not authorized".to_owned(),
        });
    }
    // ADR-0163: a connection that arrived on a delegated credential's own
    // socket acts as that credential, never as the owner the socket's uid
    // would otherwise make it.
    if let Some(id) = super::delegation::delegated_credential(&peer, credential.as_ref()) {
        let grant = state.with(|s| s.delegated_grant(id, chrono::Utc::now()));
        let Some(grant) = grant else {
            warn!(
                ?client_id,
                "HELLO denied: the delegated credential is withdrawn or expired"
            );
            return Err(ConnectionClose {
                attached_reason: None,
                detach_reason: DetachReason::AuthenticationFailed,
                code: ErrorCode::PermissionDenied,
                message: "policy denied: unauthorized: not authorized".to_owned(),
            });
        };
        state.with_mut(|s| s.set_connection_grant(client_id, grant));
        return Ok(());
    }
    let engine = state.with(|s| s.policy_engine().clone());
    // workload-auth §5: the grant comes from the verified identity alone.
    match engine.authorize_hello(&peer, credential.as_ref()).await {
        Ok(grant) => {
            state.with_mut(|s| s.set_connection_grant(client_id, grant));
            Ok(())
        }
        Err(err) => {
            warn!(?client_id, error = %err, "HELLO denied by policy");
            Err(ConnectionClose {
                attached_reason: None,
                detach_reason: DetachReason::AuthenticationFailed,
                code: ErrorCode::PermissionDenied,
                message: format!("policy denied: {err}"),
            })
        }
    }
}

/// Select the protocol-0.7 bootstrap profile both peers can speak. `NativeState`
/// requires an exact common codec and every required engine feature, so no
/// common profile is a connection-ending `CodecUnavailable`.
fn select_hello_profile(
    client_caps: &ClientCapabilities,
    client_id: ClientId,
) -> Result<(BootstrapProfile, BootstrapLimits), ConnectionClose> {
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    let server_bootstrap = crate::native_state::native_bootstrap_capabilities();
    #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
    let server_bootstrap = BootstrapCapabilities::new();
    let Ok(selection) = select_bootstrap_profile(client_caps, &server_bootstrap) else {
        let message = format!(
            "no common protocol-0.7 bootstrap profile: client profiles=0x{:02x} native_codecs=0x{:016x} native_features=0x{:08x}; server profiles=0x{:02x} native_codecs=0x{:016x} native_features=0x{:08x}. NativeState requires an exact common codec and every required engine feature; advertise SynthesizedVtRaw/SynthesizedVtStateSync or update the incompatible peer",
            client_caps.bootstrap.profiles.as_wire(),
            client_caps.bootstrap.native_codecs.as_wire(),
            client_caps.bootstrap.native_features.as_wire(),
            server_bootstrap.profiles.as_wire(),
            server_bootstrap.native_codecs.as_wire(),
            server_bootstrap.native_features.as_wire(),
        );
        warn!(?client_id, %message, "HELLO codec unavailable");
        return Err(ConnectionClose {
            attached_reason: None,
            detach_reason: DetachReason::ProtocolError,
            code: ErrorCode::CodecUnavailable,
            message,
        });
    };
    Ok(selection)
}

/// The `HISTORY_REQUEST` frame's payload.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
struct HistoryPageRequest {
    terminal_id: WireResourceId,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    cursor: bytes::Bytes,
    max_bytes: u32,
    max_rows: u32,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
/// The Terminal facet a `HISTORY_REQUEST` pages against: `None` for an
/// unknown terminal or a resource of another kind.
fn history_terminal(
    state: &SharedState,
    terminal_id: &WireResourceId,
) -> Option<crate::terminal_actor::TerminalHandle> {
    let handle = state.with(|server| {
        server
            .terminal_from_wire(terminal_id)
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
async fn serve_history_request(
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
        .send(crate::terminal_actor::NativeHistoryRequest {
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
                crate::native_state::NativeStateError::OutOfMemory
                | crate::native_state::NativeStateError::OutOfSpace { .. }
                | crate::native_state::NativeStateError::LimitExceeded => {
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

        let frame =
            match validate_dispatch_frame(&framed, negotiated.as_ref(), frame_origin, client_id) {
                Ok(frame) => frame,
                Err(close) => break 'conn ConnectionEnding::Violation(close),
            };

        // workload-auth §6: the frame guard, before any routing or handler.
        if super::dispatch_guard::refuse_frame(
            &state,
            client_id,
            &frame,
            negotiated.is_some(),
            &plumbing.out_tx,
        )
        .await
        {
            continue;
        }

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

    /// HELLO: negotiate the connection, then apply what it selected.
    async fn hello(self, hello: HelloRequest) -> FrameFlow {
        if let Err(close) = negotiate_hello(
            self.state,
            self.client_id,
            &self.plumbing.out_tx,
            hello,
            self.negotiated,
            self.transport,
            self.route_supports_quic_streams,
        )
        .await
        {
            return FrameFlow::Break(ConnectionEnding::Violation(close));
        }
        if let Some(selection) = self.negotiated.as_ref() {
            self.plumbing.set_compression(selection.compression);
            // The client learns the bit from HELLO_OK, so no Terminal
            // stream can have opened before this.
            if selection.quic_streams() {
                *self.stream_events = self.reader.take_stream_events();
            }
        }
        FrameFlow::Continue(())
    }

    /// ATTACH: vet the attach id, then attach under the negotiated
    /// bootstrap selection.
    async fn attach(self, request: AttachFrame) -> FrameFlow {
        let attach_id = request.attach_id;
        match classify_attach_id(attach_id, self.used_attach_ids, self.client_id) {
            AttachIdVerdict::Fresh => {}
            AttachIdVerdict::Reused => {
                super::send_error(
                    &self.plumbing.out_tx,
                    ErrorCode::MalformedMessage,
                    &format!("ATTACH attach_id {attach_id} was already used on this connection"),
                )
                .await;
                return FrameFlow::Continue(());
            }
            AttachIdVerdict::Reserved => {
                return FrameFlow::Break(ConnectionEnding::Violation(ConnectionClose {
                    attached_reason: Some("zero ATTACH id"),
                    detach_reason: DetachReason::ProtocolError,
                    code: ErrorCode::MalformedMessage,
                    message: "ATTACH attach_id must be nonzero".to_owned(),
                }));
            }
        }
        let Some(selection) = self.negotiated.as_ref() else {
            return FrameFlow::Continue(());
        };
        debug!(
            client_id = ?self.client_id,
            attach_id,
            profile = ?selection.profile,
            chunk_limit = selection.limits.max_chunk_bytes(),
            history_page_limit = selection.limits.max_history_page_bytes(),
            "ATTACH with immutable bootstrap selection",
        );
        let attach_started = std::time::Instant::now();
        // QUIC multi-stream: content streams start at STREAM_BIND.
        let defer_subscription = selection.quic_streams();
        handle_attach(
            self.state,
            self.client_id,
            attach_id,
            request.target,
            request.viewport,
            request.request_scrollback,
            request.scrollback_limit_lines,
            request.role_policy,
            &self.plumbing.out_tx,
            selection.client_caps,
            selection.profile,
            selection.limits,
            self.root_token,
            &mut self.plumbing.output_pumps,
            self.token,
            defer_subscription,
        )
        .await;
        crate::perf::ATTACH_HANDLE.record_elapsed(attach_started);
        FrameFlow::Continue(())
    }

    /// Session DETACH: detach, and end every Terminal stream too.
    async fn detach(self) {
        detach_on_request(
            self.state,
            self.client_id,
            &self.plumbing.out_tx,
            &mut self.plumbing.output_pumps,
        )
        .await;
        self.plumbing.drop_all_stream_bindings().await;
    }

    /// COMMAND: run it on this connection's dispatch; a command that
    /// detached a Terminal drops that Terminal's stream binding too.
    async fn command(self, request_id: u32, command: Command) -> FrameFlow {
        let Some(selection) = *self.negotiated else {
            return FrameFlow::Continue(());
        };
        let command_outcome = (CommandDispatch {
            state: self.state,
            client_id: self.client_id,
            out_tx: &self.plumbing.out_tx,
            input_lane: self.input_lane,
            token: self.token,
            root_token: self.root_token,
            selection,
            command_tasks: self.command_tasks,
            input_receipts: self.input_receipts,
            input_receipt_slots: self.input_receipt_slots,
            input_credits: self.input_credits,
            held_commands: self.held_commands,
        })
        .run(request_id, command)
        .await;
        match command_outcome {
            CommandDispatchOutcome::Completed(Some(terminal_id)) => {
                // Already unsubscribed; drop the stream binding too.
                self.plumbing.drop_stream_binding(&terminal_id).await;
            }
            CommandDispatchOutcome::Completed(None) => {}
            CommandDispatchOutcome::Cancelled => {
                return FrameFlow::Break(ConnectionEnding::Cancelled);
            }
        }
        FrameFlow::Continue(())
    }

    /// `INPUT_*`: route the event (see [`is_input_frame`]).
    async fn input(self, frame: FrameKind) -> FrameFlow {
        match frame {
            FrameKind::InputKey { terminal_id, event } => {
                return self
                    .route_input(terminal_id, TerminalInput::Key(event))
                    .await;
            }
            FrameKind::InputMouse { terminal_id, event } => {
                return self
                    .route_input(terminal_id, TerminalInput::Mouse(event))
                    .await;
            }
            FrameKind::InputFocus { terminal_id, event } => {
                return self
                    .route_input(terminal_id, TerminalInput::Focus(event))
                    .await;
            }
            FrameKind::InputPaste { terminal_id, event } => {
                return self
                    .route_input(terminal_id, TerminalInput::Paste(event))
                    .await;
            }
            FrameKind::InputTerminalReply { terminal_id, bytes } => {
                let Some(selection) = self.negotiated.as_ref() else {
                    return FrameFlow::Continue(());
                };
                dispatch_terminal_reply(
                    self.client_id,
                    *selection,
                    &terminal_id,
                    bytes,
                    &self.plumbing.out_tx,
                )
                .await;
            }
            other => return direction_invalid(self.client_id, &other),
        }
        FrameFlow::Continue(())
    }

    /// Route one structured input event. Lane-bound input first takes a
    /// pane input credit (ADR-0144), and while the pane is saturated this
    /// connection reads no further frame: the event waits rather than being
    /// dropped, and the transport pushes back on the sender. A pane that
    /// stays saturated past the stall limit refuses the event with an
    /// uncorrelated `ERROR(RESOURCE_EXHAUSTED)`.
    async fn route_input(self, terminal_id: WireResourceId, input: TerminalInput) -> FrameFlow {
        let lane = self.input_lane.filter(|_| terminal_id.is_local());
        let credit = match lane {
            Some(_) => {
                let acquired = tokio::select! {
                    biased;
                    () = self.token.cancelled() => {
                        return FrameFlow::Break(ConnectionEnding::Cancelled);
                    }
                    acquired = self.input_credits.acquire(self.state, &terminal_id) => acquired,
                };
                let Ok(credit) = acquired else {
                    super::send_error(
                        &self.plumbing.out_tx,
                        ErrorCode::ResourceExhausted,
                        &InputStalled::message(&terminal_id),
                    )
                    .await;
                    return FrameFlow::Continue(());
                };
                credit
            }
            None => None,
        };
        route_client_input(self.state, lane, self.client_id, terminal_id, input, credit).await;
        FrameFlow::Continue(())
    }

    /// Frames about one attached Terminal's content stream (see
    /// [`is_stream_frame`]).
    async fn stream_frame(self, frame: FrameKind) -> FrameFlow {
        match frame {
            FrameKind::ViewportResize { viewport } => {
                debug!(
                    client_id = ?self.client_id,
                    cols = viewport.cols,
                    rows = viewport.rows,
                    "VIEWPORT_RESIZE"
                );
                handle_viewport_resize(self.state, self.client_id, &viewport);
            }
            FrameKind::ResizeTerminal {
                terminal_id,
                cols,
                rows,
                cell_px,
            } => {
                handle_terminal_resize(
                    self.state,
                    self.client_id,
                    &terminal_id,
                    (cols, rows),
                    cell_px,
                );
            }
            // ADR-0103 §4: an agent-session stream is raw-profile with no
            // history beyond its bootstrap. Both refusals keep the connection.
            FrameKind::FrameAck {
                ref terminal_id, ..
            } if is_agent_session(self.state, terminal_id) => {
                super::send_error(
                    &self.plumbing.out_tx,
                    ErrorCode::MalformedMessage,
                    "FRAME_ACK is not valid on an agent-session stream",
                )
                .await;
            }
            FrameKind::HistoryRequest {
                ref terminal_id, ..
            } if is_agent_session(self.state, terminal_id) => {
                super::send_error(
                    &self.plumbing.out_tx,
                    ErrorCode::WrongResourceKind,
                    "an agent-session stream retains no history beyond its bootstrap",
                )
                .await;
            }
            FrameKind::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            } => {
                handle_frame_ack(
                    self.state,
                    self.client_id,
                    &terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                );
            }
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            FrameKind::HistoryRequest {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                max_bytes,
                max_rows,
            } => {
                let Some(selection) = self.negotiated.as_ref() else {
                    return FrameFlow::Continue(());
                };
                // Terminal-content replies ride the bound stream when one
                // exists (proto.md §4.9); control otherwise.
                let history_tx = self.plumbing.sender_for(&terminal_id);
                serve_history_request(
                    self.state,
                    self.client_id,
                    &history_tx,
                    *selection,
                    HistoryPageRequest {
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        cursor,
                        max_bytes,
                        max_rows,
                    },
                )
                .await;
            }
            other => return direction_invalid(self.client_id, &other),
        }
        FrameFlow::Continue(())
    }

    /// L3 metadata frames (see [`is_metadata_frame`]).
    async fn metadata(self, frame: FrameKind) -> FrameFlow {
        let (state, client_id, out_tx) = (self.state, self.client_id, &self.plumbing.out_tx);
        match frame {
            FrameKind::GetMetadata {
                request_id,
                scope,
                key,
            } => {
                handle_get_metadata(state, client_id, request_id, &scope, &key, out_tx).await;
            }
            FrameKind::SetMetadata {
                request_id,
                scope,
                key,
                value,
            } if super::approvals::is_decision(&scope, &key) => {
                super::approvals::decide(state, client_id, request_id, &key, &value, out_tx).await;
            }
            FrameKind::SetMetadata {
                request_id,
                scope,
                key,
                value,
            } => {
                handle_set_metadata(
                    state,
                    client_id,
                    request_id,
                    &scope,
                    &key,
                    value,
                    self.root_token,
                );
            }
            FrameKind::DeleteMetadata {
                request_id,
                scope,
                key,
            } => {
                handle_delete_metadata(state, client_id, request_id, &scope, &key);
            }
            FrameKind::ListMetadata { request_id, scope } => {
                handle_list_metadata(state, client_id, request_id, &scope, out_tx).await;
            }
            FrameKind::SubscribeMetadata { scope, key } => {
                handle_subscribe_metadata(state, client_id, scope, key, out_tx);
            }
            FrameKind::SubscribeEvents {
                terminal,
                after_seq,
            } => {
                handle_subscribe_events(state, client_id, terminal, after_seq, out_tx);
            }
            other => return direction_invalid(client_id, &other),
        }
        FrameFlow::Continue(())
    }

    /// Resource and filesystem frames; any other kind is not valid from a
    /// client and closes the connection.
    async fn resource(self, frame: FrameKind) -> FrameFlow {
        let (state, client_id) = (self.state, self.client_id);
        match frame {
            FrameKind::ListDirectory {
                request_id,
                path,
                host,
            } => {
                super::directory::handle_list_directory(
                    state,
                    client_id,
                    super::directory::ListRequest {
                        request_id,
                        path,
                        host,
                    },
                    &self.plumbing.out_tx,
                );
            }
            FrameKind::PathQuery {
                request_id,
                root,
                query,
                recursive,
                host,
            } => {
                super::path_search::handle_path_query(
                    state,
                    client_id,
                    super::path_search::PathRequest {
                        request_id,
                        root,
                        query,
                        recursive,
                        host,
                    },
                    &self.plumbing.out_tx,
                );
            }
            FrameKind::SpawnResource {
                request_id,
                group,
                command,
                cwd,
                env,
                term,
                satellite,
                owner_terminal,
                agent_session,
                initial_size,
                resource,
            } => {
                let Some(selection) = self.negotiated.as_ref() else {
                    return FrameFlow::Continue(());
                };
                crate::runtime::idempotent_create::handle_spawn_resource(
                    state,
                    client_id,
                    request_id,
                    SpawnRequest {
                        group,
                        command,
                        cwd,
                        env,
                        term,
                        satellite,
                        owner_terminal,
                        agent_session,
                        initial_size,
                        resource,
                    },
                    &self.plumbing.out_tx,
                    selection.profile,
                    selection.limits,
                    self.root_token,
                    self.token,
                    &mut self.plumbing.output_pumps,
                    // QUIC multi-stream: content starts at STREAM_BIND (L1 §4.9).
                    selection.quic_streams(),
                )
                .await;
            }
            FrameKind::MoveResource {
                request_id,
                terminal,
                owner_terminal,
            } => {
                handle_move_terminal(
                    state,
                    client_id,
                    request_id,
                    terminal,
                    owner_terminal,
                    &self.plumbing.out_tx,
                )
                .await;
            }
            other => return direction_invalid(client_id, &other),
        }
        FrameFlow::Continue(())
    }
}

/// A frame kind a client may not send in the negotiated phase: close.
fn direction_invalid(client_id: ClientId, other: &FrameKind) -> FrameFlow {
    warn!(?client_id, kind = ?other, "direction-invalid client frame; closing");
    FrameFlow::Break(ConnectionEnding::Violation(ConnectionClose {
        attached_reason: Some("direction-invalid frame"),
        detach_reason: DetachReason::ProtocolError,
        code: ErrorCode::InvalidCommand,
        message: format!("frame is not valid from a client in the negotiated phase: {other:?}"),
    }))
}

/// The fields of one `ATTACH` frame.
struct AttachFrame {
    attach_id: u32,
    target: phux_protocol::wire::frame::AttachTarget,
    viewport: phux_protocol::wire::frame::ViewportInfo,
    request_scrollback: bool,
    scrollback_limit_lines: u32,
    role_policy: Option<phux_protocol::wire::frame::RolePolicy>,
}

/// `INPUT_*` frames: routed to the Terminal's input path.
const fn is_input_frame(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::InputKey { .. }
            | FrameKind::InputMouse { .. }
            | FrameKind::InputFocus { .. }
            | FrameKind::InputPaste { .. }
            | FrameKind::InputTerminalReply { .. }
    )
}

/// Frames about an attached Terminal's size or content stream.
const fn is_stream_frame(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::ViewportResize { .. }
            | FrameKind::ResizeTerminal { .. }
            | FrameKind::FrameAck { .. }
            | FrameKind::HistoryRequest { .. }
    )
}

/// L3 metadata and event-subscription frames.
const fn is_metadata_frame(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::GetMetadata { .. }
            | FrameKind::SetMetadata { .. }
            | FrameKind::DeleteMetadata { .. }
            | FrameKind::ListMetadata { .. }
            | FrameKind::SubscribeMetadata { .. }
            | FrameKind::SubscribeEvents { .. }
    )
}

/// Handle one QUIC Terminal-stream event (proto.md §4.2). `Bound` binds and
/// bootstraps the stream; `Ended` detaches as `DETACH_RESOURCE` would,
/// ignoring a stale generation.
async fn handle_stream_event(
    state: &SharedState,
    client_id: ClientId,
    event: QuicStreamEvent,
    plumbing: &mut ClientPlumbing,
    negotiated: Option<&NegotiatedConnection>,
    token: &CancellationToken,
) {
    // A bind before HELLO or without the negotiated shape is reset.
    let Some(selection) = negotiated.filter(|selection| selection.quic_streams()) else {
        if let QuicStreamEvent::Bound { send, recv, .. } = event {
            refuse_terminal_stream(send, recv);
        }
        return;
    };
    match event {
        QuicStreamEvent::Bound {
            terminal_id,
            stream_id,
            send,
            recv,
            window,
            frames,
            frame_bytes,
            terminal_frame_bytes,
        } => {
            bind_terminal_stream(
                state,
                client_id,
                terminal_id,
                stream_id,
                send,
                recv,
                window,
                frames,
                frame_bytes,
                terminal_frame_bytes,
                plumbing,
                selection,
                token,
            )
            .await;
        }
        QuicStreamEvent::Ended {
            terminal_id,
            stream_id,
        } => {
            if plumbing.is_current_stream(&terminal_id, stream_id) {
                teardown_terminal_stream(state, client_id, plumbing, &terminal_id).await;
            } else {
                debug!(
                    ?client_id,
                    ?terminal_id,
                    "stale terminal-stream end ignored"
                );
            }
        }
        QuicStreamEvent::Failed {
            terminal_id,
            stream_id,
            failure,
        } => {
            handle_stream_failure(state, client_id, plumbing, terminal_id, stream_id, failure)
                .await;
        }
    }
}

async fn handle_stream_failure(
    state: &SharedState,
    client_id: ClientId,
    plumbing: &mut ClientPlumbing,
    terminal_id: WireResourceId,
    stream_id: StreamId,
    failure: QuicStreamFailure,
) {
    if !plumbing.is_current_stream(&terminal_id, stream_id) {
        debug!(
            ?client_id,
            ?terminal_id,
            "stale terminal-stream failure ignored"
        );
        return;
    }
    let (code, message) = match failure {
        QuicStreamFailure::Framing(framing) => (ErrorCode::FrameTooLarge, framing.wire_message()),
        QuicStreamFailure::IncompleteFrame => (
            ErrorCode::MalformedMessage,
            "Terminal QUIC stream ended or timed out mid-frame".to_owned(),
        ),
        QuicStreamFailure::Transport(message) => (
            ErrorCode::MalformedMessage,
            format!("Terminal QUIC stream failed: {message}"),
        ),
    };
    super::send_error(
        &plumbing.out_tx,
        code,
        &format!("{terminal_id:?} stream {stream_id:?}: {message}"),
    )
    .await;
    teardown_terminal_stream(state, client_id, plumbing, &terminal_id).await;
}

/// Subscribe-and-bootstrap one bound Terminal stream. The subscription
/// registers the control mailbox; the bootstrap runs on the stream's. A
/// failed bootstrap keeps the subscription (the client may re-bind) but
/// finishes the stream and reports the refusal on control.
#[allow(
    clippy::too_many_arguments,
    reason = "the negotiated connection context plus the split stream halves; same list as the attach path"
)]
async fn bind_terminal_stream(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: WireResourceId,
    stream_id: StreamId,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    window: SendWindow,
    frames: tokio::sync::mpsc::Sender<crate::transport::quic::AdmittedFrame>,
    frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    terminal_frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    plumbing: &mut ClientPlumbing,
    selection: &NegotiatedConnection,
    token: &CancellationToken,
) {
    // workload-auth §6: OBSERVE on the bound Terminal, before membership is
    // consulted, so an unauthorized bind learns nothing about the Terminal.
    if super::dispatch_guard::refuse_stream_bind(state, client_id, &terminal_id, &plumbing.out_tx)
        .await
    {
        refuse_terminal_stream(send, recv);
        return;
    }
    // Authorization is subscription membership.
    let subscribed = state.with(|s| {
        let local = match s.resolve_resource(&terminal_id).into_owned() {
            crate::state::ResolvedOwned::Local(local) => local,
            crate::state::ResolvedOwned::Remote(_) | crate::state::ResolvedOwned::Unknown => {
                return false;
            }
        };
        s.subscribers_for_terminal(local.id).contains(&client_id)
    });
    if !subscribed {
        refuse_terminal_stream(send, recv);
        super::send_error(
            &plumbing.out_tx,
            ErrorCode::TerminalNotFound,
            &format!("STREAM_BIND for unsubscribed terminal: {terminal_id:?}"),
        )
        .await;
        return;
    }
    let writer = QuicWriter::from_terminal_stream(send, window);
    let diagnostics = writer.diagnostic_tracker();
    debug!(?client_id, ?stream_id, context = ?diagnostics.context(), "bound QUIC diagnostic stream");
    let bind_started = std::time::Instant::now();
    let (stream_tx, ingress_active, ingress_cancel) = plumbing
        .bind_stream(client_id, terminal_id.clone(), stream_id, writer)
        .await;
    // Subscribe against control before bootstrapping, so lifecycle fanout
    // resolves there even if the Terminal dies mid-bootstrap. The role was
    // declared by the `ATTACH_RESOURCE` (ADR-0127).
    let subscription =
        subscribe_attach_terminal(state, client_id, &terminal_id, &plumbing.out_tx, None);
    let Ok(super::commands::AttachSubscription { core, handle, .. }) = subscription else {
        let mut recv = recv;
        let _ = recv.stop(0x10_u32.into());
        plumbing.drop_stream_binding(&terminal_id).await;
        super::send_error(
            &plumbing.out_tx,
            ErrorCode::TerminalNotFound,
            &format!("STREAM_BIND raced terminal death: {terminal_id:?}"),
        )
        .await;
        return;
    };
    tokio::spawn(pump_terminal_stream(
        recv,
        terminal_id.clone(),
        stream_id,
        frames,
        frame_bytes,
        terminal_frame_bytes,
        ingress_active,
        ingress_cancel,
        Some(diagnostics.clone()),
    ));
    if let Err(failure) = bootstrap_attach_terminal(
        state,
        client_id,
        &terminal_id,
        core,
        &handle,
        &stream_tx,
        stream_id,
        selection.client_caps,
        selection.profile,
        selection.limits,
        token,
        None,
    )
    .await
    {
        plumbing.drop_stream_binding(&terminal_id).await;
        super::send_error(&plumbing.out_tx, failure.code, &failure.message).await;
    } else {
        diagnostics.record_ready_latency(
            crate::stream_diagnostics::ReadyKind::Initial,
            bind_started.elapsed(),
        );
    }
}

/// Tear one Terminal stream down: unsubscribe, then drop the binding so its
/// writer drains and finishes the QUIC stream.
async fn teardown_terminal_stream(
    state: &SharedState,
    client_id: ClientId,
    plumbing: &mut ClientPlumbing,
    terminal_id: &WireResourceId,
) {
    handle_detach_terminal(state, client_id, terminal_id).await;
    plumbing.drop_stream_binding(terminal_id).await;
}

/// Write one legacy printable key on the receive task when the PTY writer
/// is idle. `true` means the event was consumed (written or refused).
fn try_inline_key(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &WireResourceId,
    input: &TerminalInput,
) -> bool {
    if !phux_perf::echo_hop::fast_path_enabled() {
        return false;
    }
    let TerminalInput::Key(event) = input else {
        return false;
    };
    let Some(prepared) = super::with_attached_input_destination(
        state,
        client_id,
        terminal_id,
        "INPUT_KEY",
        |destination| {
            let options = destination.handle.input_snapshot.borrow().key;
            let text = crate::input::key::legacy_printable(event, options)?;
            let fast = destination.handle.fast_pty.as_ref()?.upgrade()?;
            Some((fast, text.as_bytes().to_vec()))
        },
    ) else {
        return false;
    };
    let Some((fast, bytes)) = prepared else {
        return false;
    };
    match fast.try_direct(&bytes) {
        crate::terminal_actor::DirectWrite::Wrote | crate::terminal_actor::DirectWrite::Handled => {
            true
        }
        crate::terminal_actor::DirectWrite::Busy => false,
    }
}

/// Route one decoded `INPUT_*` event: a local pane id goes to the input lane
/// (ADR-0044) when there is one, spending `credit` (ADR-0144); satellite ids
/// and the no-lane path use the inline [`handle_terminal_input`] with the
/// same gates.
async fn route_client_input(
    state: &SharedState,
    input_lane: Option<&InputLaneHandle>,
    client_id: ClientId,
    terminal_id: WireResourceId,
    input: TerminalInput,
    credit: Option<crate::terminal_actor::InputCredit>,
) {
    phux_perf::echo_hop::record("server_recv");
    if try_inline_key(state, client_id, &terminal_id, &input) {
        return;
    }
    let frame_label = match &input {
        TerminalInput::Key(_) => "INPUT_KEY",
        TerminalInput::Mouse(_) => "INPUT_MOUSE",
        TerminalInput::Focus(_) => "INPUT_FOCUS",
        TerminalInput::Paste(_) => "INPUT_PASTE",
    };
    if let Some(lane) = input_lane
        && terminal_id.is_local()
    {
        // Arm before the send. `pending` is still zero while this key sits
        // in the lane thread; the next printable would otherwise write the
        // PTY ahead of it. `ROUTE_INPUT` and `APPLY_INPUT` arm in the lane
        // admission, so every source for the pane shares this count.
        let ahead = super::input_lane::arm_pane_lane(state, &terminal_id);
        lane.route(
            RoutedInput::attached(client_id, terminal_id, input, frame_label)
                .with_credit(credit)
                .with_lane_ahead(ahead),
        )
        .await;
        return;
    }
    handle_terminal_input(state, client_id, &terminal_id, input, frame_label);
}

pub(crate) async fn abort_output_pumps(
    output_pumps: &mut JoinSet<()>,
    client_id: ClientId,
    reason: &'static str,
) {
    if output_pumps.is_empty() {
        return;
    }
    debug!(
        ?client_id,
        pump_count = output_pumps.len(),
        reason,
        "aborting per-attach output pumps",
    );
    output_pumps.abort_all();
    while output_pumps.join_next().await.is_some() {}
}

// L3 metadata dispatch (SPEC §7.4 / §11.L3). Replies are gated on
// `client_speaks_l3` (SPEC §16.4): a non-L3 consumer gets silence.

pub(crate) async fn handle_get_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
    out_tx: &mpsc::Sender<Outbound>,
) {
    state.with(|s| kick_satellite_metadata_mirror(s, scope, key));
    let nonce_result = is_reserved_session_create_result(scope, key);
    let (value, speaks_l3) = state.with(|s| {
        (
            read_metadata_value(s, client_id, scope, key, nonce_result),
            s.client_speaks_l3(client_id),
        )
    });
    let one_shot = value.is_some() && nonce_result;
    debug!(
        ?client_id,
        request_id,
        ?scope,
        %key,
        present = value.is_some(),
        speaks_l3,
        "GET_METADATA",
    );
    if !speaks_l3 {
        return;
    }
    if out_tx
        .send(Outbound::Frame(FrameKind::MetadataValue {
            request_id,
            value,
        }))
        .await
        .is_err()
    {
        trace!(
            ?client_id,
            request_id, "METADATA_VALUE send dropped: writer gone"
        );
    } else if one_shot {
        state.with_mut(|s| s.consume_session_create_result(key));
    }
}

/// The value one `GET_METADATA` answers with: the asking connection's own
/// `phux.whoami/v1` record (never stored), a nonce-bearing create result only
/// its owner may read, or whatever the store holds.
fn read_metadata_value(
    s: &ServerState,
    client_id: ClientId,
    scope: &Scope,
    key: &str,
    nonce_result: bool,
) -> Option<Vec<u8>> {
    if super::whoami::is_whoami_key(scope, key) {
        return super::whoami::record_for(s, client_id);
    }
    if nonce_result && !s.owns_session_create_result(client_id, key) {
        return None;
    }
    s.metadata().get(scope, key)
}

/// Why a client write or delete of `(scope, key)` is ignored, if it is.
fn protected_key_refusal(scope: &Scope, key: &str) -> Option<&'static str> {
    if is_reserved_session_create_result(scope, key) {
        return Some("reserved session-create result key");
    }
    if is_satellite_terminal_scope(scope) {
        return Some("satellite terminal metadata is read-only");
    }
    if is_server_owned_key(key) {
        return Some("server-owned key");
    }
    None
}

/// Keys only the server writes, in any scope.
fn is_server_owned_key(key: &str) -> bool {
    use phux_protocol::wire::frame::{
        APPROVAL_KEY_PREFIX, RESOURCE_ASKED_KEY, RESOURCE_PANE_OCCUPANT_KEY, WHOAMI_KEY,
    };

    key == RESOURCE_PANE_OCCUPANT_KEY
        || key == RESOURCE_ASKED_KEY
        || key == phux_protocol::wire::frame::RESOURCE_PROGRAM_STATUS_KEY
        || key.starts_with(phux_protocol::wire::frame::RESOURCE_PROGRAM_STATUS_RECORD_PREFIX)
        || key == WHOAMI_KEY
        || key.starts_with(APPROVAL_KEY_PREFIX)
}

#[derive(serde::Deserialize)]
struct SessionCreateRequest {
    name: String,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    agent_session: Option<Vec<u8>>,
    #[serde(default)]
    request_token: Option<String>,
    /// ADR-0105: the session survives its last window.
    #[serde(default)]
    keep_empty: bool,
    /// ADR-0105: create the session with no seed terminal (implies
    /// `keep_empty`); refused alongside a `command`.
    #[serde(default)]
    empty: bool,
}

/// Translate a `SESSION_CREATE_KEY` request into a `SessionCreate` commit
/// attributed to the writer: `empty` creates an empty persistent session,
/// `keep_empty` a persistent one with a seed pane, and a bare create an
/// ephemeral one whose last pane's exit deletes it. Returns the seed pane's
/// wire id, or `None` for an empty session.
fn run_session_create(
    state: &SharedState,
    writer: ClientId,
    request: SessionCreateRequest,
    root_token: &CancellationToken,
) -> Result<Option<WireResourceId>, String> {
    if request.empty {
        let actor = state.with(|s| s.actor_for(writer));
        return crate::runtime::commands::create_empty_session(state, &request.name, &actor)
            .map(|()| None);
    }
    // The seed pane's `pane_spawned` names this writer and the create's
    // token (ADR-0123, ADR-0126).
    let origin = crate::runtime::commands::SeedOrigin {
        agent_session: request.agent_session,
        attribution: crate::runtime::commands::SpawnAttribution {
            actor: Some(writer),
            operation_id: request
                .request_token
                .as_deref()
                .and_then(crate::runtime::idempotent_create::uuid_bytes)
                .and_then(phux_protocol::ids::IdempotencyKey::new),
            retain_secs: None,
        },
        lifetime: if request.keep_empty {
            phux_core::SessionLifetime::Persistent
        } else {
            phux_core::SessionLifetime::Ephemeral
        },
    };
    let wire = crate::runtime::commands::create_named_session(
        state,
        &request.name,
        request.command,
        request.cwd.as_deref(),
        request.env,
        origin,
        root_token,
    )?;
    Ok(Some(wire))
}

/// The result document published under a create's result key. `terminal_id`
/// is `null` and `empty` is `true` for an empty session; a seeded session's
/// document keeps its pre-ADR-0105 fields. `session_id` binds the receipt to
/// the created identity even if another client later renames the session.
fn session_create_payload(
    name: &str,
    session_id: Option<u32>,
    wire: Option<&WireResourceId>,
    request_token: Option<&str>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "name": name,
        "session_id": session_id,
        "terminal_id": wire.map(WireResourceId::local_id),
        "request_token": request_token,
    });
    if wire.is_none() {
        payload["empty"] = serde_json::Value::Bool(true);
    }
    payload
}

/// Translate a `phux.session.keep_empty/v1` write into a
/// `SessionSetLifetime` commit attributed to the writer: `true` is
/// persistent, `false` ephemeral. A change is broadcast to the key's
/// subscribers; ephemeral on an empty session deletes it, detaching its
/// clients and possibly self-exiting.
fn apply_session_keep_empty(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    value: &[u8],
    root_token: &CancellationToken,
) {
    use crate::state::KeepEmptyOutcome;

    let Some((name, keep)) = phux_protocol::wire::frame::decode_session_keep_empty(value) else {
        warn!(
            ?client_id,
            request_id,
            "SET_METADATA(session-keep-empty): malformed value (want name\\0true|false); ignoring",
        );
        return;
    };
    let (outcome, clients, server_drained) = state.with_mut(|s| {
        let actor = s.actor_for(client_id);
        let outcome = s.set_session_keep_empty(name, keep, &actor);
        if matches!(
            outcome,
            KeepEmptyOutcome::Changed | KeepEmptyOutcome::Removed
        ) {
            let _ = s.metadata_broadcast_by(
                scope,
                ServerInterceptedKey::SessionKeepEmpty,
                value,
                Some(client_id),
            );
        }
        let clients: Vec<_> = s
            .take_killed_sessions()
            .into_iter()
            .flat_map(|session| s.attached_clients_in_session(session))
            .collect();
        let drained = outcome == KeepEmptyOutcome::Removed && s.self_exit_due();
        (outcome, clients, drained)
    });
    debug!(
        ?client_id,
        request_id,
        %name,
        keep,
        ?outcome,
        "SET_METADATA(session-keep-empty): applied",
    );
    detach_clients_of_killed_session(state, clients);
    if server_drained && !root_token.is_cancelled() {
        info!("nothing left running after serving clients; server self-exit");
        root_token.cancel();
    }
}

/// Detach the clients attached to a session that was just killed, with
/// `DETACHED { SESSION_KILLED }`. Each delivery waits in its own task, so a
/// wedged client's full mailbox cannot block the write that killed it.
pub(super) fn detach_clients_of_killed_session(
    state: &SharedState,
    clients: Vec<(ClientId, mpsc::Sender<Outbound>)>,
) {
    for (detached_client, tx) in clients {
        let detached_state = state.clone();
        tokio::task::spawn_local(async move {
            let _ = tx
                .send(Outbound::Frame(FrameKind::Detached {
                    reason: Some(DetachReason::SessionKilled),
                    message: "the session this attach was rooted in was killed".to_owned(),
                }))
                .await;
            detach_and_release_consumer_state(&detached_state, detached_client);
        });
    }
}

/// Why a parsed `SESSION_CREATE_KEY` request is ignored, if it is. Each
/// refusal is silent on the wire (`SET_METADATA` has no reply) and logged by
/// the caller.
fn session_create_refusal(request: &SessionCreateRequest) -> Option<&'static str> {
    use phux_protocol::wire::frame::MAX_AGENT_SESSION_RECORD_BYTES;

    // ADR-0105: an empty session has no seed terminal.
    if request.empty && (request.command.is_some() || request.agent_session.is_some()) {
        return Some("`empty` cannot carry a command or agent session");
    }
    if request
        .agent_session
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > MAX_AGENT_SESSION_RECORD_BYTES)
    {
        return Some("invalid agent session record size");
    }
    if request
        .request_token
        .as_deref()
        .is_some_and(|token| !valid_session_create_token(token))
    {
        return Some("invalid request token");
    }
    None
}

/// The key a create publishes its result under: a one-shot, request-specific
/// key for a nonce-bearing client, the original global key for a legacy one.
fn session_create_result_key(request_token: Option<&str>) -> String {
    use phux_protocol::wire::frame::{SESSION_CREATE_RESULT_KEY, SESSION_CREATE_RESULT_KEY_PREFIX};

    request_token.map_or_else(
        || SESSION_CREATE_RESULT_KEY.to_owned(),
        |token| format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{token}"),
    )
}

/// Publish a successful create's result document under `result_key`, and
/// track a one-shot key so it is consumed on its owner's first read.
fn publish_session_create_result(
    state: &SharedState,
    client_id: ClientId,
    result_key: String,
    payload: &serde_json::Value,
    one_shot: bool,
) {
    let Ok(bytes) = serde_json::to_vec(payload) else {
        return;
    };
    state.with_mut(|s| {
        let _ = s.metadata_set(&Scope::Global, &result_key, bytes);
        if one_shot {
            s.track_session_create_result(client_id, result_key);
        }
    });
}

/// The digest a create's token is bound to: every field of the request but
/// the token (ADR-0126). `env` is a sorted map, so the encoding is canonical.
fn session_create_digest(request: &SessionCreateRequest) -> [u8; 32] {
    use sha2::Digest as _;

    let canonical = serde_json::json!({
        "name": request.name,
        "command": request.command,
        "cwd": request.cwd,
        "env": request.env,
        "agent_session": request.agent_session,
        "keep_empty": request.keep_empty,
        "empty": request.empty,
    });
    sha2::Sha256::digest(canonical.to_string().as_bytes()).into()
}

fn valid_session_create_token(token: &str) -> bool {
    token.len() == 36
        && token.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn is_reserved_session_create_result(scope: &Scope, key: &str) -> bool {
    matches!(scope, Scope::Global)
        && key.starts_with(phux_protocol::wire::frame::SESSION_CREATE_RESULT_KEY_PREFIX)
}

fn handle_session_create_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    value: &[u8],
    root_token: &CancellationToken,
) {
    use crate::runtime::idempotent_create::SessionCreateAdmission;

    let Ok(request) = serde_json::from_slice::<SessionCreateRequest>(value) else {
        warn!(
            ?client_id,
            request_id,
            "SET_METADATA(session-create): malformed JSON value (want {{name, command?, cwd?, env?, request_token?, agent_session?, keep_empty?, empty?}}); ignoring",
        );
        return;
    };
    if let Some(reason) = session_create_refusal(&request) {
        warn!(
            ?client_id,
            request_id, reason, "SET_METADATA(session-create): refused; ignoring"
        );
        return;
    }
    // ADR-0126: the request token is the create's idempotency key.
    let token = request
        .request_token
        .as_deref()
        .and_then(crate::runtime::idempotent_create::uuid_bytes);
    let claim = match crate::runtime::idempotent_create::admit_session_create(
        state,
        token,
        session_create_digest(&request),
    ) {
        SessionCreateAdmission::Unkeyed => None,
        SessionCreateAdmission::Owner(claim) => Some(claim),
        SessionCreateAdmission::Replay(mut payload) => {
            // Dedupe is case-insensitive; answer in the repeat's spelling.
            payload["request_token"] = serde_json::Value::from(request.request_token.clone());
            // The repeating connection becomes the result's only owner: the
            // original sender may be the connection whose loss caused it.
            let result_key = session_create_result_key(request.request_token.as_deref());
            state.with_mut(|s| s.disown_session_create_result(&result_key));
            publish_session_create_result(state, client_id, result_key, &payload, true);
            debug!(
                ?client_id,
                request_id, "SET_METADATA(session-create): repeat replayed the original result"
            );
            return;
        }
        SessionCreateAdmission::Refused(reason) => {
            warn!(
                ?client_id,
                request_id, reason, "SET_METADATA(session-create): refused; ignoring"
            );
            return;
        }
    };
    run_and_publish_session_create(
        state,
        client_id,
        request_id,
        request,
        claim.as_ref(),
        root_token,
    );
    // An unbound claim releases the token, so a failed create can repeat.
    drop(claim);
}

/// Run an admitted create and publish its result. A keyed create binds the
/// result to its token before publishing, so a repeat answers the same one.
fn run_and_publish_session_create(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    request: SessionCreateRequest,
    claim: Option<&crate::runtime::operation_dedupe::OperationClaim>,
    root_token: &CancellationToken,
) {
    let name = request.name.clone();
    let request_token = request.request_token.clone();
    let result_key = session_create_result_key(request_token.as_deref());
    if request_token.is_some() && state.with(|s| s.session_create_result_is_pending(&result_key)) {
        warn!(
            ?client_id,
            request_id,
            "SET_METADATA(session-create): request token already has a pending result; ignoring"
        );
        return;
    }
    let outcome = run_session_create(state, client_id, request, root_token);
    if let Ok(wire) = &outcome {
        // Creation and this lookup run synchronously in the same server turn.
        let session_id = state.with_mut(|s| {
            s.find_session_by_name(&name)
                .map(|id| s.idspace.intern_session(id).get())
        });
        let payload =
            session_create_payload(&name, session_id, wire.as_ref(), request_token.as_deref());
        if let Some(claim) = claim {
            claim.bind(
                &crate::runtime::operation_dedupe::CachedOutcome::SessionCreate(payload.clone()),
            );
        }
        publish_session_create_result(
            state,
            client_id,
            result_key,
            &payload,
            request_token.is_some(),
        );
    }
    debug!(
        ?client_id,
        request_id,
        %name,
        ok = outcome.is_ok(),
        "SET_METADATA(session-create): create attempted",
    );
}
/// Reject writes into a local Terminal namespace after its owner is gone.
fn reject_unknown_local_terminal_scope(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
) -> bool {
    let Scope::Resource(terminal @ WireResourceId::Local { .. }) = scope else {
        return false;
    };
    if state.with(|s| s.terminal_from_wire(terminal)).is_some() {
        return false;
    }
    warn!(
        ?client_id,
        request_id,
        ?terminal,
        %key,
        "SET_METADATA: unknown terminal scope; ignoring",
    );
    true
}

/// The writes `SET_METADATA` refuses outright. Every rejection is a logged
/// no-op: `SET_METADATA` has no reply frame.
fn reject_set_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
    value: &[u8],
) -> bool {
    use phux_protocol::wire::frame::{
        MAX_AGENT_SESSION_RECORD_BYTES, RESOURCE_AGENT_SESSION_KEY, Scope,
    };

    if let Some(reason) = protected_key_refusal(scope, key) {
        warn!(?client_id, request_id, %key, reason, "SET_METADATA: ignoring");
        return true;
    }
    // ADR-0129 / L3 §2: an oversized value is refused whole.
    let cap = state.with(ServerState::metadata_value_bytes) as usize;
    if value.len() > cap {
        warn!(
            ?client_id,
            request_id,
            %key,
            value_len = value.len(),
            cap,
            "SET_METADATA: value exceeds limits.metadata-value-bytes; ignoring (no reply frame exists to tell the writer)"
        );
        return true;
    }
    // ADR-0129: a late write must not resurrect a reaped session's layout.
    if state.with(|s| s.layout_key_names_a_dead_session(scope, key)) == Some(true) {
        warn!(
            ?client_id,
            request_id,
            %key,
            "SET_METADATA: layout key names a session that is no longer live; ignoring"
        );
        return true;
    }
    if reject_unknown_local_terminal_scope(state, client_id, request_id, scope, key) {
        return true;
    }
    if key == RESOURCE_AGENT_SESSION_KEY
        && (!matches!(scope, Scope::Resource(WireResourceId::Local { .. }))
            || value.is_empty()
            || value.len() > MAX_AGENT_SESSION_RECORD_BYTES)
    {
        warn!(
            ?client_id,
            request_id,
            ?scope,
            value_len = value.len(),
            "SET_METADATA(agent-session): want a local Terminal scope and 1..=4096 bytes; ignoring",
        );
        return true;
    }
    false
}

/// Translate a session rename written as `current_name\0new_name` into a
/// `SessionRename` commit attributed to the writer. An applied change fans out the `current\0new` value to the
/// key's subscribers; a malformed value or unknown session is a no-op.
fn apply_session_rename(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    value: &[u8],
) {
    let parsed = std::str::from_utf8(value).ok().and_then(|s| {
        s.split_once('\0')
            .map(|(cur, new)| (cur.to_owned(), new.to_owned()))
    });
    let Some((current, new_name)) = parsed else {
        warn!(
            ?client_id,
            request_id,
            "SET_METADATA(session-name): malformed value (want current\\0new); ignoring",
        );
        return;
    };
    let (outcome, delivered) = state.with_mut(|s| {
        let actor = s.actor_for(client_id);
        let outcome = s.rename_session_as(&current, &new_name, &actor);
        // `Renamed` also covers a rename to the same name; broadcast only a
        // real change.
        let delivered =
            if matches!(outcome, crate::state::RenameOutcome::Renamed) && current != new_name {
                s.metadata_broadcast_by(
                    scope,
                    ServerInterceptedKey::SessionName,
                    value,
                    Some(client_id),
                )
            } else {
                Vec::new()
            };
        (outcome, delivered)
    });
    debug!(
        ?client_id,
        request_id,
        %current,
        %new_name,
        ?outcome,
        subscriber_count = delivered.len(),
        "SET_METADATA(session-name): applied registry rename",
    );
}

/// Store an ordinary metadata write and fan it out to subscribers. The only
/// path an explicit agent-record write takes, so it records the declaration
/// for the arbiter (ADR-0046 §E); the bytes alone cannot say who wrote them.
fn store_metadata_value(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
    value: Vec<u8>,
) {
    let declared_agent_record = matches!(scope, Scope::Resource(_)) && key == RESOURCE_AGENT_KEY;
    let agent_value = declared_agent_record.then(|| value.clone());

    let delivered = state.with_mut(|s| {
        if let (Some(bytes), Scope::Resource(terminal)) = (agent_value.as_deref(), scope) {
            s.agent_records_mut().note_explicit_set(terminal, bytes);
        }
        s.metadata_set_by(scope, key, value, Some(client_id))
    });
    // The store just changed under the detector's edge filter.
    invalidate_agent_detector(state, scope, key);
    trace!(
        ?client_id,
        request_id,
        subscriber_count = delivered.len(),
        "SET_METADATA delivered"
    );
}

pub(crate) fn handle_set_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
    value: Vec<u8>,
    root_token: &CancellationToken,
) {
    use phux_protocol::wire::frame::{SESSION_CREATE_KEY, Scope};

    debug!(?client_id, request_id, ?scope, %key, "SET_METADATA");
    if reject_set_metadata(state, client_id, request_id, scope, key, &value) {
        return;
    }
    // ADR-0019 / ADR-0027: session create, rename, and keep-empty are
    // global-scope writes the server applies rather than stores.
    if key == SESSION_CREATE_KEY && matches!(scope, Scope::Global) {
        handle_session_create_metadata(state, client_id, request_id, &value, root_token);
        return;
    }
    if key == phux_protocol::wire::frame::SESSION_NAME_KEY && matches!(scope, Scope::Global) {
        apply_session_rename(state, client_id, request_id, scope, &value);
        return;
    }
    if key == phux_protocol::wire::frame::SESSION_KEEP_EMPTY_KEY && matches!(scope, Scope::Global) {
        apply_session_keep_empty(state, client_id, request_id, scope, &value, root_token);
        return;
    }
    // R17: the TUI layout key is translated into the session's arrangement,
    // attributed to the writer, then stored and broadcast as before.
    if let Some(session) = state.with(|s| s.tui_layout_session(scope, key)) {
        let translated = state.with_mut(|s| {
            let actor = s.actor_for(client_id);
            s.translate_layout_write(session, &value, &actor)
        });
        if let Err(reason) = translated {
            debug!(?client_id, request_id, %key, %reason, "layout write not translated");
        }
    }
    // ADR-0155: a push grant is stored like any key, but the server also
    // remembers who wrote it, so a later ask is pushed only once that
    // connection is gone.
    if crate::push::is_grant_key(scope, key) {
        match crate::push::parse_grant(&value) {
            Ok(_) => {
                let admitted = state.with_mut(|s| s.push_grants_mut().register(key, client_id));
                if !admitted {
                    warn!(?client_id, request_id, %key, "SET_METADATA: push grant registry full; ignoring");
                    return;
                }
            }
            Err(reason) => {
                warn!(?client_id, request_id, %key, %reason, "SET_METADATA: ignoring push grant");
                return;
            }
        }
    }
    let config_reload =
        key == phux_protocol::wire::frame::CONFIG_RELOAD_KEY && matches!(scope, Scope::Global);
    store_metadata_value(state, client_id, request_id, scope, key, value);
    // A hub is a doorbell consumer too: it re-reads its own `[[satellites]]`
    // (L3 §3.8), before any later frame on this connection is handled, so a
    // writer's read-back barrier also covers the reload.
    if config_reload {
        crate::hub::reload_satellites(state);
        crate::uplink::reload_uplinks(state);
    }
}

pub(crate) fn handle_delete_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    key: &str,
) {
    debug!(?client_id, request_id, ?scope, %key, "DELETE_METADATA");
    if let Some(reason) = protected_key_refusal(scope, key) {
        warn!(?client_id, request_id, %key, reason, "DELETE_METADATA: ignoring");
        return;
    }
    let delivered = state.with_mut(|s| {
        // ADR-0046 §E: deleting the record withdraws any human declaration,
        // so the detector resumes ownership of this Terminal.
        if let Scope::Resource(terminal) = scope
            && key == RESOURCE_AGENT_KEY
        {
            s.agent_records_mut().note_explicit_delete(terminal);
        }
        if crate::push::is_grant_key(scope, key) {
            s.push_grants_mut().forget(key);
        }
        s.metadata_delete_by(scope, key, Some(client_id))
    });
    invalidate_agent_detector(state, scope, key);
    trace!(
        ?client_id,
        request_id,
        subscriber_count = delivered.len(),
        "DELETE_METADATA delivered"
    );
}

pub(crate) async fn handle_list_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &Scope,
    out_tx: &mpsc::Sender<Outbound>,
) {
    let (mut keys, speaks_l3) =
        state.with(|s| (s.metadata().list(scope), s.client_speaks_l3(client_id)));
    if matches!(scope, Scope::Global) {
        keys.retain(|key| {
            !key.starts_with(phux_protocol::wire::frame::SESSION_CREATE_RESULT_KEY_PREFIX)
        });
    }
    debug!(
        ?client_id,
        request_id,
        ?scope,
        key_count = keys.len(),
        speaks_l3,
        "LIST_METADATA",
    );
    if !speaks_l3 {
        return;
    }
    if out_tx
        .send(Outbound::Frame(FrameKind::MetadataKeys {
            request_id,
            keys,
        }))
        .await
        .is_err()
    {
        trace!(
            ?client_id,
            request_id, "METADATA_KEYS send dropped: writer gone"
        );
    }
}

/// A satellite Terminal scope. Client writes of one are ignored (ADR-0136):
/// the mirror is read-only, and every other key stays on the satellite.
const fn is_satellite_terminal_scope(scope: &Scope) -> bool {
    matches!(scope, Scope::Resource(WireResourceId::Satellite { .. }))
}

/// Start the read-only agent-metadata mirror when `scope` names a satellite
/// this hub routes and `key` is allowlisted. `false` otherwise.
fn kick_satellite_metadata_mirror(state: &ServerState, scope: &Scope, key: &str) -> bool {
    let Scope::Resource(terminal) = scope else {
        return false;
    };
    if !crate::hub::metadata_mirror::is_mirrored_key(key) {
        return false;
    }
    let Some((host, id)) = crate::hub::relay::satellite_route(terminal) else {
        return false;
    };
    let Some(relay) = state.hub_relay(&host) else {
        return false;
    };
    relay.mirror_terminal(id);
    true
}

/// Refuse an L3 metadata subscription whose `Terminal` scope names a
/// satellite pane with an uncorrelated `UNSUPPORTED_SATELLITE_ROUTE` push,
/// so the consumer does not wait on a change that can never come. Mirrored
/// agent keys on a routing hub are handled before this (ADR-0136). Returns
/// `true` when the caller must abandon the subscription.
fn refuse_satellite_metadata_scope(
    client_id: ClientId,
    scope: &Scope,
    key: &str,
    out_tx: &mpsc::Sender<Outbound>,
) -> bool {
    let Scope::Resource(terminal) = scope else {
        return false;
    };
    let Some((host, id)) = crate::hub::relay::satellite_route(terminal) else {
        return false;
    };
    warn!(
        ?client_id,
        satellite = %host,
        %key,
        "SUBSCRIBE_METADATA refused: L3 metadata has no satellite route"
    );
    push_error(
        out_tx,
        ErrorCode::UnsupportedSatelliteRoute,
        format!(
            "L3 metadata does not federate: no subscription to key '{key}' on \
             {host}/@{id}. The record lives on that satellite's own server; run \
             the command there.",
            host = host.as_str(),
        ),
    );
    true
}

/// Record an L3 metadata subscription for `client_id` (SPEC §7.4). The
/// mailbox is captured so a consumer that never attached still receives
/// `METADATA_CHANGED`.
pub(crate) fn handle_subscribe_metadata(
    state: &SharedState,
    client_id: ClientId,
    scope: Scope,
    key: String,
    out_tx: &mpsc::Sender<Outbound>,
) {
    if is_reserved_session_create_result(&scope, &key) {
        warn!(
            ?client_id,
            "SUBSCRIBE_METADATA: reserved session-create result key; ignoring"
        );
        return;
    }
    state.with_mut(|s| {
        if !s.client_speaks_l3(client_id) {
            debug!(?client_id, ?scope, %key, "SUBSCRIBE_METADATA refused (non-L3)");
            return;
        }
        if kick_satellite_metadata_mirror(s, &scope, &key) {
            // Fall through: the subscription is on the hub's retagged scope.
        } else if refuse_satellite_metadata_scope(client_id, &scope, &key, out_tx) {
            return;
        }
        let log_scope = scope.clone();
        let log_key = key.clone();
        if s.metadata_subscribe(client_id, scope, key, out_tx.clone()) {
            debug!(?client_id, ?log_scope, %log_key, "SUBSCRIBE_METADATA");
        } else {
            // Per-connection cap; refuse rather than evict a working one.
            warn!(
                ?client_id,
                ?log_scope,
                %log_key,
                "SUBSCRIBE_METADATA refused: per-connection subscription cap reached"
            );
        }
    });
}

/// Record an agent-event subscription for `client_id` (SPEC §7.5,
/// ADR-0123): server-wide or per-pane, not tier-gated. With `after_seq` it
/// replays from the journal first (L1 §7.3); what the mailbox cannot take
/// is owed as a `journal_gap` and delivered by the event pump.
pub(crate) fn handle_subscribe_events(
    state: &SharedState,
    client_id: ClientId,
    terminal: Option<WireResourceId>,
    after_seq: Option<u64>,
    out_tx: &mpsc::Sender<Outbound>,
) {
    debug!(?client_id, ?terminal, ?after_seq, "SUBSCRIBE_EVENTS");
    if let Some(wire_id) = &terminal
        && let Some(route) = crate::hub::relay::satellite_route(wire_id)
    {
        subscribe_via_hub(state, client_id, wire_id, route, after_seq, out_tx);
        return;
    }
    state.with_mut(|s| match after_seq {
        None => s.subscribe_events(client_id, terminal, out_tx.clone()),
        Some(after_seq) => s.subscribe_events_after(client_id, terminal, after_seq, out_tx.clone()),
    });
    ensure_event_pump(state, client_id);
}

/// Satellite-scoped `SUBSCRIBE_EVENTS`: register a hub-side proxy subscriber
/// and forward the frame over the owning link. A missing route is a typed
/// `ERROR` push. Relayed events take this hub's `seq` and are not retained,
/// so a cursor on a satellite scope is owed a `journal_gap`.
fn subscribe_via_hub(
    state: &SharedState,
    client_id: ClientId,
    wire_id: &WireResourceId,
    (host, id): (phux_protocol::ids::SatelliteHost, u32),
    after_seq: Option<u64>,
    out_tx: &mpsc::Sender<Outbound>,
) {
    let Some(relay) = state.with(|s| s.hub_relay(&host)) else {
        warn!(
            ?client_id,
            satellite = %host,
            "SUBSCRIBE_EVENTS: no route to satellite; refusing subscription"
        );
        push_error(
            out_tx,
            ErrorCode::UnsupportedSatelliteRoute,
            format!(
                "no satellite route to {host:?}: this server is not a federation hub for that host"
            ),
        );
        return;
    };
    let Some(consumer_cancel) = state.with(|s| s.client_connection_cancellation(client_id)) else {
        push_error(
            out_tx,
            ErrorCode::InternalError,
            "client connection cancellation is unavailable",
        );
        return;
    };
    let change = state.with_mut(|s| {
        s.subscribe_satellite_events(
            client_id,
            wire_id.clone(),
            crate::state::EventFilter::all(),
            after_seq,
            out_tx.clone(),
        )
    });
    // Either both registrations happen or the consumer gets an error push.
    let forwarded = relay.subscribe(
        crate::hub::relay::ProxySubscription {
            terminal: id,
            client: client_id,
            out_tx: out_tx.clone(),
            consumer_cancel,
            // Stamped by `subscribe` at enqueue.
            seq: 0,
            awaits_snapshot: false,
            resource_kind: None,
            bootstrap_profile: None,
            bootstrap_limits: None,
        },
        FrameKind::SubscribeEvents {
            terminal: Some(WireResourceId::local(id)),
            after_seq: None,
        },
    );
    if !forwarded {
        // Put the registry scope back as it was.
        state.with_mut(|s| s.restore_satellite_scope(client_id, change));
        return;
    }
    ensure_event_pump(state, client_id);
}

/// Push an uncorrelated `ERROR` without waiting; a full mailbox drops it.
fn push_error(out_tx: &mpsc::Sender<Outbound>, code: ErrorCode, message: impl Into<String>) {
    let _ = out_tx.try_send(Outbound::Frame(FrameKind::Error {
        request_id: None,
        code,
        message: message.into(),
    }));
}

/// Whether `terminal_id` resolves, on this server, to an agent session;
/// `false` for unknown or satellite ids, which other handlers answer.
fn is_agent_session(state: &SharedState, terminal_id: &WireResourceId) -> bool {
    state.with(|s| {
        s.terminal_from_wire(terminal_id)
            .and_then(|core| s.resource_handle(core))
            .is_some_and(|handle| handle.kind == crate::resource::ResourceKind::AgentSession)
    })
}

/// Journal an unattributed server-driven [`AgentEvent`] about `terminal`
/// (`None` for server-scoped) and fan it out (ADR-0123).
pub(crate) fn broadcast_event(
    state: &SharedState,
    terminal: Option<&WireResourceId>,
    event: &AgentEvent,
) {
    // ADR-0155: every ask source funnels through here, so this is where an
    // absent phone hears about it.
    if let (Some(pane), AgentEvent::Asked { id, .. }) = (terminal, event) {
        crate::push::on_asked(state, pane, id);
    }
    journal_event(
        state,
        crate::state::EventRecord::new(terminal.cloned(), event.clone()),
    );
}

/// Journal `record` and offer it to every subscription (ADR-0123) in its
/// own short lock. A full mailbox is owed a `journal_gap`.
pub(crate) fn journal_event(state: &SharedState, record: crate::state::EventRecord) {
    trace!(terminal = ?record.terminal, event = ?record.event, "EVENT: journaling");
    let _ = state.with_mut(|s| s.record_and_fanout(record));
}

/// Upper bound on queued outbound messages coalesced into one transport
/// write, so a saturated mailbox cannot starve the close-control arm.
const MAX_WRITE_COALESCE: usize = 32;

/// The one generation a client-side terminal may currently receive.
///
/// Producers enqueue from different tasks, and a reserved permit guarantees
/// capacity, not order, so a history reply can land behind the tombstone that
/// retired it. The writer is the last common ordering point and drops any
/// generation-bound frame that no longer names the live generation.
#[derive(Clone, Copy, Debug)]
struct OutboundGeneration {
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    retired: bool,
}

impl OutboundGeneration {
    fn same_identity(
        self,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> bool {
        self.stream_id == stream_id && self.bootstrap_id == bootstrap_id
    }
}

#[derive(Clone, Copy, Debug)]
enum OutboundTerminalState {
    Generation(OutboundGeneration),
    Closed,
}

#[derive(Debug, Default)]
struct OutboundGenerationFence {
    terminals: HashMap<WireResourceId, OutboundTerminalState>,
    diagnostics: Option<crate::stream_diagnostics::StreamTracker>,
}

impl OutboundGenerationFence {
    /// Admit one mailbox item into the wire stream, applying generation
    /// transitions before the next queued item is considered.
    fn admits(&mut self, message: &Outbound) -> bool {
        let Outbound::Frame(frame) = message else {
            return true;
        };
        match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } => self.admit_begin(terminal_id, *stream_id, *bootstrap_id),
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                ..
            } => {
                let already_retired = self
                    .current(terminal_id)
                    .is_some_and(|current| current.retired);
                let admitted = self.admit_tombstone(terminal_id, *stream_id, *bootstrap_id);
                if admitted
                    && !already_retired
                    && let Some(tracker) = &self.diagnostics
                {
                    tracker.record_tombstone(*reason);
                }
                admitted
            }
            FrameKind::ResourceClosed { terminal_id, .. } => {
                self.terminals
                    .insert(terminal_id.clone(), OutboundTerminalState::Closed);
                true
            }
            _ => generation_of_outbound_frame(frame).is_none_or(
                |(terminal_id, stream_id, bootstrap_id)| {
                    self.admit_data(terminal_id, stream_id, bootstrap_id)
                },
            ),
        }
    }

    fn admit_begin(
        &mut self,
        terminal_id: &WireResourceId,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> bool {
        if self.is_closed(terminal_id)
            || self.current(terminal_id).is_some_and(|current| {
                current.retired && current.same_identity(stream_id, bootstrap_id)
            })
        {
            return false;
        }
        self.terminals.insert(
            terminal_id.clone(),
            OutboundTerminalState::Generation(OutboundGeneration {
                stream_id,
                bootstrap_id,
                retired: false,
            }),
        );
        true
    }

    fn admit_tombstone(
        &mut self,
        terminal_id: &WireResourceId,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> bool {
        if self.is_closed(terminal_id)
            || self
                .current(terminal_id)
                .is_some_and(|current| !current.same_identity(stream_id, bootstrap_id))
        {
            return false;
        }
        self.terminals.insert(
            terminal_id.clone(),
            OutboundTerminalState::Generation(OutboundGeneration {
                stream_id,
                bootstrap_id,
                retired: true,
            }),
        );
        true
    }

    fn admit_data(
        &self,
        terminal_id: &WireResourceId,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> bool {
        !self.is_closed(terminal_id)
            && self.current(terminal_id).is_none_or(|current| {
                !current.retired && current.same_identity(stream_id, bootstrap_id)
            })
    }

    fn current(&self, terminal_id: &WireResourceId) -> Option<OutboundGeneration> {
        match self.terminals.get(terminal_id) {
            Some(OutboundTerminalState::Generation(generation)) => Some(*generation),
            Some(OutboundTerminalState::Closed) | None => None,
        }
    }

    fn is_closed(&self, terminal_id: &WireResourceId) -> bool {
        matches!(
            self.terminals.get(terminal_id),
            Some(OutboundTerminalState::Closed)
        )
    }
}

/// The generation identity carried by a server-to-client data frame. BEGIN
/// and the bootstrap tombstone are transitions [`OutboundGenerationFence::admits`]
/// handles itself.
const fn generation_of_outbound_frame(
    frame: &FrameKind,
) -> Option<(
    &WireResourceId,
    phux_protocol::ids::StreamId,
    phux_protocol::ids::BootstrapId,
)> {
    match frame {
        FrameKind::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            ..
        }
        | FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            ..
        }
        | FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            ..
        }
        | FrameKind::HistoryTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            ..
        }
        | FrameKind::HistoryRejected {
            terminal_id,
            stream_id,
            bootstrap_id,
            ..
        }
        | FrameKind::ResourceOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            ..
        } => Some((terminal_id, *stream_id, *bootstrap_id)),
        _ => None,
    }
}

#[cfg(test)]
mod outbound_generation_fence_tests {
    use bytes::Bytes;
    use phux_protocol::caps::BootstrapStreamProfile;
    use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
    use phux_protocol::wire::frame::{FrameKind, HistoryTombstoneReason, TombstoneReason};

    use super::{Outbound, OutboundGenerationFence};

    fn terminal() -> ResourceId {
        ResourceId::local(1)
    }

    fn stream() -> StreamId {
        StreamId::new(2).expect("stream id")
    }

    fn bootstrap(raw: u64) -> BootstrapId {
        BootstrapId::new(raw).expect("bootstrap id")
    }

    fn begin(bootstrap_id: BootstrapId) -> Outbound {
        Outbound::Frame(FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        })
    }

    fn tombstone(bootstrap_id: BootstrapId, reason: TombstoneReason) -> Outbound {
        Outbound::Frame(FrameKind::BootstrapTombstone {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id,
            reason,
            last_valid_seq: 1,
        })
    }

    fn output(bootstrap_id: BootstrapId) -> Outbound {
        Outbound::Frame(FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id,
            seq: 1,
            bytes: Bytes::from_static(b"output"),
        })
    }

    #[test]
    fn tombstone_fences_late_history_and_output_before_replacement() {
        use crate::stream_diagnostics::{
            ResyncReason, StreamContext, StreamDiagnostics, StreamLane,
        };
        let diagnostics = StreamDiagnostics::new();
        let registration = diagnostics.register(StreamContext {
            connection_id: 1,
            stream_id: 4,
            lane: StreamLane::Terminal,
        });
        let initial = bootstrap(1);
        let replacement = bootstrap(2);
        let mut fence = OutboundGenerationFence {
            diagnostics: Some(registration.tracker()),
            ..OutboundGenerationFence::default()
        };
        assert!(fence.admits(&begin(initial)));
        assert!(fence.admits(&output(initial)));
        assert!(fence.admits(&tombstone(initial, TombstoneReason::Resize)));

        assert!(!fence.admits(&Outbound::Frame(FrameKind::HistoryPage {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: initial,
            page_seq: 1,
            cursor: Bytes::from_static(b"old"),
            next_cursor: None,
            payload: Bytes::from_static(b"late history"),
            rows: 1,
        })));
        assert!(!fence.admits(&Outbound::Frame(FrameKind::HistoryTombstone {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: initial,
            cursor: Bytes::from_static(b"old"),
            reason: HistoryTombstoneReason::Stale,
        },)));
        assert!(!fence.admits(&output(initial)));
        assert!(!fence.admits(&begin(initial)));

        assert!(fence.admits(&tombstone(initial, TombstoneReason::OutboundGap)));
        assert_eq!(
            diagnostics.snapshot().streams[0].resync_reason,
            Some(ResyncReason::Resize)
        );

        assert!(fence.admits(&begin(replacement)));
        assert!(fence.admits(&output(replacement)));
        assert!(!fence.admits(&output(initial)));
        assert!(!fence.admits(&tombstone(initial, TombstoneReason::OutboundGap)));
        assert_eq!(
            diagnostics.snapshot().streams[0].resync_reason,
            Some(ResyncReason::Resize)
        );
    }
}

/// Encode one outbound message onto the end of `batch`, recording its frame
/// boundary in `ends`. Returns the message of an [`Outbound::TerminalError`],
/// after which the caller must stop draining.
fn encode_into_batch(
    message: Outbound,
    compression: Compression,
    scratch: &mut BytesMut,
    batch: &mut BytesMut,
    ends: &mut Vec<usize>,
) -> Option<String> {
    let (frame, terminal_message) = match message {
        Outbound::Frame(frame) => (frame, None),
        Outbound::TerminalError {
            request_id,
            code,
            message,
        } => (
            FrameKind::Error {
                request_id,
                code,
                message: message.clone(),
            },
            Some(message),
        ),
    };
    let encode_started = std::time::Instant::now();
    frame.encode_compressed(compress_policy(&frame, compression), scratch, batch);
    crate::perf::WIRE_ENCODE.record_elapsed(encode_started);
    ends.push(batch.len());
    terminal_message
}

/// Which frames are worth wrapping (proto.md §6.4): bootstrap and history
/// payloads only. Those are large and gate first paint; live
/// `RESOURCE_OUTPUT` measured slower when compressed.
const fn compress_policy(frame: &FrameKind, negotiated: Compression) -> Compression {
    match frame {
        FrameKind::BootstrapChunk { .. } | FrameKind::HistoryPage { .. } => negotiated,
        _ => Compression::None,
    }
}

/// Absorb up to [`MAX_WRITE_COALESCE`] further messages that are *already*
/// queued into the same batch. Stops on an empty or closed mailbox, and on a
/// terminal-error sentinel (returned so the caller can finish the shutdown
/// handshake).
fn drain_ready_into_batch(
    rx: &mut tokio::sync::mpsc::Receiver<Outbound>,
    generation_fence: &mut OutboundGenerationFence,
    compression: Compression,
    scratch: &mut BytesMut,
    batch: &mut BytesMut,
    ends: &mut Vec<usize>,
) -> Option<String> {
    for _ in 1..MAX_WRITE_COALESCE {
        let Ok(next) = rx.try_recv() else {
            return None;
        };
        if !generation_fence.admits(&next) {
            continue;
        }
        if let Some(message) = encode_into_batch(next, compression, scratch, batch, ends) {
            return Some(message);
        }
    }
    None
}

/// How a writer learns that its connection's authority was withdrawn.
#[derive(Clone, Default)]
pub(crate) struct RevocationWatch {
    rx: Option<tokio::sync::watch::Receiver<Option<Goodbye>>>,
    /// Whether this writer carries the connection's control stream, the one
    /// the goodbye frames ride. A bound Terminal stream only closes.
    control: bool,
}

impl RevocationWatch {
    /// The watch for the connection's control-stream writer.
    const fn control(rx: tokio::sync::watch::Receiver<Option<Goodbye>>) -> Self {
        Self {
            rx: Some(rx),
            control: true,
        }
    }

    /// The watch for a bound Terminal stream's writer.
    const fn stream(rx: tokio::sync::watch::Receiver<Option<Goodbye>>) -> Self {
        Self {
            rx: Some(rx),
            control: false,
        }
    }

    /// The goodbye owed, once the connection is revoked.
    fn current(&self) -> Option<Goodbye> {
        self.rx.as_ref().and_then(|rx| *rx.borrow())
    }

    /// Resolve once the connection is revoked; never, without a signal.
    async fn revoked(&mut self) -> Goodbye {
        let Some(rx) = self.rx.as_mut() else {
            return std::future::pending().await;
        };
        loop {
            let current = *rx.borrow_and_update();
            if let Some(goodbye) = current {
                return goodbye;
            }
            if rx.changed().await.is_err() {
                return std::future::pending().await;
            }
        }
    }

    /// Run one transport operation unless the connection is revoked while
    /// it is pending. `None` means the operation was abandoned mid-way.
    async fn unless_revoked<T>(
        &mut self,
        operation: impl std::future::Future<Output = io::Result<T>>,
    ) -> Option<io::Result<T>> {
        tokio::select! {
            biased;
            result = operation => Some(result),
            _ = self.revoked() => None,
        }
    }
}

/// Why a writer stops taking messages.
enum WriterStop {
    /// The mailbox is done: close normally.
    Closed,
    /// The connection's authority was withdrawn.
    Revoked(Goodbye),
}

/// The next message the generation fence admits, unless the mailbox closes
/// or the connection is revoked first. Revocation wins a tie.
async fn next_admitted(
    rx: &mut tokio::sync::mpsc::Receiver<Outbound>,
    close: &mut tokio::sync::watch::Receiver<bool>,
    close_control_open: &mut bool,
    generation_fence: &mut OutboundGenerationFence,
    revocation: &mut RevocationWatch,
) -> Result<Outbound, WriterStop> {
    loop {
        let next = tokio::select! {
            biased;
            goodbye = revocation.revoked() => return Err(WriterStop::Revoked(goodbye)),
            next = next_outbound(rx, close, close_control_open) => next,
        };
        let message = next.ok_or(WriterStop::Closed)?;
        if generation_fence.admits(&message) {
            return Ok(message);
        }
    }
}

/// The frames a revoked connection is owed (`workload-auth.md` §7 step 3).
fn goodbye_frames(revocation: Revocation) -> [FrameKind; 2] {
    [
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::PermissionDenied,
            message: revocation.message().to_owned(),
        },
        FrameKind::Detached {
            reason: Some(revocation.detach_reason()),
            message: revocation.message().to_owned(),
        },
    ]
}

/// Say what a revoked connection is owed, then close (`workload-auth.md` §7
/// steps 3 and 4). Nothing queued behind the revocation is written: the
/// mailbox drops with this task. Each step is bounded by
/// [`WRITER_DRAIN_TIMEOUT`], so a peer that stopped reading cannot hold the
/// connection open.
async fn say_goodbye<W: FrameWriter>(
    writer: &mut W,
    buf: &mut BytesMut,
    goodbye: Goodbye,
    control: bool,
    client_id: ClientId,
) {
    if let (true, Goodbye::Announce(revocation)) = (control, goodbye) {
        buf.clear();
        let mut ends = Vec::with_capacity(2);
        for frame in goodbye_frames(revocation) {
            frame.encode(buf);
            ends.push(buf.len());
        }
        let farewell = async {
            writer.write_frames(buf, &ends).await?;
            writer.flush().await
        };
        if !matches!(
            tokio::time::timeout(WRITER_DRAIN_TIMEOUT, farewell).await,
            Ok(Ok(()))
        ) {
            debug!(
                ?client_id,
                "goodbye to a revoked connection was not delivered"
            );
        }
    }
    let _ = tokio::time::timeout(WRITER_DRAIN_TIMEOUT, writer.close()).await;
    debug!(?client_id, ?goodbye, "writer closed: authority withdrawn");
}

/// Settle one transport write or flush. `false` when the writer must stop:
/// the step failed, or a revocation interrupted it (`outcome` is `None`);
/// either way the transport is closed.
async fn step_succeeded<W: FrameWriter>(
    writer: &mut W,
    outcome: Option<io::Result<()>>,
    client_id: ClientId,
    step: &'static str,
) -> bool {
    match outcome {
        Some(Ok(())) => true,
        Some(Err(err)) => {
            debug!(?client_id, error = %err, step, "writer step failed; client task ending");
            let _ = writer.close().await;
            false
        }
        None => {
            abandon_after_revocation(writer, client_id).await;
            false
        }
    }
}

/// A write the revocation interrupted may have stopped mid-frame, so no
/// goodbye can follow it: close within the bound.
async fn abandon_after_revocation<W: FrameWriter>(writer: &mut W, client_id: ClientId) {
    debug!(?client_id, "revoked mid-write; closing without a goodbye");
    let _ = tokio::time::timeout(WRITER_DRAIN_TIMEOUT, writer.close()).await;
}

/// Take the next mailbox item, honouring the close control.
///
/// Clears `close_control_open` once the control has fired or gone away, so
/// later turns wait on the mailbox alone. `None` means the mailbox is done.
async fn next_outbound(
    rx: &mut tokio::sync::mpsc::Receiver<Outbound>,
    close: &mut tokio::sync::watch::Receiver<bool>,
    close_control_open: &mut bool,
) -> Option<Outbound> {
    loop {
        if !*close_control_open {
            return rx.recv().await;
        }
        tokio::select! {
            biased;
            changed = close.changed() => {
                if changed.is_ok() && *close.borrow_and_update() {
                    rx.close();
                }
                *close_control_open = false;
            }
            message = rx.recv() => return message,
        }
    }
}

/// Write the `DETACHED` that follows an ordered terminal `ERROR`, then flush
/// (a buffering transport could otherwise lose both) and close.
async fn finish_with_terminal_error<W: FrameWriter>(
    writer: &mut W,
    buf: &mut BytesMut,
    message: String,
    client_id: ClientId,
) {
    buf.clear();
    FrameKind::Detached {
        reason: Some(DetachReason::ProtocolError),
        message,
    }
    .encode(buf);
    if let Err(err) = writer.write_frame(buf).await {
        debug!(?client_id, error = %err, "writer error on terminal DETACHED");
    }
    let _ = writer.flush().await;
    let _ = writer.close().await;
}

/// Writer task: drain the outbound mailbox into the transport, batching what
/// is already queued, until the mailbox closes. Once `revocation` fires,
/// nothing still queued is written; the goodbye is, and the transport closes
/// (`docs/spec/workload-auth.md` §7).
async fn writer_task<W: FrameWriter>(
    mut writer: W,
    mut rx: tokio::sync::mpsc::Receiver<Outbound>,
    mut close: tokio::sync::watch::Receiver<bool>,
    compression: Arc<AtomicU8>,
    client_id: ClientId,
    mut revocation: RevocationWatch,
) {
    let mut buf = BytesMut::with_capacity(1024);
    // Uncompressed image of a frame being wrapped.
    let mut scratch = BytesMut::new();
    // End offset of each frame in `buf`, for message-oriented transports.
    let mut ends: Vec<usize> = Vec::new();
    let mut close_control_open = true;
    let mut generation_fence = OutboundGenerationFence {
        diagnostics: writer.stream_tracker(),
        ..OutboundGenerationFence::default()
    };
    'writer: loop {
        let message = match next_admitted(
            &mut rx,
            &mut close,
            &mut close_control_open,
            &mut generation_fence,
            &mut revocation,
        )
        .await
        {
            Ok(message) => message,
            Err(WriterStop::Closed) => break 'writer,
            Err(WriterStop::Revoked(goodbye)) => {
                say_goodbye(
                    &mut writer,
                    &mut buf,
                    goodbye,
                    revocation.control,
                    client_id,
                )
                .await;
                return;
            }
        };
        // One write per batch of already-queued messages; the drain never
        // waits, so a lone keystroke echo adds no latency.
        buf.clear();
        ends.clear();
        // An uncompressed batch is always legal, so `Relaxed` suffices.
        let compression = Compression::from_u8(compression.load(Ordering::Relaxed));
        let mut terminal_message =
            encode_into_batch(message, compression, &mut scratch, &mut buf, &mut ends);
        if terminal_message.is_none() {
            terminal_message = drain_ready_into_batch(
                &mut rx,
                &mut generation_fence,
                compression,
                &mut scratch,
                &mut buf,
                &mut ends,
            );
        }
        crate::perf::WIRE_BATCH_FRAMES.record_len(ends.len());
        // Revoked while gathering: none of the batch is owed.
        if let Some(goodbye) = revocation.current() {
            say_goodbye(
                &mut writer,
                &mut buf,
                goodbye,
                revocation.control,
                client_id,
            )
            .await;
            return;
        }
        let write_started = std::time::Instant::now();
        let batch_len = buf.len();
        let written = revocation
            .unless_revoked(writer.write_frames(&mut buf, &ends))
            .await;
        if !step_succeeded(&mut writer, written, client_id, "write").await {
            return;
        }
        if let Some(message) = terminal_message {
            finish_with_terminal_error(&mut writer, &mut buf, message, client_id).await;
            return;
        }
        // `write_frames` may only buffer (WebSocket does); flush once per batch.
        let flushed = revocation.unless_revoked(writer.flush()).await;
        if !step_succeeded(&mut writer, flushed, client_id, "flush").await {
            return;
        }
        phux_perf::echo_hop::record("frame_send");
        crate::perf::WIRE_WRITE.record_elapsed(write_started);
        crate::perf::WIRE_WRITE_BYTES.record_len(batch_len);
        crate::perf::WIRE_BYTES_OUT.add_len(batch_len);
    }
    if let Err(err) = writer.close().await {
        debug!(?client_id, error = %err, "writer close failed");
    }
    debug!(?client_id, "writer task exiting (channel closed)");
}

#[cfg(test)]
mod writer_close_tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;
    use std::rc::Rc;

    use bytes::BytesMut;
    use phux_protocol::ids::StreamId;
    use phux_protocol::policy::TransportType;
    use phux_protocol::wire::frame::{DetachReason, ErrorCode, FrameKind};
    use tokio::sync::mpsc;
    use tokio::task::LocalSet;
    use tokio_util::sync::CancellationToken;

    use super::{RevocationWatch, handle_client};
    use crate::state::{ClientId, Outbound, SharedState};
    use crate::transport::{FrameReader, FrameWriter};

    /// What a test transport was asked to do, in order.
    #[derive(Debug)]
    enum Wrote {
        Frame(Vec<u8>),
        Flush,
        Close,
    }

    /// The frames and lifecycle a writer produced, coarsened for asserting.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum WriterEvent {
        Frame,
        Error,
        Detached(Option<DetachReason>),
        Close,
    }

    /// A transport that records everything, using the default per-frame
    /// `write_frames` slicing a message-oriented transport relies on.
    #[derive(Clone, Default)]
    struct Recorder(Rc<RefCell<Vec<Wrote>>>);

    impl Recorder {
        fn slices(&self) -> Vec<Vec<u8>> {
            self.0
                .borrow()
                .iter()
                .filter_map(|wrote| match wrote {
                    Wrote::Frame(bytes) => Some(bytes.clone()),
                    Wrote::Flush | Wrote::Close => None,
                })
                .collect()
        }

        fn events(&self) -> Vec<WriterEvent> {
            self.0
                .borrow()
                .iter()
                .filter_map(|wrote| match wrote {
                    Wrote::Frame(bytes) => Some(match FrameKind::decode(bytes).expect("frame").0 {
                        FrameKind::Error { .. } => WriterEvent::Error,
                        FrameKind::Detached { reason, .. } => WriterEvent::Detached(reason),
                        _ => WriterEvent::Frame,
                    }),
                    Wrote::Flush => None,
                    Wrote::Close => Some(WriterEvent::Close),
                })
                .collect()
        }

        /// How many frames each flushed batch carried.
        fn batches(&self) -> Vec<usize> {
            let mut batches = Vec::new();
            let mut pending = 0;
            for wrote in self.0.borrow().iter() {
                match wrote {
                    Wrote::Frame(_) => pending += 1,
                    Wrote::Flush => batches.push(std::mem::take(&mut pending)),
                    Wrote::Close => {}
                }
            }
            batches
        }
    }

    #[allow(
        clippy::unused_async_trait_impl,
        reason = "the recording test writer implements the production async transport trait without I/O"
    )]
    impl FrameWriter for Recorder {
        async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
            self.0.borrow_mut().push(Wrote::Frame(frame.to_vec()));
            Ok(())
        }

        async fn flush(&mut self) -> io::Result<()> {
            self.0.borrow_mut().push(Wrote::Flush);
            Ok(())
        }

        async fn close(&mut self) -> io::Result<()> {
            self.0.borrow_mut().push(Wrote::Close);
            Ok(())
        }
    }

    /// Scripted client frames, then pending forever (or EOF when `eof`).
    pub(super) struct Script {
        frames: VecDeque<BytesMut>,
        eof: bool,
    }

    impl Script {
        pub(super) fn new(frames: impl IntoIterator<Item = FrameKind>) -> Self {
            Self {
                frames: frames
                    .into_iter()
                    .map(|frame| {
                        let mut encoded = BytesMut::new();
                        frame.encode(&mut encoded);
                        encoded
                    })
                    .collect(),
                eof: false,
            }
        }
    }

    impl FrameReader for Script {
        async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
            if let Some(frame) = self.frames.pop_front() {
                return Ok(Some(frame));
            }
            if self.eof {
                return Ok(None);
            }
            std::future::pending().await
        }
    }

    /// Run the writer over `frames` already queued and a closed mailbox.
    async fn write_all(frames: impl IntoIterator<Item = Outbound>) -> Recorder {
        let recorder = Recorder::default();
        let (tx, rx) = mpsc::channel(64);
        for frame in frames {
            tx.try_send(frame).expect("queue frame");
        }
        drop(tx);
        let (_close_tx, close_rx) = tokio::sync::watch::channel(false);
        writer(recorder.clone(), rx, close_rx).await;
        recorder
    }

    fn writer<W: FrameWriter>(
        transport: W,
        rx: mpsc::Receiver<Outbound>,
        close: tokio::sync::watch::Receiver<bool>,
    ) -> impl std::future::Future<Output = ()> {
        super::writer_task(
            transport,
            rx,
            close,
            std::sync::Arc::new(std::sync::atomic::AtomicU8::new(
                phux_protocol::caps::Compression::None.as_u8(),
            )),
            ClientId(1),
            RevocationWatch::default(),
        )
    }

    fn pong(nonce: u64) -> Outbound {
        Outbound::Frame(FrameKind::Pong { nonce })
    }

    struct StalledWriter {
        entered: std::sync::Arc<tokio::sync::Notify>,
        dropped: Rc<std::cell::Cell<bool>>,
    }

    impl Drop for StalledWriter {
        fn drop(&mut self) {
            self.dropped.set(true);
        }
    }

    impl FrameWriter for StalledWriter {
        async fn write_frame(&mut self, _frame: &[u8]) -> io::Result<()> {
            self.entered.notify_one();
            std::future::pending().await
        }

        async fn close(&mut self) -> io::Result<()> {
            std::future::pending().await
        }
    }

    fn binding(
        tx: mpsc::Sender<Outbound>,
        writer: tokio::task::JoinSet<()>,
        writer_close: tokio::sync::watch::Sender<bool>,
    ) -> super::StreamBinding {
        super::StreamBinding {
            tx,
            stream_id: StreamId::new(1).expect("stream id"),
            writer,
            writer_close,
            diagnostics: None,
            ingress_active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ingress_cancel: CancellationToken::new(),
        }
    }

    /// Retirement is bounded: a writer stalled on a peer that stopped
    /// reading is reaped even while another sender keeps the mailbox open.
    #[tokio::test(flavor = "current_thread")]
    async fn retired_stream_reaps_stalled_writer_with_extra_sender() {
        LocalSet::new()
            .run_until(async {
                let entered = std::sync::Arc::new(tokio::sync::Notify::new());
                let dropped = Rc::new(std::cell::Cell::new(false));
                let (tx, rx) = mpsc::channel(1);
                let extra_sender = tx.clone();
                let (close_tx, close_rx) = tokio::sync::watch::channel(false);
                let mut tasks = tokio::task::JoinSet::new();
                let stalled = StalledWriter {
                    entered: entered.clone(),
                    dropped: dropped.clone(),
                };
                tasks.spawn_local(writer(stalled, rx, close_rx));
                tx.send(pong(1)).await.expect("queue");
                tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
                    .await
                    .expect("writer entered");
                let mut binding = binding(tx, tasks, close_tx);
                let ingress_active = binding.ingress_active.clone();
                let ingress_cancel = binding.ingress_cancel.clone();
                tokio::time::timeout(std::time::Duration::from_secs(2), binding.retire())
                    .await
                    .expect("bounded retirement");
                assert!(dropped.get(), "retirement must reap the transport writer");
                assert!(binding.writer.is_empty());
                assert!(!ingress_active.load(std::sync::atomic::Ordering::Acquire));
                assert!(ingress_cancel.is_cancelled());
                assert!(extra_sender.is_closed());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retired_stream_drains_queued_frames_before_close() {
        LocalSet::new()
            .run_until(async {
                let recorder = Recorder::default();
                let (tx, rx) = mpsc::channel(2);
                let (writer_close, close_rx) = tokio::sync::watch::channel(false);
                tx.try_send(pong(1)).expect("queue");
                tx.try_send(pong(2)).expect("queue");
                let extra_sender = tx.clone();
                let mut tasks = tokio::task::JoinSet::new();
                tasks.spawn_local(writer(recorder.clone(), rx, close_rx));
                let mut binding = binding(tx, tasks, writer_close);
                binding.retire().await;
                assert_eq!(
                    recorder.events(),
                    [WriterEvent::Frame, WriterEvent::Frame, WriterEvent::Close]
                );
                assert!(extra_sender.is_closed());
                assert!(binding.writer.is_empty());
            })
            .await;
    }

    /// An already-queued burst costs one transport write and flush.
    #[tokio::test(flavor = "current_thread")]
    async fn a_queued_burst_costs_one_transport_write() {
        let recorder = write_all((0..8).map(pong)).await;
        assert_eq!(recorder.batches(), [8]);
    }

    /// Batching coalesces writes, never frames: each slice a message-oriented
    /// transport sends is exactly one well-formed frame, including one near
    /// a megabyte beside tiny ones.
    #[tokio::test(flavor = "current_thread")]
    async fn a_batch_slices_back_into_exactly_the_frames_that_went_in() {
        let sent: Vec<FrameKind> = (0..8)
            .chain([20])
            .map(|shift| FrameKind::Error {
                request_id: None,
                code: ErrorCode::InternalError,
                message: "x".repeat(1 << shift),
            })
            .collect();
        let recorder = write_all(sent.iter().cloned().map(Outbound::Frame)).await;
        let slices = recorder.slices();
        assert_eq!(slices.len(), sent.len());
        for (slice, original) in slices.iter().zip(&sent) {
            phux_protocol::wire::framing::check_frame(slice).expect("one well-formed frame");
            let (decoded, rest) = FrameKind::decode(slice).expect("slice decodes");
            assert!(rest.is_empty(), "a slice must not carry a second frame");
            assert_eq!(format!("{decoded:?}"), format!("{original:?}"));
        }
    }

    /// A lone frame leaves on its own write; nothing lingers for company.
    #[tokio::test(flavor = "current_thread")]
    async fn a_lone_frame_is_written_without_waiting_for_company() {
        LocalSet::new()
            .run_until(async {
                let recorder = Recorder::default();
                let (tx, rx) = mpsc::channel(8);
                let (_close_tx, close_rx) = tokio::sync::watch::channel(false);
                let task = tokio::task::spawn_local(writer(recorder.clone(), rx, close_rx));
                tx.send(pong(1)).await.expect("queue frame");
                for _ in 0..16 {
                    if !recorder.batches().is_empty() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert_eq!(recorder.batches(), [1]);
                drop(tx);
                task.await.expect("writer task");
            })
            .await;
    }

    /// Nothing queued behind a terminal error is written; the ERROR and its
    /// DETACHED precede the close.
    #[tokio::test(flavor = "current_thread")]
    async fn terminal_error_is_written_before_transport_close() {
        let recorder = write_all([
            pong(1),
            Outbound::TerminalError {
                request_id: None,
                code: ErrorCode::CodecUnavailable,
                message: "fatal native stream failure".to_owned(),
            },
            pong(2),
        ])
        .await;
        assert_eq!(
            recorder.events(),
            [
                WriterEvent::Frame,
                WriterEvent::Error,
                WriterEvent::Detached(Some(DetachReason::ProtocolError)),
                WriterEvent::Close
            ],
        );
    }

    async fn client(
        reader: impl FrameReader + 'static,
        token: CancellationToken,
        root_token: CancellationToken,
    ) -> Recorder {
        let recorder = Recorder::default();
        Box::pin(handle_client(
            reader,
            recorder.clone(),
            SharedState::new(),
            ClientId(9),
            token,
            root_token,
            None,
            TransportType::UnixSocket,
            false,
        ))
        .await
        .expect("clean close");
        recorder
    }

    /// However a connection ends, its token fires, so no per-connection task
    /// outlives it.
    #[tokio::test(flavor = "current_thread")]
    async fn eof_cancels_the_connection_token() {
        LocalSet::new()
            .run_until(async {
                let token = CancellationToken::new();
                let mut eof = Script::new([]);
                eof.eof = true;
                client(eof, token.clone(), CancellationToken::new()).await;
                assert!(token.is_cancelled());
            })
            .await;
    }

    /// Run a negotiated connection over `frames` (HELLO first, EOF last)
    /// and decode everything the server wrote, plus whether it closed.
    async fn negotiated(frames: impl IntoIterator<Item = FrameKind>) -> (Vec<FrameKind>, bool) {
        let state = SharedState::new();
        let client_id = state.with_mut(|server| {
            let client_id = server.new_client_id();
            server.set_peer_identity(
                client_id,
                phux_protocol::policy::PeerIdentity {
                    uid: 0,
                    pid: None,
                    exe_path: None,
                    mcp_host_key: None,
                    transport: TransportType::UnixSocket,
                    source_addr: None,
                },
            );
            client_id
        });
        let hello = FrameKind::Hello {
            client_name: "dispatch-characterization".to_owned(),
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            client_caps: phux_protocol::caps::ClientCapabilities::new(),
        };
        let mut script = Script::new(std::iter::once(hello).chain(frames));
        script.eof = true;
        let recorder = Recorder::default();
        LocalSet::new()
            .run_until(Box::pin(handle_client(
                script,
                recorder.clone(),
                state,
                client_id,
                CancellationToken::new(),
                CancellationToken::new(),
                None,
                TransportType::UnixSocket,
                false,
            )))
            .await
            .expect("clean close");
        let decoded = recorder
            .slices()
            .iter()
            .map(|slice| FrameKind::decode(slice).expect("frame").0)
            .collect();
        let closed = recorder.events().last() == Some(&WriterEvent::Close);
        (decoded, closed)
    }

    /// A negotiated PING is answered and the connection stays up until EOF.
    #[tokio::test(flavor = "current_thread")]
    async fn a_negotiated_ping_is_answered_and_keeps_the_connection() {
        let (frames, closed) = negotiated([FrameKind::Ping { nonce: 7 }]).await;
        assert!(
            matches!(frames.first(), Some(FrameKind::HelloOk { .. })),
            "{frames:?}"
        );
        assert!(
            frames
                .iter()
                .any(|frame| matches!(frame, FrameKind::Pong { nonce: 7 })),
            "{frames:?}"
        );
        assert!(
            !frames
                .iter()
                .any(|frame| matches!(frame, FrameKind::Error { .. }))
        );
        assert!(!closed, "EOF leaves nobody to close for");
    }

    /// A server-to-client kind from a client is direction-invalid: ERROR,
    /// DETACHED, close.
    #[tokio::test(flavor = "current_thread")]
    async fn a_server_frame_from_a_client_closes_the_connection() {
        let (frames, closed) = negotiated([FrameKind::Pong { nonce: 1 }]).await;
        assert!(
            frames.iter().any(|frame| matches!(
                frame,
                FrameKind::Error { code: ErrorCode::InvalidCommand, message, .. }
                    if message.contains("negotiated phase")
            )),
            "{frames:?}"
        );
        assert!(matches!(
            frames.last(),
            Some(FrameKind::Detached {
                reason: Some(DetachReason::ProtocolError),
                ..
            })
        ));
        assert!(closed);
    }

    /// ATTACH id zero is reserved: the connection closes as malformed.
    #[tokio::test(flavor = "current_thread")]
    async fn a_zero_attach_id_closes_the_connection() {
        let (frames, closed) = negotiated([FrameKind::Attach {
            attach_id: 0,
            target: phux_protocol::wire::frame::AttachTarget::ByName("none".to_owned()),
            viewport: phux_protocol::wire::frame::ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
            role_policy: None,
        }])
        .await;
        assert!(
            frames.iter().any(|frame| matches!(
                frame,
                FrameKind::Error { code: ErrorCode::MalformedMessage, message, .. }
                    if message.contains("attach_id must be nonzero")
            )),
            "{frames:?}"
        );
        assert!(closed);
    }

    /// A reused ATTACH id is refused, but the connection stays up.
    #[tokio::test(flavor = "current_thread")]
    async fn a_reused_attach_id_is_refused_without_closing() {
        let attach = || FrameKind::Attach {
            attach_id: 3,
            target: phux_protocol::wire::frame::AttachTarget::ByName("none".to_owned()),
            viewport: phux_protocol::wire::frame::ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
            role_policy: None,
        };
        let (frames, closed) = Box::pin(negotiated([
            attach(),
            attach(),
            FrameKind::Ping { nonce: 5 },
        ]))
        .await;
        assert!(
            frames.iter().any(|frame| matches!(
                frame,
                FrameKind::Error { code: ErrorCode::MalformedMessage, message, .. }
                    if message.contains("already used on this connection")
            )),
            "{frames:?}"
        );
        assert!(
            frames
                .iter()
                .any(|frame| matches!(frame, FrameKind::Pong { nonce: 5 })),
            "a later frame is still served: {frames:?}"
        );
        assert!(!closed);
    }

    struct PingBeforeHelloReader {
        remaining: u8,
    }

    impl FrameReader for PingBeforeHelloReader {
        async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
            if self.remaining == 0 {
                return std::future::pending().await;
            }
            tokio::time::sleep(crate::transport::HANDSHAKE_DEADLINE / 4).await;
            self.remaining -= 1;
            let mut frame = BytesMut::new();
            FrameKind::Ping {
                nonce: u64::from(self.remaining),
            }
            .encode(&mut frame);
            Ok(Some(frame))
        }
    }

    /// Pre-HELLO PINGs get PONGs but cannot extend the absolute deadline,
    /// which ends in ERROR, DETACHED, close.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn pre_hello_ping_cannot_reset_absolute_handshake_deadline() {
        LocalSet::new()
            .run_until(async {
                let task = tokio::task::spawn_local(client(
                    PingBeforeHelloReader { remaining: 8 },
                    CancellationToken::new(),
                    CancellationToken::new(),
                ));
                tokio::time::advance(crate::transport::HANDSHAKE_DEADLINE).await;
                let events = task.await.expect("client task").events();
                assert!(events.contains(&WriterEvent::Frame));
                assert!(
                    events.ends_with(&[
                        WriterEvent::Error,
                        WriterEvent::Detached(Some(DetachReason::ProtocolError)),
                        WriterEvent::Close,
                    ]),
                    "{events:?}",
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn root_cancellation_flushes_server_shutdown_before_transport_close() {
        LocalSet::new()
            .run_until(async {
                let root_token = CancellationToken::new();
                let task = tokio::task::spawn_local(client(
                    Script::new([]),
                    root_token.child_token(),
                    root_token.clone(),
                ));
                tokio::task::yield_now().await;
                root_token.cancel();
                assert_eq!(
                    task.await.expect("client task").events(),
                    [
                        WriterEvent::Detached(Some(DetachReason::ServerShutdown)),
                        WriterEvent::Close
                    ],
                );
            })
            .await;
    }
}
#[cfg(all(test, feature = "native-engine", not(target_arch = "wasm32")))]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "tests"
)]
mod fatal_preflight_close_tests {
    use std::io;

    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::caps::{
        BootstrapCapabilities, ClientCapabilities, EngineCodec, EngineFeatureSet,
    };
    use phux_protocol::policy::{PeerIdentity, TransportType};
    use phux_protocol::wire::frame::{
        AttachTarget, DetachReason, ErrorCode, FrameKind, ViewportInfo,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{broadcast, mpsc};
    use tokio::task::LocalSet;
    use tokio_util::sync::CancellationToken;

    use super::handle_client;
    use super::writer_close_tests::Script;
    use crate::state::SharedState;
    use crate::terminal_actor::{ConsumerAttachOutcome, PaneOutput};
    use crate::transport::FrameWriter;

    struct DuplexWriter(tokio::io::WriteHalf<tokio::io::DuplexStream>);

    impl FrameWriter for DuplexWriter {
        async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
            self.0.write_all(frame).await
        }

        async fn close(&mut self) -> io::Result<()> {
            self.0.shutdown().await
        }
    }

    /// The next frame the peer reads, or `None` at EOF; one second at most.
    async fn next_frame(
        reader: &mut tokio::io::ReadHalf<tokio::io::DuplexStream>,
    ) -> Option<FrameKind> {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            let mut header = [0_u8; 4];
            match reader.read_exact(&mut header).await {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return None,
                Err(error) => panic!("read: {error}"),
            }
            let mut framed = phux_protocol::wire::framing::frame_buffer(header).expect("frame");
            reader
                .read_exact(&mut framed[phux_protocol::wire::framing::LENGTH_PREFIX_LEN..])
                .await
                .expect("frame body");
            Some(FrameKind::decode(&framed).expect("decode").0)
        })
        .await
        .expect("peer read timed out")
    }

    fn native_failure_handle() -> (
        crate::resource::ResourceHandle,
        mpsc::Receiver<crate::terminal_actor::ConsumerAttachRequest>,
        mpsc::Receiver<crate::terminal_actor::NativeBootstrapRequest>,
    ) {
        let (output, _output_seed) = broadcast::channel::<PaneOutput>(8);
        let (consumer_attach, consumer_attach_rx) = mpsc::channel(8);
        let (native_bootstrap, native_bootstrap_rx) = mpsc::channel(8);
        (
            crate::resource::ResourceHandle {
                kind: crate::resource::ResourceKind::Terminal,
                parent: None,
                output,
                consumer_attach,
                consumer_detach: mpsc::channel(8).0,
                consumer_ack: mpsc::channel(8).0,
                upgrade: mpsc::channel(8).0,
                control: mpsc::channel(8).0,
                facet: crate::resource::ResourceFacetHandle::Terminal(
                    crate::terminal_actor::TerminalHandle {
                        native_bootstrap,
                        ..crate::terminal_actor::TerminalHandle::detached_for_test(80, 24)
                    },
                ),
            },
            consumer_attach_rx,
            native_bootstrap_rx,
        )
    }

    /// A native bootstrap preflight failure is a terminal error: the peer
    /// reads `HELLO_OK`, ERROR, DETACHED, and only then EOF.
    #[tokio::test(flavor = "current_thread")]
    async fn native_preflight_failure_flushes_error_then_duplex_eof() {
        LocalSet::new()
            .run_until(async {
                let state = SharedState::new();
                let (_session, terminal) =
                    state.with_mut(|server| server.seed_session("fatal-duplex"));
                let (handle, mut consumer_attach_rx, mut native_bootstrap_rx) =
                    native_failure_handle();
                let client_id = state.with_mut(|server| {
                    let _ =
                        server.register_resource_handle(terminal, handle, CancellationToken::new());
                    let client_id = server.new_client_id();
                    server.set_peer_identity(
                        client_id,
                        PeerIdentity {
                            uid: 0,
                            pid: None,
                            exe_path: None,
                            mcp_host_key: None,
                            transport: TransportType::UnixSocket,
                            source_addr: None,
                        },
                    );
                    client_id
                });

                let native = BootstrapCapabilities::new().with_native(
                    EngineCodec::LibghosttySnapshotV1,
                    EngineFeatureSet::required_native(),
                );
                let reader = Script::new([
                    FrameKind::Hello {
                        client_name: "fatal-duplex-test".to_owned(),
                        protocol_major: PROTOCOL_VERSION.major,
                        protocol_minor: PROTOCOL_VERSION.minor,
                        protocol_patch: PROTOCOL_VERSION.patch,
                        client_caps: ClientCapabilities::new().with_bootstrap(native),
                    },
                    FrameKind::Attach {
                        attach_id: 1,
                        target: AttachTarget::ByName("fatal-duplex".to_owned()),
                        viewport: ViewportInfo::new(80, 24),
                        request_scrollback: false,
                        scrollback_limit_lines: 0,
                        role_policy: None,
                    },
                ]);
                let (server_io, peer_io) = tokio::io::duplex(64 * 1024);
                let (_server_read, server_write) = tokio::io::split(server_io);
                let (mut client_read, _client_write) = tokio::io::split(peer_io);
                let task = tokio::task::spawn_local(handle_client(
                    reader,
                    DuplexWriter(server_write),
                    state,
                    client_id,
                    CancellationToken::new(),
                    CancellationToken::new(),
                    None,
                    TransportType::UnixSocket,
                    false,
                ));
                let actor = tokio::task::spawn_local(async move {
                    let registration = consumer_attach_rx.recv().await.expect("registration");
                    registration
                        .reply
                        .send(Ok(ConsumerAttachOutcome {
                            tick_managed: false,
                            state_sync_bootstrap: None,
                        }))
                        .expect("registration reply");
                    native_bootstrap_rx
                        .recv()
                        .await
                        .expect("native preflight")
                        .reply
                        .send(Err(crate::native_state::NativeStateError::OutOfMemory))
                        .expect("native failure reply");
                });

                assert!(matches!(
                    next_frame(&mut client_read).await,
                    Some(FrameKind::HelloOk {
                        selected_profile: phux_protocol::caps::BootstrapProfile::NativeState {
                            codec: EngineCodec::LibghosttySnapshotV1,
                            ..
                        },
                        ..
                    })
                ));
                assert!(matches!(
                    next_frame(&mut client_read).await,
                    Some(FrameKind::Error {
                        code: ErrorCode::CodecUnavailable,
                        ..
                    })
                ));
                assert!(matches!(
                    next_frame(&mut client_read).await,
                    Some(FrameKind::Detached {
                        reason: Some(DetachReason::ProtocolError),
                        ..
                    })
                ));
                assert!(next_frame(&mut client_read).await.is_none());
                actor.await.expect("actor task");
                task.await.expect("client task").expect("client result");
            })
            .await;
    }
}

#[cfg(test)]
mod metadata_tests {
    use phux_protocol::ids::{ResourceId as WireResourceId, SatelliteHost};
    use phux_protocol::wire::frame::{ErrorCode, FrameKind, Scope};
    use tokio_util::sync::CancellationToken;

    use super::{handle_delete_metadata, handle_set_metadata, handle_subscribe_metadata};
    use crate::state::{ClientId, Outbound, SharedState};

    const AGENT_KEY: &str = "phux.agent/v1";

    fn set(state: &SharedState, scope: &Scope, key: &str, value: &[u8]) {
        handle_set_metadata(
            state,
            ClientId(1),
            1,
            scope,
            key,
            value.to_vec(),
            &CancellationToken::new(),
        );
    }

    fn get(state: &SharedState, scope: &Scope, key: &str) -> Option<Vec<u8>> {
        state.with(|s| s.metadata().get(scope, key))
    }

    fn satellite_scope() -> Scope {
        Scope::Resource(WireResourceId::Satellite {
            host: SatelliteHost::new("gpubox"),
            id: 7,
        })
    }

    fn expect_satellite_refusal(rx: &mut tokio::sync::mpsc::Receiver<Outbound>, needle: &str) {
        match rx.try_recv() {
            Ok(Outbound::Frame(FrameKind::Error {
                request_id: None,
                code: ErrorCode::UnsupportedSatelliteRoute,
                message,
            })) => {
                assert!(message.contains("does not federate"), "{message}");
                assert!(message.contains(needle), "{message}");
            }
            other => panic!("expected an uncorrelated satellite refusal, got {other:?}"),
        }
    }

    /// phux-w7z2.57: a `SUBSCRIBE_METADATA` naming a satellite pane is
    /// refused with a typed error and records nothing, while local and
    /// unscoped subscriptions are untouched.
    #[test]
    fn satellite_subscriptions_are_refused_and_local_ones_recorded() {
        let state = SharedState::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Outbound>(4);
        let client = ClientId(1);
        let scope = satellite_scope();
        handle_subscribe_metadata(&state, client, scope.clone(), AGENT_KEY.to_owned(), &tx);
        assert!(
            state
                .with(|s| s.metadata().subscribers_for(&scope, AGENT_KEY))
                .is_empty()
        );
        expect_satellite_refusal(&mut rx, "gpubox");

        let local = Scope::Resource(WireResourceId::local(1));
        handle_subscribe_metadata(&state, client, local.clone(), AGENT_KEY.to_owned(), &tx);
        handle_subscribe_metadata(
            &state,
            client,
            Scope::Global,
            "phux.tui.focus/v1".to_owned(),
            &tx,
        );
        assert_eq!(
            state.with(|s| s.metadata().subscribers_for(&local, AGENT_KEY)),
            vec![client]
        );
        assert_eq!(
            state.with(|s| s
                .metadata()
                .subscribers_for(&Scope::Global, "phux.tui.focus/v1")),
            vec![client]
        );
        assert!(
            rx.try_recv().is_err(),
            "an accepted subscription pushes nothing"
        );
    }

    /// ADR-0136: on a hub that routes the host, the two agent keys install a
    /// subscription and queue the mirror; any other key still refuses, and a
    /// client cannot write the satellite scope.
    #[test]
    fn a_hub_mirrors_the_agent_allowlist_and_refuses_the_rest() {
        use phux_protocol::wire::frame::RESOURCE_ASKED_KEY;

        use crate::hub::relay::{HubRelays, RelayHandle, RelayRequest};

        let state = SharedState::new();
        let (handle, mut mailbox) = RelayHandle::new(SatelliteHost::new("gpubox"));
        let relays = HubRelays::default();
        relays.insert(handle);
        state.with_mut(|s| s.set_hub_relays(relays));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Outbound>(4);
        let client = ClientId(3);
        let scope = satellite_scope();

        for key in [AGENT_KEY, RESOURCE_ASKED_KEY] {
            handle_subscribe_metadata(&state, client, scope.clone(), key.to_owned(), &tx);
            assert_eq!(
                state
                    .with(|s| s.metadata().subscribers_for(&scope, key))
                    .len(),
                1
            );
            assert!(matches!(
                mailbox.requests.try_recv(),
                Ok(RelayRequest::MirrorTerminal { terminal: 7 })
            ));
        }
        assert!(rx.try_recv().is_err());

        handle_subscribe_metadata(
            &state,
            client,
            scope.clone(),
            "phux.tags/v1".to_owned(),
            &tx,
        );
        expect_satellite_refusal(&mut rx, "phux.tags/v1");

        let mirrored = br#"{"name":"reviewer"}"#;
        state.with_mut(|s| s.metadata_set(&scope, AGENT_KEY, mirrored.to_vec()));
        set(&state, &scope, AGENT_KEY, b"overwrite");
        handle_delete_metadata(&state, client, 2, &scope, AGENT_KEY);
        assert_eq!(
            get(&state, &scope, AGENT_KEY).as_deref(),
            Some(&mirrored[..])
        );
    }

    #[test]
    fn terminal_metadata_cannot_outlive_or_target_a_missing_terminal() {
        use phux_protocol::wire::frame::RESOURCE_AGENT_SESSION_KEY;

        let state = SharedState::new();
        let (_session, pane) = state.with_mut(|s| s.seed_session("scope-test"));
        let scope = Scope::Resource(state.with_mut(|s| s.intern_terminal_wire(pane)));
        set(&state, &scope, "phux.test/v1", b"live");
        assert_eq!(get(&state, &scope, "phux.test/v1"), Some(b"live".to_vec()));

        for invalid in [Vec::new(), vec![b'x'; 4097]] {
            set(&state, &scope, RESOURCE_AGENT_SESSION_KEY, &invalid);
            assert!(get(&state, &scope, RESOURCE_AGENT_SESSION_KEY).is_none());
        }

        state.with_mut(|s| s.reap_terminal(pane));
        set(&state, &scope, "phux.test/v1", b"orphan");
        assert!(get(&state, &scope, "phux.test/v1").is_none());

        let missing = Scope::Resource(WireResourceId::local(u32::MAX));
        set(&state, &missing, "phux.test/v1", b"missing");
        assert!(get(&state, &missing, "phux.test/v1").is_none());
    }

    /// ADR-0129 / L3 §2: an over-cap value is refused whole; a value exactly
    /// at the cap is stored.
    #[test]
    fn a_metadata_value_over_the_cap_is_refused_and_nothing_is_stored() {
        let state = SharedState::new();
        let cap = state.with(crate::state::ServerState::metadata_value_bytes) as usize;
        set(
            &state,
            &Scope::Global,
            "phux.test.cap/v1",
            &vec![b'x'; cap + 1],
        );
        assert!(get(&state, &Scope::Global, "phux.test.cap/v1").is_none());
        let at_cap = vec![b'x'; cap];
        set(&state, &Scope::Global, "phux.test.cap/v1", &at_cap);
        assert_eq!(
            get(&state, &Scope::Global, "phux.test.cap/v1"),
            Some(at_cap)
        );
    }

    /// ADR-0129: the TUI's post-close layout republish can land after the
    /// reap; it must not resurrect the dead session's layout key.
    #[test]
    fn a_late_layout_write_for_a_reaped_session_is_not_stored() {
        let state = SharedState::new();
        let (session, pane) = state.with_mut(|s| {
            s.create_terminal(
                crate::state::TerminalTarget::NewSession {
                    name: "resurrect-test",
                    lifetime: phux_core::SessionLifetime::Ephemeral,
                },
                phux_core::TerminalLaunch::default(),
                phux_core::ExitPolicy::Remove,
                &phux_core::Actor::owner(),
            )
            .expect("ephemeral session")
        });
        let wire = state.with_mut(|s| s.idspace.intern_session(session));
        let key = format!("phux.tui.layout/v1/{}", wire.get());
        let scope = Scope::Group(crate::state::DEFAULT_GROUP_ID);
        set(&state, &scope, &key, b"live");
        assert_eq!(get(&state, &scope, &key), Some(b"live".to_vec()));
        state.with_mut(|s| s.reap_terminal(pane));
        assert!(state.with(|s| s.registry().session(session).is_none()));
        set(&state, &scope, &key, b"late republish");
        assert!(get(&state, &scope, &key).is_none());
    }

    /// Server-owned records and the owner-only create-result namespace refuse
    /// client writes and deletes.
    #[test]
    fn clients_cannot_forge_server_owned_or_reserved_keys() {
        let state = SharedState::new();
        let reserved_key = format!(
            "{}11111111-1111-4111-8111-111111111111",
            phux_protocol::wire::frame::SESSION_CREATE_RESULT_KEY_PREFIX,
        );
        set(&state, &Scope::Global, &reserved_key, b"forged");
        assert!(get(&state, &Scope::Global, &reserved_key).is_none());

        let (_session, pane) = state.with_mut(|s| s.seed_session("occupant-owner"));
        let scope = Scope::Resource(state.with_mut(|s| s.intern_terminal_wire(pane)));
        let key = phux_protocol::wire::frame::RESOURCE_PANE_OCCUPANT_KEY;
        let authoritative = br#"{"foreground":"zsh","is_pane_shell":true}"#.to_vec();
        state.with_mut(|s| s.metadata_set(&scope, key, authoritative.clone()));
        set(
            &state,
            &scope,
            key,
            br#"{"foreground":"vim","is_pane_shell":true}"#,
        );
        handle_delete_metadata(&state, ClientId(2), 6, &scope, key);
        assert_eq!(get(&state, &scope, key), Some(authoritative));
    }

    /// A create receipt binds the created identity, so it survives a rename
    /// and the reuse of the original name.
    #[tokio::test(flavor = "current_thread")]
    async fn empty_create_receipt_keeps_original_identity_after_name_reuse() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let state = SharedState::new();
                let nonce = "11111111-1111-4111-8111-111111111111";
                let value = serde_json::to_vec(&serde_json::json!({
                    "name": "original", "request_token": nonce, "empty": true, "keep_empty": true,
                }))
                .expect("request JSON");
                set(
                    &state,
                    &Scope::Global,
                    phux_protocol::wire::frame::SESSION_CREATE_KEY,
                    &value,
                );
                let key = super::session_create_result_key(Some(nonce));
                let bytes = get(&state, &Scope::Global, &key).expect("receipt");
                let receipt: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
                let session_id = |state: &SharedState| {
                    state.with_mut(|s| {
                        s.idspace
                            .intern_session(s.find_session_by_name("original").expect("session"))
                            .get()
                    })
                };
                let original_id = session_id(&state);
                assert_eq!(receipt["session_id"], original_id);
                state.with_mut(|s| s.rename_session("original", "renamed"));
                crate::runtime::commands::create_empty_session(
                    &state,
                    "original",
                    &phux_core::Actor::owner(),
                )
                .expect("replacement");
                assert_ne!(original_id, session_id(&state));
                assert_eq!(get(&state, &Scope::Global, &key), Some(bytes));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pending_session_create_token_cannot_be_reused_by_another_connection() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let state = SharedState::new();
                let root_token = CancellationToken::new();
                for (client_id, name) in [(ClientId(1), "first"), (ClientId(2), "collision")] {
                    let value = serde_json::to_vec(&serde_json::json!({
                        "name": name,
                        "request_token": "11111111-1111-4111-8111-111111111111",
                    }))
                    .expect("request JSON");
                    handle_set_metadata(
                        &state,
                        client_id,
                        1,
                        &Scope::Global,
                        phux_protocol::wire::frame::SESSION_CREATE_KEY,
                        value,
                        &root_token,
                    );
                }
                assert!(state.with(|s| s.session_by_name("first").is_some()));
                assert!(state.with(|s| s.session_by_name("collision").is_none()));
                root_token.cancel();
            })
            .await;
    }
}
