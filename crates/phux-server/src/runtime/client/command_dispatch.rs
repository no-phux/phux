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
    /// Its own task beside the dispatcher: a checkout runs Git or `gh` for
    /// seconds, and neither control frames nor uploads should queue behind
    /// it.
    Detached,
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
    if matches!(command, Command::Checkout { .. }) {
        return Route::Detached;
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
