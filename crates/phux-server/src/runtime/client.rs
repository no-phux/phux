//! Submodule for runtime internals.

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use bytes::BytesMut;
use phux_core::ids::ResourceId as CoreResourceId;
use phux_core::process::ExitOutcome;
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
    AgentEvent, CloseReason, Command, CommandResult, DetachReason, ErrorCode, FrameKind,
    RESOURCE_AGENT_KEY,
};
use phux_protocol::wire::framing::FramingError;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use super::input_lane::{InputLaneHandle, RoutedInput};
use super::{
    STALE_PROBE_TIMEOUT, ServerError, SpawnRequest, bootstrap_attach_terminal, handle_attach,
    handle_detach_terminal, handle_frame_ack, handle_move_terminal, handle_terminal_input,
    handle_terminal_reply, handle_terminal_resize, handle_viewport_resize,
    subscribe_attach_terminal,
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
    fn context(&self) -> super::approvals::HeldContext {
        super::approvals::HeldContext {
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
            } => lane.begin_apply(self.client_id, operation_id, terminal_id, events),
            Command::RouteInput { terminal_id, event } => {
                lane.begin_route(self.client_id, terminal_id, event)
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
    ctx: &super::approvals::HeldContext,
    request_id: u32,
    command: Command,
    started: std::time::Instant,
) {
    ctx.run(request_id, command).await;
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
                let lane_owner =
                    super::super::input_lane::spawn_input_lane(SharedState::new()).unwrap();
                let lane = lane_owner.handle();
                let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(1);
                out_tx
                    .try_send(Outbound::Frame(FrameKind::Pong { nonce: 7 }))
                    .unwrap();
                let slots = Arc::new(tokio::sync::Semaphore::new(1));
                let receipt = lane.begin_apply(
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

/// A pane's event source and the wire id its events are journaled under
/// (ADR-0123). Handed to the pane's exit watcher, which is its drain.
///
/// The engine's sink is the one place an event can be lost before the
/// journal. Each drop is counted there, and the drain journals the count as
/// a `source_gap` scoped to this pane right after the event it read, so a
/// full sink is a typed loss rather than silence.
pub(crate) struct PaneEvents {
    /// The pane's wire id, interned at spawn.
    pub(crate) wire: WireResourceId,
    /// The runtime end of the pane's event sink.
    pub(crate) source: crate::resource::event_sink::EventSource,
}

#[cfg(test)]
mod pane_event_drain_tests {
    use super::*;

    /// A pane reaped by a failed publication already has its `pane_closed`;
    /// an event its drain reads afterwards is not journaled.
    #[test]
    fn a_reaped_pane_journals_nothing_after_its_close() {
        let mut s = crate::state::ServerState::new();
        let (_session, _window, pane) = s.seed_session("drain");
        let wire = s.intern_terminal_wire(pane);
        journal_drained_event(&mut s, &wire, AgentEvent::Bell.into(), 0);
        let live = s.journal_head();
        assert_eq!(live, 1, "a live pane's event is journaled");
        let _ = reap_pane_journaling_close(&mut s, pane);
        let closed = s.journal_head();
        assert_eq!(closed, live + 1, "the close");
        journal_drained_event(&mut s, &wire, AgentEvent::Bell.into(), 2);
        assert_eq!(s.journal_head(), closed, "nothing follows pane_closed");
    }
}

/// Drain a pane's events into the journal for as long as its sink lives:
/// the fallback for a pane with no exit notification to watch.
fn spawn_pane_event_drain(state: SharedState, mut events: PaneEvents) {
    tokio::task::spawn_local(async move {
        while let Some(event) = events.source.recv().await {
            let dropped = events.source.take_dropped();
            state.with_mut(|s| journal_drained_event(s, &events.wire, event, dropped));
        }
        let dropped = events.source.take_dropped();
        state.with_mut(|s| journal_source_gap(s, &events.wire, dropped));
    });
}

/// Journal a pane's events, in emission order, until its exit fires.
/// Returns the exit status and, when the sink is still open, the source,
/// whose already-queued events the reap lock journals before the close.
async fn journal_until_exit(
    state: &SharedState,
    mut exit: oneshot::Receiver<ExitOutcome>,
    events: Option<PaneEvents>,
) -> (ExitOutcome, Option<PaneEvents>) {
    let Some(mut events) = events else {
        return (exit.await.unwrap_or_default(), None);
    };
    loop {
        tokio::select! {
            biased;
            outcome = &mut exit => return (outcome.unwrap_or_default(), Some(events)),
            event = events.source.recv() => {
                let Some(event) = event else { break };
                let dropped = events.source.take_dropped();
                state.with_mut(|s| journal_drained_event(s, &events.wire, event, dropped));
            }
        }
    }
    let dropped = events.source.take_dropped();
    state.with_mut(|s| journal_source_gap(s, &events.wire, dropped));
    (exit.await.unwrap_or_default(), None)
}

/// Start the event pump for `client_id`'s subscription, once (ADR-0123):
/// a task that waits for room in the connection's mailbox and hands the
/// subscription whatever it is owed, a `journal_gap` or the rest of a
/// cursor replay, a frame at a time. It never runs inside frame dispatch,
/// so a consumer that stops reading delays only itself, and it ends with
/// the connection or the subscription.
pub(crate) fn ensure_event_pump(state: &SharedState, client_id: ClientId) {
    let Some(pump) = state.with_mut(|s| s.claim_event_pump(client_id)) else {
        return;
    };
    let cancel = state
        .with(|s| s.client_connection_cancellation(client_id))
        .unwrap_or_default();
    let state = state.clone();
    tokio::task::spawn_local(async move {
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = pump.wake.notified() => {}
            }
            if !pump_owed_frames(&state, (client_id, pump.epoch), &pump.tx, &cancel).await {
                return;
            }
        }
    });
}

/// Send subscription `epoch` of `client_id` its owed event frames as
/// mailbox room frees. `false` once the connection or that subscription is
/// gone, including when a re-subscribe replaced it.
async fn pump_owed_frames(
    state: &SharedState,
    (client_id, epoch): (ClientId, u64),
    tx: &mpsc::Sender<Outbound>,
    cancel: &CancellationToken,
) -> bool {
    loop {
        let permit = tokio::select! {
            () = cancel.cancelled() => return false,
            permit = tx.reserve() => match permit {
                Ok(permit) => permit,
                Err(_) => return false,
            },
        };
        match state.with_mut(|s| s.next_owed_event_frame(client_id, epoch)) {
            crate::state::PumpStep::Frame(frame) => permit.send(Outbound::Frame(frame)),
            crate::state::PumpStep::Idle => return true,
            crate::state::PumpStep::Gone => return false,
        }
    }
}

/// Reap a pane whose publication failed after its `pane_spawned` was
/// journaled (the spawner vanished, a preflight or send failed, its
/// generation was lost): journal its `pane_closed` in the same lock, once
/// (ADR-0123). The pane's exit watcher then finds it gone and journals
/// nothing more, so every `pane_spawned` is followed by exactly one
/// `pane_closed`. Returns what [`ServerState::reap_terminal`]
/// returns; `false` without journaling when the pane is already gone.
pub(crate) fn reap_pane_journaling_close(s: &mut ServerState, pane: CoreResourceId) -> bool {
    if s.registry().resource(pane).is_none() {
        return false;
    }
    let wire = s.intern_terminal_wire(pane);
    let parent = s
        .resource_parent(pane)
        .map(|parent| s.intern_terminal_wire(parent));
    journal_pane_closed(
        s,
        &wire,
        parent.as_ref(),
        None,
        crate::state::CloseAttribution::default(),
    );
    s.reap_terminal(pane)
}

/// Journal every event a closing pane already queued, and any loss its
/// sink counted, so all of them take a `seq` before its `pane_closed`.
fn journal_pending_events(s: &mut ServerState, events: &mut PaneEvents) {
    while let Some(event) = events.source.try_recv() {
        journal_drained_event(s, &events.wire, event, 0);
    }
    journal_source_gap(s, &events.wire, events.source.take_dropped());
}

/// Journal one event a pane's engine emitted, then any loss its sink
/// counted since the previous one. Nothing once the pane is gone: a pane
/// reaped by a failed publication or its parent's cascade already has its
/// `pane_closed`, and nothing about it may follow.
fn journal_drained_event(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    emitted: crate::resource::event_sink::Emitted,
    dropped: u64,
) {
    if !pane_is_live(s, wire_terminal_id) {
        return;
    }
    let crate::resource::event_sink::Emitted {
        event,
        operation_id,
    } = emitted;
    let actor = control_actor(&event);
    let record = crate::state::EventRecord::new(Some(wire_terminal_id.clone()), event)
        .with_actor(actor)
        .with_operation_id(operation_id);
    let _ = s.record_and_fanout(record);
    journal_source_gap(s, wire_terminal_id, dropped);
}

/// Journal `source_gap { dropped }` for a pane, when anything was dropped.
fn journal_source_gap(s: &mut ServerState, wire_terminal_id: &WireResourceId, dropped: u64) {
    if dropped == 0 || !pane_is_live(s, wire_terminal_id) {
        return;
    }
    let gap = AgentEvent::SourceGap { dropped };
    let _ = s.record_and_fanout(crate::state::EventRecord::new(
        Some(wire_terminal_id.clone()),
        gap,
    ));
}

/// Whether the pane `wire_terminal_id` names is still registered. A reap
/// retires the wire id, so a reaped pane no longer resolves.
fn pane_is_live(s: &ServerState, wire_terminal_id: &WireResourceId) -> bool {
    s.terminal_from_wire(wire_terminal_id)
        .is_some_and(|pane| s.registry().resource(pane).is_some())
}

/// Whether the pane `wire_terminal_id` names is retained after its process
/// exited (ADR-0124).
fn pane_exited(s: &ServerState, wire_terminal_id: &WireResourceId) -> bool {
    s.terminal_from_wire(wire_terminal_id)
        .is_some_and(|pane| s.retained_exit(pane).is_some())
}

/// The connection an engine-emitted event names as its cause: the `actor`
/// of a supervisory `terminal_control` (a take, a give, a signal). Every
/// other engine event is server-driven.
fn control_actor(event: &AgentEvent) -> Option<ClientId> {
    match event {
        AgentEvent::TerminalControl {
            actor: Some(actor), ..
        } => Some(ClientId(u64::from(actor.get()))),
        _ => None,
    }
}

/// Spawn the per-pane detector metadata drain (ADR-0046): the actor emits
/// edge-filtered [`AgentDetectEvent`]s and this task, which can reach
/// `ServerState`, performs the authority check and the metadata write.
pub(crate) fn spawn_agent_state_drain(
    state: SharedState,
    wire_terminal_id: WireResourceId,
    mut rx: tokio::sync::mpsc::Receiver<crate::agent_detect::AgentDetectEvent>,
) {
    use crate::agent_detect::AgentDetectEvent;

    tokio::task::spawn_local(async move {
        while let Some(event) = rx.recv().await {
            // Both the ask broadcast and the hook re-take the state lock, so
            // they are resolved under it and fired after it is released.
            let mut asked = None;
            let hook = state.with_mut(|s| {
                // ADR-0124: an exited pane's record was withdrawn; a report
                // queued before the detector stopped must not reassert it.
                if pane_exited(s, &wire_terminal_id) {
                    return None;
                }
                let scope = Scope::Resource(wire_terminal_id.clone());
                // Skip reading the prior record when no hook can run.
                let hooks_live = s.hook_dispatcher().is_some();
                match event {
                    AgentDetectEvent::Occupant(occupant) => {
                        if let Ok(bytes) = serde_json::to_vec(&occupant) {
                            s.metadata_set(
                                &scope,
                                phux_protocol::wire::frame::RESOURCE_PANE_OCCUPANT_KEY,
                                bytes,
                            );
                        }
                        None
                    }
                    AgentDetectEvent::Retract => {
                        drain_retract(s, &wire_terminal_id, &scope, hooks_live)
                    }
                    AgentDetectEvent::Reidentified { kind, name } => {
                        drain_reidentified(s, &wire_terminal_id, &scope, hooks_live, &kind, &name)
                    }
                    AgentDetectEvent::State(report) => {
                        drain_state(s, &wire_terminal_id, &scope, hooks_live, &report)
                    }
                    AgentDetectEvent::AskSentinel(ask) => {
                        asked = drain_ask_sentinel(s, &wire_terminal_id, ask);
                        None
                    }
                }
            });
            if let Some(payload) = asked {
                broadcast_event(&state, Some(&wire_terminal_id), &payload.into_event());
            }
            if let Some(event) = hook {
                crate::hooks::fire_hook(&state, event);
            }
        }
    });
}

/// The drain's `AskSentinel` arm: the pane's `phux-ask` title changed
/// (ADR-0036 tier 2). Runs the edge through the same arbiter `REPORT_ASKED`
/// uses and returns the payload to broadcast, if any. A cleared marker
/// retracts only a sentinel-owned ask and broadcasts nothing.
fn drain_ask_sentinel(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    ask: Option<crate::agent_asked::AskedPayload>,
) -> Option<crate::agent_asked::AskedPayload> {
    use crate::agent_asked::AskedSource;

    let terminal = s.terminal_from_wire(wire_terminal_id)?;
    let emitted = if let Some(payload) = ask {
        s.report_agent_asked(terminal, AskedSource::Sentinel, payload)
            .emit_payload()
    } else {
        s.retract_agent_asked(terminal, AskedSource::Sentinel);
        None
    };
    // ADR-0136: the metadata flag is how a consumer sees a clear.
    crate::hub::metadata_mirror::publish_asked_flag(
        s,
        wire_terminal_id,
        s.agent_is_asked(terminal),
    );
    emitted
}

/// The drain's `Retract` arm: the pane's agent is confirmed gone. A declared
/// record (L3 §3.7), or a detector record carrying a human's identity, is
/// withdrawn to `unknown` with its other fields kept; a record the detector
/// wrote alone is deleted; anything else is not ours.
fn drain_retract(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    scope: &Scope,
    hooks_live: bool,
) -> Option<HookEvent> {
    let existing = s.metadata().get(scope, RESOURCE_AGENT_KEY);
    let prior = prior_state(hooks_live, existing.as_deref());
    if s.agent_records().is_declared(wire_terminal_id) {
        withdraw_declared(s, wire_terminal_id, scope, existing.as_deref())?;
        return retract_hook(wire_terminal_id, prior);
    }
    if !s.agent_records().detector_owns(wire_terminal_id) {
        return None;
    }
    let withdrawn = if s.agent_records().has_explicit_identity(wire_terminal_id) {
        crate::agent_state::withdraw_state(existing.as_deref())
    } else {
        None
    };
    match withdrawn {
        Some(bytes) => {
            s.metadata_set(scope, RESOURCE_AGENT_KEY, bytes);
        }
        None => {
            s.metadata_delete(scope, RESOURCE_AGENT_KEY);
        }
    }
    s.agent_records_mut()
        .note_detector_retract(wire_terminal_id);
    retract_hook(wire_terminal_id, prior)
}

/// Withdraw a declared record's state to `unknown`, keeping its writer's
/// other fields. `None` when there is nothing to withdraw.
fn withdraw_declared(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    scope: &Scope,
    existing: Option<&[u8]>,
) -> Option<()> {
    let bytes = crate::agent_state::withdraw_state(existing)?;
    s.metadata_set(scope, RESOURCE_AGENT_KEY, bytes);
    s.agent_records_mut()
        .note_declaration_withdrawn(wire_terminal_id);
    Some(())
}

/// The drain's `Reidentified` arm: a different occupant now owns the pane.
/// One write landing on `unknown` (invariant I2 in `crate::agent_state`); a
/// declared record is withdrawn rather than corrected, and a pane with no
/// record is left without one.
fn drain_reidentified(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    scope: &Scope,
    hooks_live: bool,
    kind: &str,
    name: &str,
) -> Option<HookEvent> {
    use crate::hooks::AGENT_STATE_UNKNOWN;

    let existing = s.metadata().get(scope, RESOURCE_AGENT_KEY)?;
    let prior = prior_state(hooks_live, Some(&existing));
    if s.agent_records().is_declared(wire_terminal_id) {
        withdraw_declared(s, wire_terminal_id, scope, Some(&existing))?;
        return retract_hook(wire_terminal_id, prior);
    }
    let owned = s.agent_records().identity_ownership(wire_terminal_id);
    let bytes =
        crate::agent_state::compose(Some(&existing), kind, name, AGENT_STATE_UNKNOWN, owned);
    s.metadata_set(scope, RESOURCE_AGENT_KEY, bytes);
    s.agent_records_mut().note_detector_write(wire_terminal_id);
    state_change_hook(wire_terminal_id, kind, name, prior, AGENT_STATE_UNKNOWN)
}

/// The drain's `State` arm: the detector derived a state for this pane.
///
/// A declared record outranks the detector (ADR-0046 §E). `kind`, `name` and
/// `state` are composed from one read under one lock (invariant I1). Where an
/// explicit writer's `kind` contradicts the detector, the `kind` is kept and
/// the state is withdrawn to `unknown` (invariant I2).
fn drain_state(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    scope: &Scope,
    hooks_live: bool,
    report: &crate::agent_detect::AgentReport,
) -> Option<HookEvent> {
    if s.agent_records().is_declared(wire_terminal_id) {
        return None;
    }
    let existing = s.metadata().get(scope, RESOURCE_AGENT_KEY);
    let prior = prior_state(hooks_live, existing.as_deref());
    let owned = s.agent_records().identity_ownership(wire_terminal_id);
    let contradicted = owned.kind
        && crate::agent_state::explicit_kind_is_contradicted(
            existing.as_deref(),
            &report.kind,
            &crate::agent_detect::rules::global(),
        );
    let (to, bytes) = if contradicted {
        let bytes = crate::agent_state::withdraw_state(existing.as_deref())?;
        (crate::hooks::AGENT_STATE_UNKNOWN, bytes)
    } else {
        let to = report.state.as_str();
        let bytes =
            crate::agent_state::compose(existing.as_deref(), &report.kind, &report.name, to, owned);
        (to, bytes)
    };
    s.metadata_set(scope, RESOURCE_AGENT_KEY, bytes);
    if !contradicted {
        // Withdrawing is not authoring: the detector must not gain the right
        // to delete an explicit writer's record.
        s.agent_records_mut().note_detector_write(wire_terminal_id);
    }
    state_change_hook(wire_terminal_id, &report.kind, &report.name, prior, to)
}

/// The stored state before a detector write, if any.
struct Prior(Option<String>);

/// The prior state, read only when a hook could fire: `None` means no hook
/// is owed at all.
fn prior_state(hooks_live: bool, existing: Option<&[u8]>) -> Option<Prior> {
    hooks_live.then(|| Prior(crate::agent_state::stored_state(existing)))
}

/// The `agent-state-changed` event for a detector write, unless hooks are
/// off or the store already held that state.
fn state_change_hook(
    wire_terminal_id: &WireResourceId,
    kind: &str,
    name: &str,
    prior: Option<Prior>,
    to: &str,
) -> Option<HookEvent> {
    let Prior(from) = prior?;
    (from.as_deref() != Some(to))
        .then(|| HookEvent::agent_state_changed(wire_terminal_id, kind, name, from.as_deref(), to))
}

/// The `agent-state-changed` event for a withdrawn record: a write to
/// `unknown` with no identity.
fn retract_hook(wire_terminal_id: &WireResourceId, prior: Option<Prior>) -> Option<HookEvent> {
    state_change_hook(
        wire_terminal_id,
        "",
        "",
        prior,
        crate::hooks::AGENT_STATE_UNKNOWN,
    )
}

/// Re-arm the pane detector's edge filter after someone else wrote its
/// `phux.agent/v1` record (ADR-0046 §E); otherwise the filter keeps modelling
/// a store that no longer exists and suppresses the next write. A full or
/// closed control mailbox is benign (the actor is wedged or gone).
fn invalidate_agent_detector(state: &SharedState, scope: &Scope, key: &str) {
    if key != RESOURCE_AGENT_KEY {
        return;
    }
    let Scope::Resource(wire) = scope else {
        return;
    };
    let handle = state.with(|s| {
        s.terminal_from_wire(wire)
            .and_then(|pane| s.resource_handle(pane).cloned())
    });
    if let Some(handle) = handle {
        let _ = handle
            .control
            .try_send(crate::terminal_actor::ControlRequest::AgentRecordInvalidated);
    }
}

/// Spawn the per-pane exit watcher: on PTY EOF it journals and broadcasts
/// `RESOURCE_CLOSED` to every subscriber, then reaps the pane. Detaching is
/// the consumer's decision (ADR-0015). A dropped `exit_notify` sender is
/// treated as an exit with unknown status. The watcher is also the pane's
/// event drain (ADR-0123), so every event is journaled before `pane_closed`.
pub(crate) fn spawn_terminal_exit_watcher(
    state: SharedState,
    pane: CoreResourceId,
    exit_notify: Option<oneshot::Receiver<ExitOutcome>>,
    root_token: CancellationToken,
    events: Option<PaneEvents>,
) {
    let Some(rx) = exit_notify else {
        if let Some(events) = events {
            spawn_pane_event_drain(state, events);
        }
        return;
    };
    tokio::task::spawn_local(async move {
        let (mut exit, mut events) = journal_until_exit(&state, rx, events).await;
        while let Some(next_rx) = replace_last_shell(&state, pane, &root_token).await {
            info!("last shell exited; respawning a default shell in place");
            let next = journal_until_exit(&state, next_rx, events).await;
            exit = next.0;
            events = next.1;
        }
        // ADR-0124: a retained pane stays `Exited` until purged or expired,
        // then takes the ordinary close path below.
        let retained = state.with_mut(|s| retain_exited_pane(s, pane, exit, events.as_mut()));
        let exit_hook_owed = retained.is_none();
        if let Some(RetainedExit {
            retention,
            wire_terminal_id,
            agent_hook,
            control,
        }) = retained
        {
            // Awaited so a full mailbox delays the retire instead of dropping it.
            if let Some(control) = control {
                let _ = control.send(crate::resource::ControlRequest::Retire).await;
            }
            fire_retained_exit_hooks(&state, &wire_terminal_id, agent_hook, exit);
            events = hold_until_purge(&state, &retention, events).await;
        }
        let Some(reap) = state.with_mut(|s| reap_exited_pane(s, pane, exit, events.as_mut()))
        else {
            return;
        };
        announce_close(&state, reap, &root_token, exit_hook_owed).await;
    });
}

/// If `pane` is the session's last Terminal and this was a natural process
/// exit, replace the child in place and return the next EOF receiver.
async fn replace_last_shell(
    state: &SharedState,
    pane: CoreResourceId,
    root_token: &CancellationToken,
) -> Option<oneshot::Receiver<ExitOutcome>> {
    if root_token.is_cancelled() {
        return None;
    }
    let handle = state.with(|s| {
        s.should_replace_last_shell(pane)
            .then(|| s.resource_handle(pane).cloned())
            .flatten()
    })?;
    let command = replacement_shell_command(state, pane);
    let (reply_tx, reply_rx) = oneshot::channel();
    handle
        .control
        .send(crate::resource::ControlRequest::ReplaceChild {
            command: crate::resource::ReplacementCommand(command),
            reply: reply_tx,
        })
        .await
        .ok()?;
    reply_rx.await.ok()?.ok()
}

fn replacement_shell_command(
    state: &SharedState,
    pane: CoreResourceId,
) -> portable_pty::CommandBuilder {
    state.with_mut(|s| {
        let mut cmd = crate::terminal_actor::default_shell_command(s.shell(), s.login_shell());
        crate::terminal_actor::apply_term(&mut cmd, s.term());
        let wire = s.intern_terminal_wire(pane);
        crate::terminal_actor::apply_terminal_id(&mut cmd, &wire);
        crate::terminal_actor::apply_server_socket(&mut cmd, s.server_socket_path());
        cmd
    })
}

/// A pane kept as `Exited` after its process exited (ADR-0124), as its exit
/// watcher holds it until the purge.
struct RetainedExit {
    /// What a purge cancels, and when retention expires.
    retention: crate::state::Retention,
    /// The pane's wire id, for the `pane-exit` hook.
    wire_terminal_id: WireResourceId,
    /// The `agent-state-changed` hook the withdrawn agent record owes.
    agent_hook: Option<HookEvent>,
    /// The engine's control mailbox, for the `Retire` sent off the lock.
    control: Option<tokio::sync::mpsc::Sender<crate::resource::ControlRequest>>,
}

/// Keep `pane` as `Exited` when it asked to be retained and nothing is
/// already closing it (ADR-0124 §2), all in one lock: record its exit facet,
/// journal what it already queued and then `terminal_control { Exited }`,
/// withdraw its `phux.agent/v1` state to `unknown` (L3 §3.7), and tell its
/// engine to stop the detector and refuse input. No `RESOURCE_CLOSED`: the
/// resource is still here. `None` leaves the pane to the close path.
fn retain_exited_pane(
    s: &mut ServerState,
    pane: CoreResourceId,
    exit: ExitOutcome,
    events: Option<&mut PaneEvents>,
) -> Option<RetainedExit> {
    let retention = s.retain_exited(pane, exit, unix_now_ms())?;
    let wire_terminal_id = s.intern_terminal_wire(pane);
    if let Some(events) = events {
        journal_pending_events(s, events);
    }
    let input_holder = s.input_lease_holder(pane).map(super::wire_client);
    let exited = AgentEvent::TerminalControl {
        lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Exited,
        exit_status: exit.status,
        input_holder,
        action: phux_protocol::wire::frame::ControlAction::Exited,
        actor: None,
    };
    let _ = s.record_and_fanout(crate::state::EventRecord::new(
        Some(wire_terminal_id.clone()),
        exited,
    ));
    let control = s.resource_handle(pane).map(|handle| handle.control.clone());
    let scope = Scope::Resource(wire_terminal_id.clone());
    let hooks_live = s.hook_dispatcher().is_some();
    let agent_hook = drain_retract(s, &wire_terminal_id, &scope, hooks_live);
    Some(RetainedExit {
        retention,
        wire_terminal_id,
        agent_hook,
        control,
    })
}

/// The hooks a retained exit owes, fired off-lock: `pane-exit` now, because
/// the process is what exited (its purge fires none), and the withdrawn agent
/// record's `agent-state-changed`.
fn fire_retained_exit_hooks(
    state: &SharedState,
    wire_terminal_id: &WireResourceId,
    agent_hook: Option<HookEvent>,
    exit: ExitOutcome,
) {
    crate::hooks::fire_hook(state, HookEvent::pane_exit(wire_terminal_id, exit.status));
    if let Some(event) = agent_hook {
        crate::hooks::fire_hook(state, event);
    }
}

/// Hold a retained pane until its purge (ADR-0124 §4): a kill, an eviction,
/// or a shutdown cancels its engine token, or its retention expires. Its
/// events are still journaled meanwhile (a lease change on an exited pane is
/// still an event). Returns the event source for the close to drain.
async fn hold_until_purge(
    state: &SharedState,
    retention: &crate::state::Retention,
    mut events: Option<PaneEvents>,
) -> Option<PaneEvents> {
    let expiry = tokio::time::sleep(retention.hold);
    tokio::pin!(expiry);
    loop {
        tokio::select! {
            biased;
            () = retention.token.cancelled() => return events,
            () = &mut expiry => return events,
            event = next_pane_event(&mut events) => journal_held_event(state, &mut events, event),
        }
    }
}

/// The next event a pane's engine emits; pending forever once the source is
/// gone, so a held pane waits only on its purge.
async fn next_pane_event(
    events: &mut Option<PaneEvents>,
) -> Option<crate::resource::event_sink::Emitted> {
    match events.as_mut() {
        Some(events) => events.source.recv().await,
        None => std::future::pending().await,
    }
}

/// Journal one event a held pane emitted; a closed source journals its last
/// loss and is dropped.
fn journal_held_event(
    state: &SharedState,
    events: &mut Option<PaneEvents>,
    event: Option<crate::resource::event_sink::Emitted>,
) {
    let Some(pane_events) = events.as_mut() else {
        return;
    };
    let dropped = pane_events.source.take_dropped();
    let wire = pane_events.wire.clone();
    if let Some(event) = event {
        state.with_mut(|s| journal_drained_event(s, &wire, event, dropped));
        return;
    }
    state.with_mut(|s| journal_source_gap(s, &wire, dropped));
    *events = None;
}

/// Wall-clock now in Unix milliseconds; `0` for a clock before the epoch.
fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_millis()).ok())
        .unwrap_or(0)
}

/// Close a pane whose process exited (or whose retention ended) in one
/// critical section that also gathers who must be told, so no ATTACH can
/// interleave between gathering subscribers and the reap. `None` when
/// another closer already reaped the pane.
fn reap_exited_pane(
    s: &mut ServerState,
    pane: CoreResourceId,
    exit: ExitOutcome,
    events: Option<&mut PaneEvents>,
) -> Option<ReapAndNotify> {
    // ADR-0104 §2: a cascading parent may already have reaped this child.
    let reason = s.begin_resource_close(pane)?;
    // ADR-0124: a retained pane's close reports the exit it was kept with.
    let exit = s.retained_outcome(pane).unwrap_or(exit);
    // Interned before the cascade and the reap, which retires the wire id.
    let wire_terminal_id = s.intern_terminal_wire(pane);
    if let Some(events) = events {
        journal_pending_events(s, events);
    }
    let parent = s
        .resource_parent(pane)
        .map(|parent| s.intern_terminal_wire(parent));
    let cascaded = cascade_children(s, pane, &wire_terminal_id);
    // Every subscriber, including `ATTACH_RESOURCE`-only ones (L1 §3.1).
    let targets: Vec<mpsc::Sender<Outbound>> = s.terminal_fanout_targets(pane);
    // ADR-0123: `pane_closed` is journaled in the lock that removes the pane.
    let attribution = s.take_close_attribution(pane);
    journal_pane_closed(
        s,
        &wire_terminal_id,
        parent.as_ref(),
        exit.status,
        attribution,
    );
    let (server_empty, actor_token) = s.reap_terminal_deferring_actor_cancel(pane);
    let served = s.has_served_client();
    // ADR-0105: clients of a session a group kill released are detached.
    let killed_clients: Vec<_> = s
        .take_killed_sessions()
        .into_iter()
        .flat_map(|session| s.attached_clients_in_session(session))
        .collect();
    Some(ReapAndNotify {
        wire_terminal_id,
        reason,
        exit,
        cascaded,
        targets,
        server_empty,
        killed_clients,
        served,
        actor_token,
    })
}

/// Close every descendant bound to `pane` (ADR-0104 §2) in the parent's
/// lock, before the parent is reaped, so no client observes a child whose
/// parent has left. Each descendant's frame is emitted by the parent's
/// watcher; its own watcher later finds it already claimed.
fn cascade_children(
    s: &mut ServerState,
    pane: CoreResourceId,
    wire_terminal_id: &WireResourceId,
) -> Vec<CascadedClose> {
    // Deepest first, so no descendant leaves the registry unjournaled.
    let descendants = s.resource_descendants(pane);
    let mut cascaded = Vec::new();
    for child in descendants.into_iter().rev() {
        let Some(recorded) = s.begin_resource_close(child) else {
            continue;
        };
        // An unmarked child leaves because its parent is; one named in the
        // same kill keeps that kill's reason.
        let child_reason = if recorded == CloseReason::Exited {
            CloseReason::ParentClosed
        } else {
            recorded
        };
        let wire_child_id = s.intern_terminal_wire(child);
        let child_targets = s.terminal_fanout_targets(child);
        let child_exit = s.retained_outcome(child).unwrap_or(ExitOutcome::UNKNOWN);
        let attribution = s.take_close_attribution(child);
        journal_pane_closed(
            s,
            &wire_child_id,
            Some(wire_terminal_id),
            child_exit.status,
            attribution,
        );
        s.reap_terminal(child);
        cascaded.push(CascadedClose {
            wire_terminal_id: wire_child_id,
            targets: child_targets,
            reason: child_reason,
            exit: child_exit,
        });
    }
    cascaded
}

#[cfg(test)]
mod cascade_close_tests {
    use phux_core::process::ExitOutcome;
    use phux_core::resource::AgentFacet;
    use phux_protocol::wire::frame::CloseReason;

    use super::reap_exited_pane;
    use crate::state::ServerState;

    fn agent(provider: &str) -> AgentFacet {
        AgentFacet {
            provider: provider.to_owned(),
            native_id: None,
            state: None,
        }
    }

    #[test]
    fn reap_cascades_through_grandchildren() {
        let mut state = ServerState::new();
        let (_session, _window, grandparent) = state.seed_session("main");
        let child = state
            .registry_mut()
            .new_agent_session(grandparent, agent("child"))
            .expect("child");
        let grandchild = state
            .registry_mut()
            .new_agent_session(grandparent, agent("grandchild"))
            .expect("grandchild seed");
        state
            .registry_mut()
            .resource_mut(grandchild)
            .expect("live")
            .parent = Some(child);

        let wire_grandparent = state.intern_terminal_wire(grandparent);
        let wire_grandchild = state.intern_terminal_wire(grandchild);
        let reap = reap_exited_pane(&mut state, grandparent, ExitOutcome::exited(0), None)
            .expect("grandparent was live");

        assert_eq!(reap.wire_terminal_id, wire_grandparent);
        assert!(
            reap.cascaded.iter().any(|c| {
                c.wire_terminal_id == wire_grandchild && c.reason == CloseReason::ParentClosed
            }),
            "grandchild must be in the cascade with ParentClosed, not silently \
             dropped when the child is reaped; got {:?}",
            reap.cascaded
                .iter()
                .map(|c| (&c.wire_terminal_id, c.reason))
                .collect::<Vec<_>>(),
        );
        assert!(
            state.registry().resource(grandchild).is_none(),
            "grandchild must leave the registry with its ancestor"
        );
        // Immediate children still cascade; this is the one-level case the
        // previous helper covered, plus the grandchild.
        assert_eq!(reap.cascaded.len(), 2);
    }
}

/// The off-lock half of a close: the `pane-exit` hook when the exit was not
/// already announced, `RESOURCE_CLOSED` (children first), the detach of a
/// released keep-empty session's clients, and the phux-60s self-exit.
async fn announce_close(
    state: &SharedState,
    reap: ReapAndNotify,
    root_token: &CancellationToken,
    exit_hook_owed: bool,
) {
    let ReapAndNotify {
        wire_terminal_id,
        reason,
        exit,
        cascaded,
        targets,
        server_empty,
        served,
        killed_clients,
        actor_token,
    } = reap;
    // A retained pane already fired `pane-exit` when its process exited.
    if exit_hook_owed {
        crate::hooks::fire_hook(state, HookEvent::pane_exit(&wire_terminal_id, exit.status));
    }

    // Children first: the order the tree came apart in.
    for child in &cascaded {
        broadcast_terminal_closed(
            &child.wire_terminal_id,
            &child.targets,
            child.exit,
            child.reason,
        )
        .await;
    }
    broadcast_terminal_closed(&wire_terminal_id, &targets, exit, reason).await;
    if let Some(token) = actor_token {
        token.cancel();
    }

    // ADR-0105: after the closes, so a client sees its last pane go before
    // the session that held it.
    detach_clients_of_killed_session(state, killed_clients);

    // Self-exit once the last session is gone, but only after serving a
    // client (a fresh auto-spawned server's launcher is still connecting)
    // and not during an ordinary shutdown.
    if server_empty && served && !root_token.is_cancelled() {
        // Let writers flush `RESOURCE_CLOSED` before the root cancel closes
        // their transports.
        yield_until_close_frames_taken(&targets).await;
        for child in &cascaded {
            yield_until_close_frames_taken(&child.targets).await;
        }
        info!("last session reaped after serving clients; server self-exit");
        root_token.cancel();
    }
}

/// Yield until each client writer has taken the just-queued close frames,
/// or a small turn budget expires.
async fn yield_until_close_frames_taken(targets: &[mpsc::Sender<Outbound>]) {
    for _ in 0..256 {
        if targets
            .iter()
            .all(|tx| tx.is_closed() || tx.capacity() == tx.max_capacity())
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    // A few more turns for the write and flush.
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

/// Everything the exit watcher captures under the reap lock for the
/// off-lock `RESOURCE_CLOSED` fanout.
struct ReapAndNotify {
    /// The pane's wire id, interned before the reap retired it.
    wire_terminal_id: WireResourceId,
    /// Why this pane is closing (ADR-0104 §4).
    reason: CloseReason,
    exit: ExitOutcome,
    /// Children already reaped, waiting only for their frames.
    cascaded: Vec<CascadedClose>,
    /// Every client subscribed to the pane at reap time.
    targets: Vec<mpsc::Sender<Outbound>>,
    /// The reap emptied the last session.
    server_empty: bool,
    /// ADR-0105: clients of a released session, detached with `SESSION_KILLED`.
    killed_clients: Vec<(ClientId, mpsc::Sender<Outbound>)>,
    /// Whether any client has ever attached.
    served: bool,
    /// Cancelled after `RESOURCE_CLOSED` is queued so a fenced pump can
    /// publish the last screen first.
    actor_token: Option<CancellationToken>,
}

/// One resource closed because its parent did (ADR-0104 §2), captured
/// under the parent's lock and broadcast off it.
struct CascadedClose {
    wire_terminal_id: WireResourceId,
    targets: Vec<mpsc::Sender<Outbound>>,
    reason: CloseReason,
    /// Unknown unless the child is a retained exited pane.
    exit: ExitOutcome,
}

/// Send `RESOURCE_CLOSED` to every mailbox in `targets`, gathered by the
/// caller under the reap lock. Best-effort: closed mailboxes are skipped.
pub(crate) async fn broadcast_terminal_closed(
    wire_terminal_id: &WireResourceId,
    targets: &[mpsc::Sender<Outbound>],
    exit: ExitOutcome,
    reason: phux_protocol::wire::frame::CloseReason,
) {
    if targets.is_empty() {
        debug!("RESOURCE_CLOSED: no L1-subscribed clients to notify");
    } else {
        debug!(
            count = targets.len(),
            ?exit,
            "RESOURCE_CLOSED: broadcasting to subscribed clients",
        );
        // Concurrent, so one stalled subscriber cannot starve the rest.
        let sends = targets.iter().map(|tx| {
            let tx = tx.clone();
            let frame = Outbound::Frame(FrameKind::ResourceClosed {
                terminal_id: wire_terminal_id.clone(),
                exit_status: exit.status,
                reason,
                signal: exit.signal,
            });
            async move {
                let _ = tx.send(frame).await;
            }
        });
        futures_util::future::join_all(sends).await;
    }
}

/// Journal a resource's `pane_closed` (ADR-0123) under the lock that reaps
/// it. A child's close names its parent (ADR-0104 §2).
pub(super) fn journal_pane_closed(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    parent: Option<&WireResourceId>,
    exit_status: Option<i32>,
    attribution: crate::state::CloseAttribution,
) {
    // Nothing follows a close (L1 §7): withdraw held actions first (ADR-0128).
    s.withdraw_approvals_naming(wire_terminal_id);
    let record = crate::state::EventRecord::new(
        Some(wire_terminal_id.clone()),
        AgentEvent::ResourceClosed { exit_status },
    )
    .with_parent(parent.cloned())
    .with_actor(attribution.actor)
    .with_operation_id(attribution.operation_id);
    let _ = s.record_and_fanout(record);
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

/// Prepare and validate the parent directory of `socket_path`, so no other
/// user can plant or swap the socket.
///
/// A directory we create is made `0o700` and verified. An existing one is
/// validated, never mutated: it must be ours and not writable by others, or
/// sticky (like `/tmp`). Symlinks are resolved (macOS `/tmp` is one) and
/// the checks run against the canonical target.
pub(crate) fn prepare_socket_dir(socket_path: &Path) -> Result<(), ServerError> {
    let Some(parent) = socket_path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    let fail = |source| ServerError::PrepareDir {
        path: parent.to_path_buf(),
        source,
    };
    let expected_uid = rustix::process::geteuid().as_raw();
    let pre_existing = parent.exists();

    if !pre_existing {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(parent).map_err(fail)?;
    }

    let real = std::fs::canonicalize(parent).map_err(fail)?;
    let metadata = std::fs::metadata(&real).map_err(fail)?;
    if !metadata.is_dir() {
        return Err(fail(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket parent is not a directory",
        )));
    }

    let mode = metadata.mode() & 0o7777;
    let sticky = mode & 0o1000 != 0;
    let others_may_write = mode & 0o022 != 0;

    if pre_existing {
        // Validate only. Ours-and-private, or sticky (the /tmp arrangement),
        // are both safe; anything else lets another user swap the socket.
        let ours_and_private = metadata.uid() == expected_uid && !others_may_write;
        if !ours_and_private && !sticky {
            return Err(fail(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "socket parent is writable by other users and not sticky; \
                 point --socket at a directory you own, or at a sticky temp dir",
            )));
        }
        return Ok(());
    }

    // We just created it, so it must be exactly what we asked for. A mismatch
    // means someone raced us between create and stat.
    if metadata.uid() != expected_uid || mode & 0o777 != 0o700 {
        return Err(fail(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket parent ownership or permissions changed during setup",
        )));
    }
    Ok(())
}

/// Restrict a freshly bound UDS to its owning user.
pub(crate) fn secure_socket_file(socket_path: &Path) -> Result<(), ServerError> {
    let metadata = std::fs::symlink_metadata(socket_path)?;
    let expected_uid = rustix::process::geteuid().as_raw();
    if !metadata.file_type().is_socket() || metadata.uid() != expected_uid {
        return Err(ServerError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "bound socket is not an owner-controlled Unix socket",
        )));
    }
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    let secured = std::fs::symlink_metadata(socket_path)?;
    if secured.uid() != expected_uid || secured.mode() & 0o777 != 0o600 {
        return Err(ServerError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "bound socket ownership or permissions changed during setup",
        )));
    }
    Ok(())
}

#[cfg(test)]
mod socket_security_tests {
    use super::*;

    /// A directory phux creates is private; an existing one is validated and
    /// never chmod-ed: ours-and-private or sticky (like `/tmp`) is accepted,
    /// anything writable by others is refused. Symlinks are judged by their
    /// target (macOS `/tmp` is one).
    #[test]
    fn socket_parent_directory_rules() {
        use std::os::unix::fs::symlink;

        // (existing mode, reached through a symlink, accepted)
        let cases = [
            (None, false, true),
            (Some(0o777), false, false),
            (Some(0o1777), false, true),
            (Some(0o700), true, true),
            (Some(0o777), true, false),
        ];
        for (mode, via_symlink, accepted) in cases {
            let root = tempfile::tempdir().expect("tempdir");
            let target = root.path().join("runtime");
            if let Some(mode) = mode {
                std::fs::create_dir(&target).expect("dir");
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                    .expect("mode");
            }
            let parent = if via_symlink {
                let link = root.path().join("runtime-link");
                symlink(&target, &link).expect("symlink");
                link
            } else {
                target.clone()
            };
            let result = prepare_socket_dir(&parent.join("phux.sock"));
            let case = (mode, via_symlink);
            if accepted {
                result.unwrap_or_else(|err| panic!("{case:?} refused: {err}"));
            } else {
                assert!(
                    matches!(&result, Err(ServerError::PrepareDir { source, .. })
                        if source.kind() == io::ErrorKind::PermissionDenied),
                    "{case:?} accepted",
                );
            }
            let actual = std::fs::metadata(&target).expect("metadata").mode() & 0o7777;
            assert_eq!(actual, mode.unwrap_or(0o700), "{case:?} mode changed");
        }
    }
}

/// Handle the case where `socket_path` already exists. If something accepts a
/// connection on it within the probe timeout, treat it as live and refuse to
/// start. Otherwise unlink the stale entry so `bind` can succeed.
pub(crate) async fn handle_existing_socket(socket_path: &Path) -> Result<(), ServerError> {
    let metadata = match std::fs::symlink_metadata(socket_path) {
        Ok(m) => m,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(ServerError::Io(err)),
    };
    // Anything sitting in the way — socket, file, symlink — gets probed and
    // either rejected or removed.
    let connect = tokio::time::timeout(STALE_PROBE_TIMEOUT, UnixStream::connect(socket_path)).await;
    if let Ok(Ok(_stream)) = connect {
        return Err(ServerError::SocketBusy(socket_path.to_path_buf()));
    }
    debug!(
        path = %socket_path.display(),
        file_type = ?metadata.file_type(),
        "removing stale socket entry",
    );
    std::fs::remove_file(socket_path).map_err(ServerError::Io)?;
    Ok(())
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
                            if let Err(err) = handle_client(reader, writer, task_state.clone(), client_id, client_token, task_root_token, task_input_lane, task_transport, task_supports_quic_streams).await {
                                warn!(error = %err, "client task ended with error");
                            }
                            // The one site that forgets connection state.
                            release_connection_state(&task_state, client_id);
                            task_state.with_mut(ServerState::note_connection_closed);
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
            .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery]))
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
    clippy::cognitive_complexity,
    clippy::too_many_arguments,
    clippy::significant_drop_tightening,
    clippy::single_match_else,
    reason = "one read loop with one dispatch arm per wire frame; arm bodies are extracted, and the connection context comes verbatim from the accept loop"
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
    let mut used_attach_ids = HashSet::new();
    // Written once by HELLO; a duplicate HELLO is fatal.
    let mut negotiated: Option<NegotiatedConnection> = None;
    // QUIC multi-stream only, taken once HELLO negotiates it.
    let mut stream_events: Option<tokio::sync::mpsc::Receiver<QuicStreamEvent>> = None;
    // Absolute from admission: pre-HELLO PINGs must not keep a peer alive.
    let hello_deadline = tokio::time::sleep(crate::transport::HANDSHAKE_DEADLINE);
    tokio::pin!(hello_deadline);
    let route_input = |terminal_id, input| {
        route_client_input(&state, input_lane.as_ref(), client_id, terminal_id, input);
    };

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
                warn!(?client_id, "client did not complete HELLO before deadline; closing");
                break 'conn ConnectionEnding::Violation(ConnectionClose {
                    attached_reason: None,
                    detach_reason: DetachReason::ProtocolError,
                    code: ErrorCode::VersionIncompatible,
                    message: "HELLO deadline elapsed".to_owned(),
                });
            }
            res = reader.read_frame() => match res {
                Ok(Some(framed)) => (framed, reader.frame_origin()),
                Ok(None) => {
                    debug!("client disconnected (eof)");
                    break 'conn ConnectionEnding::TransportGone;
                }
                Err(err) => {
                    let Some(close) = framing_violation_close(&err, client_id) else {
                        debug!(error = %err, "client read error; closing");
                        break 'conn ConnectionEnding::TransportGone;
                    };
                    break 'conn ConnectionEnding::Violation(close);
                }
            },
            () = command_tasks.stopped() => {
                warn!(?client_id, "bulk command worker stopped unexpectedly; closing");
                break 'conn ConnectionEnding::Violation(ConnectionClose {
                    attached_reason: Some("bulk command worker stopped"),
                    detach_reason: DetachReason::ProtocolError,
                    code: ErrorCode::InternalError,
                    message: "bulk command worker stopped unexpectedly".to_owned(),
                });
            }
            Some(_) = input_receipts.join_next(), if !input_receipts.is_empty() => continue,
            Some(_) = held_commands.join_next(), if !held_commands.is_empty() => continue,
            event = async {
                match stream_events.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => core::future::pending().await,
                }
            } => {
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
                if let Err(close) = negotiate_hello(
                    &state,
                    client_id,
                    &plumbing.out_tx,
                    hello,
                    &mut negotiated,
                    transport,
                    route_supports_quic_streams,
                )
                .await
                {
                    break 'conn ConnectionEnding::Violation(close);
                }
                if let Some(selection) = negotiated.as_ref() {
                    plumbing.set_compression(selection.compression);
                    // The client learns the bit from HELLO_OK, so no Terminal
                    // stream can have opened before this.
                    if selection.quic_streams() {
                        stream_events = reader.take_stream_events();
                    }
                }
            }
            FrameKind::Ping { nonce } => reply_pong(&plumbing.out_tx, client_id, nonce).await,
            FrameKind::Attach {
                attach_id,
                target,
                viewport,
                request_scrollback,
                scrollback_limit_lines,
                role_policy,
            } => {
                match classify_attach_id(attach_id, &mut used_attach_ids, client_id) {
                    AttachIdVerdict::Fresh => {}
                    AttachIdVerdict::Reused => {
                        super::send_error(
                            &plumbing.out_tx,
                            ErrorCode::MalformedMessage,
                            &format!(
                                "ATTACH attach_id {attach_id} was already used on this connection"
                            ),
                        )
                        .await;
                        continue;
                    }
                    AttachIdVerdict::Reserved => {
                        break 'conn ConnectionEnding::Violation(ConnectionClose {
                            attached_reason: Some("zero ATTACH id"),
                            detach_reason: DetachReason::ProtocolError,
                            code: ErrorCode::MalformedMessage,
                            message: "ATTACH attach_id must be nonzero".to_owned(),
                        });
                    }
                }
                let Some(selection) = negotiated.as_ref() else {
                    continue;
                };
                debug!(
                    ?client_id,
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
                    &state,
                    client_id,
                    attach_id,
                    target,
                    viewport,
                    request_scrollback,
                    scrollback_limit_lines,
                    role_policy,
                    &plumbing.out_tx,
                    selection.client_caps,
                    selection.profile,
                    selection.limits,
                    &root_token,
                    &mut plumbing.output_pumps,
                    &token,
                    defer_subscription,
                )
                .await;
                crate::perf::ATTACH_HANDLE.record_elapsed(attach_started);
            }
            FrameKind::Detach => {
                detach_on_request(
                    &state,
                    client_id,
                    &plumbing.out_tx,
                    &mut plumbing.output_pumps,
                )
                .await;
                // Session DETACH ends every Terminal stream too.
                plumbing.drop_all_stream_bindings().await;
            }
            FrameKind::ViewportResize { viewport } => {
                debug!(
                    ?client_id,
                    cols = viewport.cols,
                    rows = viewport.rows,
                    "VIEWPORT_RESIZE"
                );
                handle_viewport_resize(&state, client_id, &viewport);
            }
            FrameKind::InputKey { terminal_id, event } => {
                route_input(terminal_id, TerminalInput::Key(event));
            }
            FrameKind::InputMouse { terminal_id, event } => {
                route_input(terminal_id, TerminalInput::Mouse(event));
            }
            FrameKind::InputFocus { terminal_id, event } => {
                route_input(terminal_id, TerminalInput::Focus(event));
            }
            FrameKind::InputPaste { terminal_id, event } => {
                route_input(terminal_id, TerminalInput::Paste(event));
            }
            FrameKind::InputTerminalReply { terminal_id, bytes } => {
                let Some(selection) = negotiated.as_ref() else {
                    continue;
                };
                dispatch_terminal_reply(
                    client_id,
                    *selection,
                    &terminal_id,
                    bytes,
                    &plumbing.out_tx,
                )
                .await;
            }
            // ADR-0103 §4: an agent-session stream is raw-profile with no
            // history beyond its bootstrap. Both refusals keep the connection.
            FrameKind::FrameAck {
                ref terminal_id, ..
            } if is_agent_session(&state, terminal_id) => {
                super::send_error(
                    &plumbing.out_tx,
                    ErrorCode::MalformedMessage,
                    "FRAME_ACK is not valid on an agent-session stream",
                )
                .await;
            }
            FrameKind::HistoryRequest {
                ref terminal_id, ..
            } if is_agent_session(&state, terminal_id) => {
                super::send_error(
                    &plumbing.out_tx,
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
                    &state,
                    client_id,
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
                let Some(selection) = negotiated.as_ref() else {
                    continue;
                };
                // Terminal-content replies ride the bound stream when one
                // exists (proto.md §4.9); control otherwise.
                let history_tx = plumbing.sender_for(&terminal_id);
                serve_history_request(
                    &state,
                    client_id,
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
            FrameKind::GetMetadata {
                request_id,
                scope,
                key,
            } => {
                handle_get_metadata(
                    &state,
                    client_id,
                    request_id,
                    &scope,
                    &key,
                    &plumbing.out_tx,
                )
                .await;
            }
            FrameKind::SetMetadata {
                request_id,
                scope,
                key,
                value,
            } if super::approvals::is_decision(&scope, &key) => {
                super::approvals::decide(
                    &state,
                    client_id,
                    request_id,
                    &key,
                    &value,
                    &plumbing.out_tx,
                )
                .await;
            }
            FrameKind::SetMetadata {
                request_id,
                scope,
                key,
                value,
            } => {
                handle_set_metadata(
                    &state,
                    client_id,
                    request_id,
                    &scope,
                    &key,
                    value,
                    &root_token,
                );
            }
            FrameKind::DeleteMetadata {
                request_id,
                scope,
                key,
            } => {
                handle_delete_metadata(&state, client_id, request_id, &scope, &key);
            }
            FrameKind::ListMetadata { request_id, scope } => {
                handle_list_metadata(&state, client_id, request_id, &scope, &plumbing.out_tx).await;
            }
            FrameKind::ListDirectory {
                request_id,
                path,
                host,
            } => {
                super::directory::handle_list_directory(
                    &state,
                    client_id,
                    super::directory::ListRequest {
                        request_id,
                        path,
                        host,
                    },
                    &plumbing.out_tx,
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
                    &state,
                    client_id,
                    super::path_search::PathRequest {
                        request_id,
                        root,
                        query,
                        recursive,
                        host,
                    },
                    &plumbing.out_tx,
                );
            }
            FrameKind::SubscribeMetadata { scope, key } => {
                handle_subscribe_metadata(&state, client_id, scope, key, &plumbing.out_tx);
            }
            FrameKind::SubscribeEvents {
                terminal,
                after_seq,
            } => {
                handle_subscribe_events(&state, client_id, terminal, after_seq, &plumbing.out_tx);
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
                let Some(selection) = negotiated.as_ref() else {
                    continue;
                };
                crate::runtime::idempotent_create::handle_spawn_resource(
                    &state,
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
                    &plumbing.out_tx,
                    selection.profile,
                    selection.limits,
                    &root_token,
                    &token,
                    &mut plumbing.output_pumps,
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
                    &state,
                    client_id,
                    request_id,
                    terminal,
                    owner_terminal,
                    &plumbing.out_tx,
                )
                .await;
            }
            FrameKind::ResizeTerminal {
                terminal_id,
                cols,
                rows,
            } => {
                handle_terminal_resize(&state, client_id, &terminal_id, cols, rows);
            }
            FrameKind::Command {
                request_id,
                command,
            } => {
                let Some(selection) = negotiated else {
                    continue;
                };
                let command_outcome = (CommandDispatch {
                    state: &state,
                    client_id,
                    out_tx: &plumbing.out_tx,
                    input_lane: input_lane.as_ref(),
                    token: &token,
                    root_token: &root_token,
                    selection,
                    command_tasks: &mut command_tasks,
                    input_receipts: &mut input_receipts,
                    input_receipt_slots: &input_receipt_slots,
                    held_commands: &mut held_commands,
                })
                .run(request_id, command)
                .await;
                match command_outcome {
                    CommandDispatchOutcome::Completed(Some(terminal_id)) => {
                        // Already unsubscribed; drop the stream binding too.
                        plumbing.drop_stream_binding(&terminal_id).await;
                    }
                    CommandDispatchOutcome::Completed(None) => {}
                    CommandDispatchOutcome::Cancelled => break 'conn ConnectionEnding::Cancelled,
                }
            }
            other => {
                warn!(?client_id, kind = ?other, "direction-invalid client frame; closing");
                break 'conn ConnectionEnding::Violation(ConnectionClose {
                    attached_reason: Some("direction-invalid frame"),
                    detach_reason: DetachReason::ProtocolError,
                    code: ErrorCode::InvalidCommand,
                    message: format!(
                        "frame is not valid from a client in the negotiated phase: {other:?}"
                    ),
                });
            }
        }
    };
    // A closing connection withdraws its holds before any await (ADR-0128):
    // no decision can land in the teardown window.
    super::approvals::withdraw(&state, client_id);
    held_commands.shutdown().await;
    input_receipts.shutdown().await;
    command_tasks.shutdown().await;
    match ending {
        ConnectionEnding::TransportGone => {}
        ConnectionEnding::Violation(close) => plumbing.close(close, &state, client_id).await,
        ConnectionEnding::Cancelled => {
            plumbing
                .close_for_cancellation(&state, client_id, &root_token)
                .await;
        }
    }
    Ok(())
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

/// Route one decoded `INPUT_*` event: a local pane id goes to the input lane
/// (ADR-0044) when there is one; satellite ids and the no-lane path use the
/// inline [`handle_terminal_input`] with the same gates.
fn route_client_input(
    state: &SharedState,
    input_lane: Option<&InputLaneHandle>,
    client_id: ClientId,
    terminal_id: WireResourceId,
    input: TerminalInput,
) {
    let frame_label = match &input {
        TerminalInput::Key(_) => "INPUT_KEY",
        TerminalInput::Mouse(_) => "INPUT_MOUSE",
        TerminalInput::Focus(_) => "INPUT_FOCUS",
        TerminalInput::Paste(_) => "INPUT_PASTE",
    };
    if let Some(lane) = input_lane
        && terminal_id.is_local()
    {
        lane.route(RoutedInput::attached(
            client_id,
            terminal_id,
            input,
            frame_label,
        ));
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

/// Run the create a `SESSION_CREATE_KEY` request asked for: an empty,
/// keep-empty session (ADR-0105), or a seeded one that is optionally marked
/// keep-empty afterwards in the same single-threaded turn. Returns the seed
/// pane's wire id, or `None` for an empty session.
fn run_session_create(
    state: &SharedState,
    writer: ClientId,
    request: SessionCreateRequest,
    root_token: &CancellationToken,
) -> Result<Option<WireResourceId>, String> {
    if request.empty {
        return crate::runtime::commands::create_empty_session(state, &request.name).map(|()| None);
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
    if request.keep_empty {
        state.with_mut(|s| s.set_session_keep_empty(&request.name, true));
    }
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

/// Apply a `phux.session.keep_empty/v1` write (ADR-0105) to the registry.
/// A change is broadcast to the key's subscribers; clearing the mark on an
/// empty session removes it, detaching its clients and possibly self-exiting
/// as the reap cascade does.
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
        let session = s.find_session_by_name(name);
        let outcome = s.set_session_keep_empty(name, keep);
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
        let removed = outcome == KeepEmptyOutcome::Removed;
        let clients = match session {
            Some(session) if removed => s.attached_clients_in_session(session),
            _ => Vec::new(),
        };
        let drained = removed && s.registry().session_count() == 0 && s.has_served_client();
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
        info!("last session killed after serving clients; server self-exit");
        root_token.cancel();
    }
}

/// Detach the clients attached to a session that was just killed, with
/// `DETACHED { SESSION_KILLED }`. Each delivery waits in its own task, so a
/// wedged client's full mailbox cannot block the write that killed it.
fn detach_clients_of_killed_session(
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

/// Apply a session rename written as `current_name\0new_name` to the
/// registry. An applied change fans out the `current\0new` value to the
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
        let outcome = s.rename_session(&current, &new_name);
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
    store_metadata_value(state, client_id, request_id, scope, key, value);
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
    frame.encode_compressed(compress_policy(&frame, compression), scratch, batch);
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
            .unless_revoked(writer.write_frames(&buf, &ends))
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
        handle_client(
            reader,
            recorder.clone(),
            SharedState::new(),
            ClientId(9),
            token,
            root_token,
            None,
            TransportType::UnixSocket,
            false,
        )
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
            .run_until(handle_client(
                script,
                recorder.clone(),
                state,
                client_id,
                CancellationToken::new(),
                CancellationToken::new(),
                None,
                TransportType::UnixSocket,
                false,
            ))
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
        let (frames, closed) = negotiated([attach(), attach(), FrameKind::Ping { nonce: 5 }]).await;
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
                let (_session, _window, terminal) =
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
        let (_session, _window, pane) = state.with_mut(|s| s.seed_session("scope-test"));
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
        let (session, _window, pane) = state.with_mut(|s| s.seed_session("resurrect-test"));
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

        let (_session, _window, pane) = state.with_mut(|s| s.seed_session("occupant-owner"));
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
                crate::runtime::commands::create_empty_session(&state, "original")
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod agent_drain_tests {
    use phux_protocol::ids::ResourceId as WireResourceId;
    use phux_protocol::wire::frame::{RESOURCE_AGENT_KEY, Scope};

    use super::{Prior, retract_hook, spawn_agent_state_drain, state_change_hook};
    use crate::agent_asked::AskedPayload;
    use crate::agent_detect::record::AgentRecordJson;
    use crate::agent_detect::{AgentDetectEvent, AgentReport, DetectedState};
    use crate::hooks::AGENT_STATE_UNKNOWN;
    use crate::state::SharedState;

    fn report_of(kind: &str, state: DetectedState) -> AgentDetectEvent {
        AgentDetectEvent::State(AgentReport {
            kind: kind.to_owned(),
            name: kind.to_owned(),
            state,
        })
    }

    fn claude(state: DetectedState) -> AgentDetectEvent {
        report_of("claude", state)
    }

    fn reidentified(kind: &str) -> AgentDetectEvent {
        AgentDetectEvent::Reidentified {
            kind: kind.to_owned(),
            name: kind.to_owned(),
        }
    }

    /// A pane at wire id 1, optionally carrying a record its human wrote.
    fn pane(declared: Option<&[u8]>) -> (SharedState, WireResourceId) {
        let state = SharedState::new();
        let terminal = WireResourceId::new(1);
        if let Some(bytes) = declared {
            state.with_mut(|s| {
                s.agent_records_mut().note_explicit_set(&terminal, bytes);
                s.metadata_set(
                    &Scope::Resource(terminal.clone()),
                    RESOURCE_AGENT_KEY,
                    bytes.to_vec(),
                );
            });
        }
        (state, terminal)
    }

    /// Drive the real drain task to quiescence over `events`.
    async fn drain(state: &SharedState, terminal: &WireResourceId, events: Vec<AgentDetectEvent>) {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        spawn_agent_state_drain(state.clone(), terminal.clone(), rx);
        for event in events {
            tx.send(event).await.expect("drain is alive");
        }
        drop(tx);
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    fn raw(state: &SharedState, terminal: &WireResourceId) -> Option<Vec<u8>> {
        let scope = Scope::Resource(terminal.clone());
        state.with(|s| s.metadata().get(&scope, RESOURCE_AGENT_KEY))
    }

    fn stored(state: &SharedState, terminal: &WireResourceId) -> Option<AgentRecordJson> {
        raw(state, terminal).and_then(|bytes| AgentRecordJson::decode(&bytes))
    }

    async fn local(test: impl std::future::Future<Output = ()>) {
        tokio::task::LocalSet::new().run_until(test).await;
    }

    #[test]
    fn hooks_fire_only_on_real_edges() {
        let terminal = WireResourceId::local(1);
        let ctx = |event: &crate::hooks::HookEvent, key: &str| event.context.get(key).cloned();
        let was = |state: &str| Some(Prior(Some(state.to_owned())));

        assert!(
            state_change_hook(&terminal, "claude", "", None, "working").is_none(),
            "no hook is owed while hooks are off"
        );
        let first =
            state_change_hook(&terminal, "claude", "", Some(Prior(None)), "working").expect("edge");
        assert_eq!(first.name, crate::hooks::AGENT_STATE_CHANGED);
        assert_eq!(
            ctx(&first, "from"),
            None,
            "a first sighting has no prior state"
        );
        assert_eq!(ctx(&first, "agent-name"), None, "an empty name is omitted");
        let blocked =
            state_change_hook(&terminal, "claude", "rev", was("working"), "blocked").expect("edge");
        assert_eq!(ctx(&blocked, "from").as_deref(), Some("working"));
        assert_eq!(ctx(&blocked, "to").as_deref(), Some("blocked"));
        assert!(state_change_hook(&terminal, "claude", "claude", was("idle"), "idle").is_none());

        let retract = retract_hook(&terminal, was("working")).expect("a retract is an edge");
        assert_eq!(ctx(&retract, "to").as_deref(), Some(AGENT_STATE_UNKNOWN));
        assert!(retract_hook(&terminal, was(AGENT_STATE_UNKNOWN)).is_none());
    }

    /// One drain run per row over a pane that may carry a human's record:
    /// the resulting `(kind, name, state, session)`, or `None` when no record
    /// remains.
    ///
    /// - An identity-only record is not a declaration: the detector fills in
    ///   `state`, and a retract withdraws it without deleting the name.
    /// - A record the detector wrote alone is deleted by a retract.
    /// - I2: a new occupant lands on `unknown` in one write; a declaration is
    ///   withdrawn rather than corrected; no record means nothing is written.
    /// - I1: a dropped `Reidentified` is healed by the next `State` write.
    /// - A custom kind the detector cannot derive still gets its state, and
    ///   reasserting a kind never drags a human's name with it.
    #[tokio::test(flavor = "current_thread")]
    async fn detector_writes_respect_human_records() {
        type Expect<'a> = Option<(Option<&'a str>, &'a str, &'a str, Option<&'a str>)>;
        type Case<'a> = (Option<&'a [u8]>, Vec<AgentDetectEvent>, Expect<'a>);
        let identity = br#"{"name":"reviewer","kind":"claude","session":"fleet-7"}"#.as_slice();
        let declared = br#"{"name":"me","kind":"claude","state":"working"}"#.as_slice();
        let custom = br#"{"name":"reviewer","kind":"my-agent","session":"fleet-7"}"#.as_slice();
        let nameless = br#"{"name":"reviewer","session":"fleet-7"}"#.as_slice();
        let cases: Vec<Case<'_>> = vec![
            (
                Some(identity),
                vec![claude(DetectedState::Working), AgentDetectEvent::Retract],
                Some((
                    Some("claude"),
                    "reviewer",
                    AGENT_STATE_UNKNOWN,
                    Some("fleet-7"),
                )),
            ),
            (
                None,
                vec![claude(DetectedState::Working), AgentDetectEvent::Retract],
                None,
            ),
            (
                None,
                vec![claude(DetectedState::Working), reidentified("codex")],
                Some((Some("codex"), "codex", AGENT_STATE_UNKNOWN, None)),
            ),
            (
                None,
                vec![
                    claude(DetectedState::Working),
                    report_of("codex", DetectedState::Blocked),
                ],
                Some((Some("codex"), "codex", "blocked", None)),
            ),
            (
                Some(declared),
                vec![reidentified("codex")],
                Some((Some("claude"), "me", AGENT_STATE_UNKNOWN, None)),
            ),
            (None, vec![reidentified("codex")], None),
            (
                Some(custom),
                vec![claude(DetectedState::Blocked)],
                Some((Some("my-agent"), "reviewer", "blocked", Some("fleet-7"))),
            ),
            (
                Some(nameless),
                vec![
                    claude(DetectedState::Working),
                    report_of("codex", DetectedState::Idle),
                ],
                Some((Some("codex"), "reviewer", "idle", Some("fleet-7"))),
            ),
        ];
        for (row, (record, events, expect)) in cases.into_iter().enumerate() {
            local(async {
                let (state, terminal) = pane(record);
                drain(&state, &terminal, events).await;
                let got = stored(&state, &terminal);
                let got = got.as_ref().map(|r| {
                    (
                        r.kind.as_deref(),
                        r.name.as_str(),
                        r.state.as_str(),
                        r.session.as_deref(),
                    )
                });
                assert_eq!(got, expect, "row {row}");
            })
            .await;
        }
    }

    /// A declared state outranks derivations (L3 §3.7), but a confirmed
    /// departure withdraws it to `unknown`, never deletes it; afterwards the
    /// detector's writes land again (phux-w7z2.13: a `kill -9` must not pin
    /// the pane at `working`).
    #[tokio::test(flavor = "current_thread")]
    async fn a_retract_withdraws_a_declared_state_but_never_deletes_it() {
        local(async {
            let (state, terminal) = pane(Some(
                br#"{"name":"me","kind":"claude","state":"working","attention":"high"}"#,
            ));
            drain(&state, &terminal, vec![claude(DetectedState::Idle)]).await;
            assert_eq!(
                stored(&state, &terminal).expect("declared").state,
                "working"
            );

            drain(&state, &terminal, vec![AgentDetectEvent::Retract]).await;
            let record = stored(&state, &terminal).expect("withdrawing is not deleting");
            assert_eq!(record.state, AGENT_STATE_UNKNOWN);
            assert_eq!(record.name, "me");
            assert_eq!(record.kind.as_deref(), Some("claude"));
            assert_eq!(record.attention, None);

            drain(&state, &terminal, vec![claude(DetectedState::Working)]).await;
            let after = stored(&state, &terminal).expect("still there");
            assert_eq!(after.state, "working", "the detector resumed");
            assert_eq!(after.name, "me");
        })
        .await;
    }

    /// phux-w7z2.45: a shim pane declared `kind: claude` now running codex
    /// keeps the declared kind (L3 §3.7) and withholds codex's state, and
    /// every later contradicted tick writes nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn a_shim_pane_never_takes_a_state_from_an_occupant_its_kind_denies() {
        local(async {
            let (state, terminal) = pane(Some(br#"{"name":"claude","kind":"claude"}"#));
            drain(
                &state,
                &terminal,
                vec![
                    claude(DetectedState::Working),
                    reidentified("codex"),
                    report_of("codex", DetectedState::Working),
                ],
            )
            .await;
            let record = stored(&state, &terminal).expect("still there");
            assert_eq!(record.kind.as_deref(), Some("claude"));
            assert_eq!(record.name, "claude");
            assert_eq!(record.state, AGENT_STATE_UNKNOWN);

            let first = raw(&state, &terminal);
            let churn = [
                DetectedState::Blocked,
                DetectedState::Idle,
                DetectedState::Working,
                DetectedState::Done,
            ];
            let ticks = (0..20)
                .map(|i| report_of("codex", churn[i % churn.len()]))
                .collect();
            drain(&state, &terminal, ticks).await;
            assert_eq!(raw(&state, &terminal), first, "withholding is free");
        })
        .await;
    }

    /// Level-triggered: the state resumes once the pane runs the declared
    /// kind again, and clearing the declaration hands the kind back.
    #[tokio::test(flavor = "current_thread")]
    async fn a_contradiction_heals_on_the_declared_kind_or_a_cleared_declaration() {
        local(async {
            let (state, terminal) = pane(Some(br#"{"name":"claude","kind":"claude"}"#));
            drain(
                &state,
                &terminal,
                vec![report_of("codex", DetectedState::Working)],
            )
            .await;
            assert_eq!(
                stored(&state, &terminal).expect("written").state,
                AGENT_STATE_UNKNOWN
            );
            drain(&state, &terminal, vec![claude(DetectedState::Blocked)]).await;
            assert_eq!(stored(&state, &terminal).expect("written").state, "blocked");

            drain(
                &state,
                &terminal,
                vec![report_of("codex", DetectedState::Working)],
            )
            .await;
            let scope = Scope::Resource(terminal.clone());
            state.with_mut(|s| {
                s.agent_records_mut().note_explicit_delete(&terminal);
                s.metadata_delete(&scope, RESOURCE_AGENT_KEY);
            });
            drain(
                &state,
                &terminal,
                vec![report_of("codex", DetectedState::Working)],
            )
            .await;
            let record = stored(&state, &terminal).expect("rewritten");
            assert_eq!(record.kind.as_deref(), Some("codex"));
            assert_eq!(record.state, "working");
        })
        .await;
    }

    /// phux-uaon: an identity-only SET that reached the store but not the
    /// arbiter still survives the next detector write.
    #[tokio::test(flavor = "current_thread")]
    async fn a_subsequent_detector_write_merges_over_an_identity_only_set() {
        local(async {
            let (state, terminal) = pane(None);
            drain(&state, &terminal, vec![claude(DetectedState::Blocked)]).await;
            let scope = Scope::Resource(terminal.clone());
            state.with_mut(|s| {
                s.metadata_set(
                    &scope,
                    RESOURCE_AGENT_KEY,
                    br#"{"name":"reviewer","session":"fleet-7"}"#.to_vec(),
                );
            });
            drain(&state, &terminal, vec![claude(DetectedState::Blocked)]).await;
            let record = stored(&state, &terminal).expect("merged");
            assert_eq!(record.name, "reviewer");
            assert_eq!(record.session.as_deref(), Some("fleet-7"));
            assert_eq!(record.state, "blocked");
            assert_eq!(record.kind.as_deref(), Some("claude"));
        })
        .await;
    }

    /// ADR-0036 tier 2: a `phux-ask` marker lands in the same ledger a
    /// `REPORT_ASKED` hook writes to, and clearing it retracts the ask.
    #[tokio::test(flavor = "current_thread")]
    async fn an_ask_sentinel_lands_in_the_ledger_the_hook_shares() {
        local(async {
            let state = SharedState::new();
            let (pane, terminal) = state.with_mut(|s| {
                let (_session, _window, pane) = s.seed_session("demo");
                (pane, s.intern_terminal_wire(pane))
            });
            let ask = AskedPayload {
                id: "q1".to_owned(),
                question: "Deploy to prod?".to_owned(),
                suggestions: vec!["Yes".to_owned(), "No".to_owned()],
                elapsed_seconds: None,
            };
            drain(
                &state,
                &terminal,
                vec![AgentDetectEvent::AskSentinel(Some(ask))],
            )
            .await;
            assert_eq!(
                state.with(|s| s.current_agent_asked(pane).map(|p| p.id.clone())),
                Some("q1".to_owned()),
            );
            drain(&state, &terminal, vec![AgentDetectEvent::AskSentinel(None)]).await;
            assert!(state.with(|s| s.current_agent_asked(pane).is_none()));
        })
        .await;
    }
}
