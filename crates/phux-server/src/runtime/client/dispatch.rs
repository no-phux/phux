//! Admitting one framed message, and routing an admitted command to the input lane, the bulk worker, a detached task, or its handler.

use super::{
    BytesMut, CancellationToken, ClientId, Command, CommandResult, ConnectionClose, DetachReason,
    ErrorCode, FrameKind, FrameOrigin, InputCredits, InputLaneHandle, InputStalled, JoinSet,
    NegotiatedConnection, Outbound, SharedState, WireResourceId, before_hello_close,
    decode_client_frame, is_pre_hello_type, mpsc, reject_frame_before_hello,
};

pub(super) const MAX_PENDING_INPUT_RECEIPTS: usize = 128;

pub(super) fn spawn_input_receipt(
    receipts: &mut JoinSet<()>,
    slot: tokio::sync::OwnedSemaphorePermit,
    out_tx: mpsc::Sender<Outbound>,
    request_id: u32,
    receipt: crate::runtime::input_lane::InputReceipt,
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

pub(super) const fn requires_terminal_stream(frame: &FrameKind) -> bool {
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

pub(super) fn validate_dispatch_frame(
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
pub(in crate::runtime) enum Route {
    /// The input lane: acknowledged or routed input to a local Terminal.
    InputLane,
    /// The connection's bulk worker, retaining this many payload bytes.
    Bulk(usize),
    /// Its own task beside the dispatcher: a checkout runs Git or `gh` for
    /// seconds, and neither control frames nor uploads should queue behind
    /// it.
    Detached,
    /// The command handler.
    Handler,
}

/// The route an admitted command takes. An approved held command asks the
/// same question (ADR-0128), so it routes as it would have unheld.
pub(in crate::runtime) fn route(command: &Command, has_input_lane: bool) -> Route {
    let local_input = matches!(
        command,
        Command::ApplyInput { terminal_id, .. } | Command::RouteInput { terminal_id, .. }
            if matches!(terminal_id, WireResourceId::Local { .. })
    );
    if has_input_lane && local_input {
        return Route::InputLane;
    }
    if matches!(command, Command::Checkout { .. }) {
        return Route::Detached;
    }
    crate::runtime::command_tasks::CommandTasks::retained_bytes(command)
        .map_or(Route::Handler, Route::Bulk)
}

pub(super) struct CommandDispatch<'a> {
    pub(super) state: &'a SharedState,
    pub(super) client_id: ClientId,
    pub(super) out_tx: &'a mpsc::Sender<Outbound>,
    pub(super) input_lane: Option<&'a InputLaneHandle>,
    pub(super) token: &'a CancellationToken,
    pub(super) root_token: &'a CancellationToken,
    pub(super) selection: NegotiatedConnection,
    pub(super) command_tasks: &'a mut crate::runtime::command_tasks::CommandTasks,
    pub(super) input_receipts: &'a mut JoinSet<()>,
    pub(super) input_receipt_slots: &'a std::sync::Arc<tokio::sync::Semaphore>,
    /// This connection's pane input credits (ADR-0144).
    pub(super) input_credits: &'a mut InputCredits,
    /// The waiters of this connection's held commands (ADR-0128). Aborted
    /// with the connection, which withdraws their approvals.
    pub(super) held_commands: &'a mut JoinSet<()>,
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

    pub(super) async fn run_inner(
        mut self,
        request_id: u32,
        command: Command,
    ) -> Option<WireResourceId> {
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

        match (route(&command, self.input_lane.is_some()), self.input_lane) {
            (Route::InputLane, Some(lane)) => {
                crate::runtime::commands::note_local_use(self.state, self.client_id, &command);
                self.submit_input(lane, request_id, command).await;
                return None;
            }
            (Route::Bulk(retained), _) => {
                self.submit_bulk(request_id, command, retained, command_started)
                    .await;
                return None;
            }
            (Route::Detached, _) => {
                self.submit_detached(request_id, command, command_started)
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
    pub(super) fn context(&self) -> crate::runtime::commands::CommandContext {
        crate::runtime::commands::CommandContext {
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
    pub(super) async fn hold(&mut self, request_id: u32, command: Command) {
        crate::runtime::approvals::hold_command(
            self.context(),
            self.held_commands,
            request_id,
            command,
        )
        .await;
    }

    pub(super) async fn submit_input(
        &mut self,
        lane: &InputLaneHandle,
        request_id: u32,
        command: Command,
    ) {
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

    pub(super) async fn submit_bulk(
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
pub(super) static DETACHED_COMMANDS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(64)));

impl CommandDispatch<'_> {
    /// Run `command` on its own task so the dispatcher keeps reading. The
    /// connection's cancellation ends it; its result is still correlated by
    /// `request_id`.
    pub(super) async fn submit_detached(
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
pub(super) async fn run_handler(
    ctx: &crate::runtime::commands::CommandContext,
    request_id: u32,
    command: Command,
    started: std::time::Instant,
) {
    crate::runtime::commands::handle_command(ctx, request_id, command).await;
    crate::perf::CMD_HANDLE.record_elapsed(started);
}
