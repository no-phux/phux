use super::hello::NegotiatedConnection;
use super::hello::{
    before_hello_close, decode_client_frame, is_pre_hello_type, reject_frame_before_hello,
};
use super::plumbing::ConnectionClose;
use crate::runtime::apps::{ConnectionLane, InputLane};
use crate::state::{ClientId, Outbound, SharedState};
use crate::transport::FrameOrigin;
use bytes::BytesMut;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, DetachReason, ErrorCode, FrameKind};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::trace;

pub(super) const MAX_PENDING_INPUT_RECEIPTS: usize = 128;

fn spawn_input_receipt(
    receipts: &mut JoinSet<()>,
    slot: tokio::sync::OwnedSemaphorePermit,
    out_tx: mpsc::Sender<Outbound>,
    request_id: u32,
    receipt: futures_util::future::LocalBoxFuture<'static, CommandResult>,
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
            | FrameKind::ClipboardReply { .. }
            | FrameKind::FrameAck { .. }
            | FrameKind::HistoryRequest { .. }
    )
}

/// Why a framed client message is not dispatched.
pub(super) enum Undispatched {
    /// A protocol violation: the connection ends.
    Close(ConnectionClose),
    /// Well framed, but newer than this build understands (protocol 1.x):
    /// queue this `Unsupported` answer and keep reading.
    Refuse(Box<FrameKind>),
}

impl From<ConnectionClose> for Undispatched {
    fn from(close: ConnectionClose) -> Self {
        Self::Close(close)
    }
}

/// Validate one framed message for dispatch, then run the frame guard
/// (workload-auth §6) before any routing or handler. A message this build
/// does not support, or the guard refuses, is answered here and yields
/// `Ok(None)`: the connection keeps reading. A violation is the `Err` that
/// ends it.
pub(super) async fn admit_frame(
    state: &SharedState,
    framed: &BytesMut,
    negotiated: Option<&NegotiatedConnection>,
    origin: FrameOrigin,
    client_id: ClientId,
    out_tx: &mpsc::Sender<Outbound>,
) -> Result<Option<FrameKind>, ConnectionClose> {
    let frame = match validate_dispatch_frame(framed, negotiated, origin, client_id) {
        Ok(frame) => frame,
        Err(Undispatched::Close(close)) => return Err(close),
        Err(Undispatched::Refuse(refusal)) => {
            if out_tx.send(Outbound::Frame(*refusal)).await.is_err() {
                trace!(?client_id, "Unsupported refusal dropped: writer gone");
            }
            return Ok(None);
        }
    };
    let refused = crate::runtime::dispatch_guard::refuse_frame(
        state,
        client_id,
        &frame,
        negotiated.is_some(),
        out_tx,
    )
    .await;
    Ok((!refused).then_some(frame))
}

pub(super) fn validate_dispatch_frame(
    framed: &BytesMut,
    negotiated: Option<&NegotiatedConnection>,
    origin: FrameOrigin,
    client_id: ClientId,
) -> Result<FrameKind, Undispatched> {
    // Before HELLO only HELLO and PING are legal. Refuse anything else by its
    // type byte, so an unauthenticated peer never reaches the other body
    // decoders (server-to-client snapshots and lists included).
    if negotiated.is_none() && !is_pre_hello_type(framed) {
        return Err(before_hello_close(client_id).into());
    }
    let frame = decode_client_frame(framed, negotiated)?;
    if negotiated.is_some_and(|selection| selection.quic_streams())
        && origin == FrameOrigin::Control
        && requires_terminal_stream(&frame)
    {
        return Err(Undispatched::Close(ConnectionClose {
            attached_reason: Some("Terminal frame sent on QUIC control stream"),
            detach_reason: DetachReason::ProtocolError,
            code: ErrorCode::MalformedMessage,
            message: "Terminal-scoped frame requires a bound QUIC stream".to_owned(),
        }));
    }
    if let Some(close) = reject_frame_before_hello(&frame, negotiated.is_some(), client_id) {
        return Err(close.into());
    }
    Ok(frame)
}

/// Where an admitted command is dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime) enum Route {
    /// The input lane (ADR-0044), for the commands it carries.
    InputLane,
    /// The connection's bulk worker, retaining this many payload bytes.
    Bulk(usize),
    /// Its own task beside the dispatcher: a checkout runs Git or `gh` for
    /// seconds, and neither control frames nor uploads should queue behind
    /// it.
    Detached,
    /// The connection's service exchanges, which bound their own tasks.
    ServiceHttp,
    /// The command handler.
    Handler,
}

/// The route an admitted command takes. An approved held command asks the
/// same question (ADR-0128), so it routes as it would have unheld.
pub(in crate::runtime) fn route(command: &Command, input_lane: Option<&InputLane>) -> Route {
    if input_lane.is_some_and(|lane| lane.rides(command)) {
        return Route::InputLane;
    }
    // A copy or paste waits on the producer's clipboard, and a mirror
    // start for its surface; nothing else on the connection should wait.
    if matches!(
        command,
        Command::Checkout { .. }
            | Command::Credentials {
                op: phux_protocol::credentials::CredentialOp::Request(_)
            }
            | Command::Mirror { .. }
            | Command::SendDocumentInput {
                intent: phux_protocol::wire::frame::DocumentInput::CopyClipboard { .. }
                    | phux_protocol::wire::frame::DocumentInput::CopySelection { .. }
                    | phux_protocol::wire::frame::DocumentInput::Paste { .. },
                ..
            }
    ) {
        return Route::Detached;
    }
    if crate::runtime::service_http::is_service_http(command) {
        return Route::ServiceHttp;
    }
    crate::runtime::command_tasks::CommandTasks::retained_bytes(command)
        .map_or(Route::Handler, Route::Bulk)
}

pub(super) struct CommandDispatch<'a> {
    pub(in crate::runtime) state: &'a SharedState,
    pub(in crate::runtime) client_id: ClientId,
    pub(in crate::runtime) out_tx: &'a mpsc::Sender<Outbound>,
    /// This connection's place on the input lane, with its pane input
    /// credits (ADR-0144).
    pub(in crate::runtime) lane: Option<&'a mut ConnectionLane>,
    pub(in crate::runtime) token: &'a CancellationToken,
    pub(super) root_token: &'a CancellationToken,
    pub(super) selection: NegotiatedConnection,
    pub(super) command_tasks: &'a mut crate::runtime::command_tasks::CommandTasks,
    pub(super) input_receipts: &'a mut JoinSet<()>,
    pub(super) input_receipt_slots: &'a std::sync::Arc<tokio::sync::Semaphore>,
    /// The waiters of this connection's held commands (ADR-0128). Aborted
    /// with the connection, which withdraws their approvals.
    pub(super) held_commands: &'a mut JoinSet<()>,
    /// This connection's open service exchanges.
    pub(super) service_http: &'a crate::runtime::service_http::Exchanges,
}

pub(super) enum CommandDispatchOutcome {
    Completed(Option<WireResourceId>),
    Cancelled,
}

impl CommandDispatch<'_> {
    pub(super) async fn run(self, request_id: u32, command: Command) -> CommandDispatchOutcome {
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
        match crate::runtime::dispatch_guard::guard_command(
            self.state,
            self.client_id,
            request_id,
            &command,
            self.out_tx,
        )
        .await
        {
            crate::runtime::dispatch_guard::Guarded::Admitted => {}
            crate::runtime::dispatch_guard::Guarded::Refused => return None,
            crate::runtime::dispatch_guard::Guarded::Held => {
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

        match route(&command, self.lane.as_deref().map(ConnectionLane::lane)) {
            Route::InputLane => {
                crate::runtime::commands::note_local_use(self.state, self.client_id, &command);
                self.submit_input(request_id, command, command_started)
                    .await;
                return None;
            }
            Route::Bulk(retained) => {
                self.submit_bulk(request_id, command, retained, command_started)
                    .await;
                return None;
            }
            Route::Detached => {
                self.submit_detached(request_id, command, command_started)
                    .await;
                return None;
            }
            Route::ServiceHttp => {
                crate::runtime::service_http::submit(
                    self.context(),
                    self.service_http,
                    request_id,
                    command,
                )
                .await;
                return None;
            }
            Route::Handler => {}
        }
        run_handler(&self.context(), request_id, command, command_started).await;
        detached_stream
    }

    /// This connection's dispatch context, owned, for work that outlives
    /// the dispatch.
    fn context(&self) -> crate::runtime::commands::CommandContext {
        crate::runtime::commands::CommandContext {
            state: self.state.clone(),
            client_id: self.client_id,
            out_tx: self.out_tx.clone(),
            client_caps: self.selection.client_caps,
            profile: self.selection.profile,
            limits: self.selection.limits,
            input_lane: self.lane.as_deref().map(|lane| lane.lane().clone()),
            token: self.token.clone(),
            root_token: self.root_token.clone(),
            defer_subscription: self.selection.quic_streams(),
        }
    }

    /// Hold a command the guard held (ADR-0128). Its waiter runs it later
    /// in this connection's context, exactly as this dispatch would have.
    async fn hold(&mut self, request_id: u32, command: Command) {
        crate::runtime::approvals::hold_command(
            self.context(),
            self.held_commands,
            request_id,
            command,
        )
        .await;
    }

    async fn submit_input(
        &mut self,
        request_id: u32,
        command: Command,
        command_started: std::time::Instant,
    ) {
        let Some(lane) = self.lane.as_deref_mut() else {
            // `route` sends nothing here without a lane; should it, the
            // handler still answers the command.
            run_handler(&self.context(), request_id, command, command_started).await;
            return;
        };
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
        let receipt = match lane.begin(self.state, self.client_id, command).await {
            Ok(receipt) => receipt,
            Err(result) => {
                let _ = self
                    .out_tx
                    .send(Outbound::Frame(FrameKind::CommandResult {
                        request_id,
                        result,
                    }))
                    .await;
                return;
            }
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

/// Detached commands in flight across every connection. Each local checkout
/// is bounded again by its worker pool; this bounds relayed ones waiting on a
/// satellite.
static DETACHED_COMMANDS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(64)));

impl CommandDispatch<'_> {
    /// Run `command` on its own task so the dispatcher keeps reading. The
    /// connection's cancellation ends it; its result is still correlated by
    /// `request_id`.
    async fn submit_detached(
        &self,
        request_id: u32,
        command: Command,
        command_started: std::time::Instant,
    ) {
        let Ok(slot) = DETACHED_COMMANDS.clone().try_acquire_owned() else {
            let _ = self
                .out_tx
                .send(Outbound::Frame(FrameKind::CommandResult {
                    request_id,
                    result: CommandResult::Error {
                        code: ErrorCode::ResourceExhausted,
                        message: "too many long-running commands in flight; retry".to_owned(),
                    },
                }))
                .await;
            return;
        };
        let ctx = self.context();
        tokio::task::spawn_local(async move {
            let _slot = slot;
            let token = ctx.token.clone();
            tokio::select! {
                biased;
                () = token.cancelled() => {}
                () = run_handler(&ctx, request_id, command, command_started) => {}
            }
        });
    }
}

/// Run one admitted command through the handler.
async fn run_handler(
    ctx: &crate::runtime::commands::CommandContext,
    request_id: u32,
    command: Command,
    started: std::time::Instant,
) {
    crate::runtime::commands::handle_command(ctx, request_id, command).await;
    crate::perf::CMD_HANDLE.record_elapsed(started);
}

#[cfg(test)]
mod input_receipt_capacity_tests;
