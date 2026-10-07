//! The [`TerminalActor`] event loop (`run`) and the state-sync tick
//! emitter (`tick_emit`).

use super::{
    AgentDetector, Bytes, CanonicalTerminal, ClientId, ConsumerAckRequest, ConsumerDetachRequest,
    ConsumerSyncState, DEFAULT_TICK_INTERVAL, EncodedInputRequest, FrameKind, MAX_EMIT_INSTANTS,
    MAX_INPUT_COALESCE, MAX_PTY_COALESCE, MAX_PTY_COALESCE_BYTES, NativeOrPty, Outbound,
    PaneOutput, PaneUpgradeHandle, PtyEvent, PwdRequest, RESIZE_RESYNC_DEBOUNCE, ResizeRequest,
    ResyncAudience, ResyncReason, ResyncTarget, ScreenReply, ScreenRequest,
    SetDefaultColorsRequest, SnapshotBytes, SnapshotRequest, TerminalActor, TerminalInput,
    UpgradeHandleRequest, debug, error, mpsc, recv_native_or_pty, tick, trace, warn,
};
use crate::grid::SnapshotSynthesizer;
use crate::grid::SynthesisError;
use crate::grid::reference::ReferenceCursorMode;

#[path = "run_loop_service.rs"]
mod service;

#[cfg(test)]
#[path = "tests_run_loop_service.rs"]
mod tests_service;

/// What the `run` loop must do after one PTY-ingress turn.
enum PtyTurn {
    /// Keep looping with `native_step_due` untouched (the EOF path).
    Continue,
    /// Keep looping with a freshly recomputed `native_step_due`.
    Stepped(bool),
    /// The raw output sequence is exhausted; the PTY is torn down.
    Shutdown,
}

/// One bounded PTY read burst and why the drain stopped.
struct PtyBurst {
    /// The chunks to write to the `Terminal` and broadcast as one frame.
    payload: Bytes,
    /// Reader chunks folded into `payload` (`pty.burst.chunks`).
    chunks: u64,
    /// A queued EOF was observed while draining; handle it after the flush.
    saw_eof: bool,
    /// The drain stopped at the byte cap (more output likely queued), so the
    /// loop yields before the next bounded parse.
    hit_byte_cap: bool,
}

/// Consumer-independent render products of one state-sync tick.
#[derive(Clone, Copy)]
struct TickRender {
    /// Grid width the tick rendered at.
    cols: u16,
    /// Grid height the tick rendered at.
    rows: u16,
    /// Live cursor/mode capture shared by every consumer's diff.
    live_cm: ReferenceCursorMode,
}

/// What one consumer's slot in a state-sync tick produced.
enum TickOutcome {
    /// Nothing shipped (not tick-managed, gated, backpressured, or unchanged).
    Skipped,
    /// The consumer's outbound mailbox is closed; reap the entry.
    Closed,
    /// A `ResourceOutput` frame shipped, carrying this many payload bytes.
    Emitted(usize),
}

/// The consumer walk of one productive tick.
struct TickEmitWalk {
    /// Frames actually shipped this tick.
    emitted: u64,
    /// Sum of shipped payload bytes.
    total_out_bytes: usize,
    /// Consumers whose outbound mailbox was closed; reap after borrows drop.
    closed: Vec<ClientId>,
    /// Instant the productive work started (excludes gated/idle ticks).
    started: std::time::Instant,
}

/// Which half of the cooperative native-capture pump this turn owes: a
/// yield to the runtime or one record step, alternating so capture advances
/// without starving sibling tasks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BootstrapPump {
    /// No capture work in flight: both pump arms stay disabled.
    Idle,
    /// This turn owes the in-flight bootstrap one record step.
    StepDue,
    /// This turn owes the runtime a yield before the next record step.
    YieldDue,
}

impl BootstrapPump {
    /// Resolve from bootstrap state and whether a step is owed.
    const fn resolve(work_pending: bool, step_owed: bool) -> Self {
        if !work_pending {
            return Self::Idle;
        }
        if step_owed {
            Self::StepDue
        } else {
            Self::YieldDue
        }
    }
}

/// One claim on the debounced resync: why, and for a gap, which pump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OwedResync {
    /// Why the requester's generation cannot continue.
    pub(super) reason: ResyncReason,
    /// The pump that fell behind; `None` means every subscriber.
    pub(super) target: Option<ResyncTarget>,
}

/// The debounced post-resize resync. Re-armed per resize; on fire, one
/// snapshot at the settled size. A reflow is owed to everyone, a gap only to
/// the pumps that asked; all share one deadline and one synthesis.
pub(super) struct ResyncDebounce {
    /// A resync is owed once the deadline lands.
    pending: bool,
    /// Why the owed resync was armed; broadcast when the deadline fires.
    reason: ResyncReason,
    /// Every subscriber is owed the resync, whatever `targets` says.
    everyone: bool,
    /// Pumps owed a gap resync, deduplicated.
    targets: Vec<ResyncTarget>,
}

impl ResyncDebounce {
    /// Nothing owed.
    pub(super) const fn idle() -> Self {
        Self {
            pending: false,
            reason: ResyncReason::Resize,
            everyone: false,
            targets: Vec::new(),
        }
    }

    /// Whether the owed snapshot may fire (none is blocked by a bootstrap).
    const fn may_fire(&self, bootstrap_pending: bool) -> bool {
        self.pending && !bootstrap_pending
    }

    /// Owe a resync for `reason` and (re)start the debounce.
    ///
    /// An already-owed gap resync does not push the deadline out: N pumps
    /// retrying independently would otherwise keep resetting it and livelock.
    /// Resizes always re-arm (the last size wins). The audience is recorded
    /// first either way.
    fn arm(&mut self, owed: OwedResync, deadline: std::pin::Pin<&mut tokio::time::Sleep>) {
        let coalesce_gap = self.pending && owed.reason == ResyncReason::OutboundGap;
        self.include(owed);
        if coalesce_gap {
            return;
        }
        deadline.reset(tokio::time::Instant::now() + RESIZE_RESYNC_DEBOUNCE);
    }

    /// Fold `owed` into the audience without touching the deadline (PTY EOF:
    /// the snapshot is about to fire).
    fn include(&mut self, owed: OwedResync) {
        match owed.target {
            None => self.everyone = true,
            Some(target) if !self.targets.contains(&target) => self.targets.push(target),
            Some(_) => {}
        }
        if self.pending && owed.reason == ResyncReason::OutboundGap {
            return;
        }
        self.pending = true;
        self.reason = owed.reason;
    }

    /// Clear the owed resync, returning its reason and audience.
    fn take(&mut self) -> (ResyncReason, ResyncAudience) {
        self.pending = false;
        let targets = std::mem::take(&mut self.targets);
        let audience = if std::mem::replace(&mut self.everyone, false) {
            ResyncAudience::Everyone
        } else {
            ResyncAudience::Only(targets.into())
        };
        (self.reason, audience)
    }
}

/// Loop-local timers and ingress preference owned by [`TerminalActor::run`].
struct RunLoopState {
    /// Shared state-sync cadence; rebuilt when a consumer's RTT shifts it.
    tick_interval: std::time::Duration,
    /// The armed state-sync interval (Delay + first tick eaten).
    tick: tokio::time::Interval,
    /// Agent-detector cadence; rebuilt when `detect_tick` asks for a new one.
    detect_interval: std::time::Duration,
    /// The armed detector interval.
    detect_tick: tokio::time::Interval,
    /// False after intentional disarming: the first resumed deadline is stale.
    detect_deadline_valid: bool,
    /// Debounced post-resize / gap resync owed to subscribers.
    resync: ResyncDebounce,
    /// Next combined ingress prefers native control when true.
    prefer_native: bool,
    /// The cooperative native pump owes a record step on the next turn.
    native_step_due: bool,
    /// Round-robin service opportunity owed after a bounded ingress turn.
    service: service::ServiceRotation,
}

impl TerminalActor {
    /// Run the actor's event loop until shutdown. Native prefix capture
    /// advances by one record between ingress turns.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    pub async fn run(mut self) {
        debug!(
            cols = self.cols,
            rows = self.rows,
            has_pty = self.pty.is_some(),
            "TerminalActor started",
        );

        let mut state = self.arm_run_loop().await;
        // Far-future until a resize arms it.
        let resync_deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
        tokio::pin!(resync_deadline);
        self.drive_run_loop(&mut state, resync_deadline).await;
    }

    /// Arm the state-sync tick, the detector, and idle bookkeeping.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    async fn arm_run_loop(&mut self) -> RunLoopState {
        // State-sync tick: starts at `DEFAULT_TICK_INTERVAL` and adapts to
        // the fastest consumer's RTT/2 (see `adaptive_tick_interval`).
        let tick_interval = DEFAULT_TICK_INTERVAL;
        let tick = armed_interval(tick_interval).await;

        self.install_agent_detector();
        let detect_interval = crate::agent_detect::TICK_UNIDENTIFIED;
        let detect_tick = armed_interval(detect_interval).await;

        // Native control and PTY output share one arm, alternating
        // preference; both stay enabled so a silent PTY never parks history.
        RunLoopState {
            tick_interval,
            tick,
            detect_interval,
            detect_tick,
            detect_deadline_valid: true,
            resync: ResyncDebounce::idle(),
            prefer_native: false,
            native_step_due: false,
            service: service::ServiceRotation::default(),
        }
    }

    /// Drive the `select!` until cancel, sequence exhaustion, or every
    /// mailbox closes.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    #[expect(
        clippy::cognitive_complexity,
        reason = "one biased select! whose arms are each a single handler call; the macro expansion and the per-arm bootstrap guards carry the score, and splitting the select would break its priority order"
    )]
    async fn drive_run_loop(
        &mut self,
        state: &mut RunLoopState,
        mut resync_deadline: std::pin::Pin<&mut tokio::time::Sleep>,
    ) {
        loop {
            // One read serves every guard below.
            let bootstrap_pending = self.native_bootstrap_pending();
            let pump = BootstrapPump::resolve(self.native_work_pending(), state.native_step_due);
            let detector_armed = self.detector_tick_armed(bootstrap_pending);
            if !detector_armed {
                state.detect_deadline_valid = false;
            }

            tokio::select! {
                biased;

                () = self.core.token.cancelled() => {
                    self.shutdown_from_cancel(&mut state.resync).await;
                    return;
                }

                // Encoded input from the input lane: highest priority.
                Some(request) = self.encoded_input_rx.recv() =>
                    self.service_encoded_input_batch(request),

                // Inline input (ROUTE_INPUT and attached input events), encoded here.
                // Before PTY output so keystrokes are not starved; batch
                // bounded by `MAX_INPUT_COALESCE`.
                Some(input) = self.input_rx.recv(), if !bootstrap_pending =>
                    self.service_input_batch(&input),

                // Apply admitted geometry before a bootstrap/screen capture
                // or another PTY turn; output pressure cannot starve the
                // final size. In-flight native cuts still finish first.
                Some(req) = self.resize_rx.recv(), if !bootstrap_pending =>
                    self.service_resize_request(req, &mut state.resync, resync_deadline.as_mut()),

                () = std::future::ready(()), if pump == BootstrapPump::StepDue =>
                    self.service_cooperative_native_step(&mut state.native_step_due),

                // Never await here: offer one ready service, then let ingress
                // continue. The rotation includes timers and cannot be monopolized
                // by a single busy control channel.
                () = std::future::ready(()), if state.service.due =>
                    self.service_pending_turn(state, resync_deadline.as_mut()),

                ingress = recv_native_or_pty(
                    &mut self.native_requests,
                    self.pty_rx.as_mut(),
                    state.prefer_native,
                ) => {
                    state.service.due = true;
                    if self.service_ingress_turn(ingress, state).await.is_break() {
                        return;
                    }
                },

                Some(req) = self.snapshot_rx.recv(), if !bootstrap_pending =>
                    self.reply_bounded_snapshot(req),

                Some(req) = self.set_default_colors_rx.recv(), if !bootstrap_pending =>
                    self.install_client_default_colors(req),

                Some(req) = self.screen_rx.recv(), if !bootstrap_pending =>
                    self.reply_screen_state(req),

                Some(req) = self.upgrade_rx.recv(), if !bootstrap_pending =>
                    self.reply_upgrade_handle(req),

                Some(req) = self.pwd_rx.recv() => self.reply_pane_cwd(req),

                Some(req) = self.process_rx.recv() => self.reply_process_facet(req),

                // Debounced resize resync, once the storm settles.
                () = &mut resync_deadline, if state.resync.may_fire(bootstrap_pending) =>
                    self.fire_owed_resync(&mut state.resync),

                Some(req) = self.consumer_attach_rx.recv(), if !bootstrap_pending =>
                    self.handle_consumer_attach(req),

                Some(req) = self.consumer_detach_rx.recv() =>
                    self.service_consumer_detach(req, &mut state.tick, &mut state.tick_interval),

                // FRAME_ACK: advances the consumer's acked reference. A lost
                // ack only means a larger diff next tick.
                Some(req) = self.consumer_ack_rx.recv(), if !bootstrap_pending =>
                    self.service_frame_ack(&req, &mut state.tick, &mut state.tick_interval),

                // Supervisory control (ADR-0033): lease broadcasts and signals.
                Some(req) = self.core.control_rx.recv() => self.handle_control_request(req),

                // Disarmed when there is nothing to do (`state_tick_armed`),
                // so an idle pane has no timer wakeups. Every event that can
                // make the tick relevant is itself a loop turn, so re-arming
                // is immediate.
                _ = state.tick.tick(), if !bootstrap_pending && self.state_tick_armed() =>
                    self.service_state_tick(),

                // Agent-state detector (ADR-0046): driven only by this
                // adaptive interval, never by PTY bytes.
                scheduled = state.detect_tick.tick(), if detector_armed =>
                    self.service_detector_deadline(scheduled, state),

                () = tokio::task::yield_now(), if pump == BootstrapPump::YieldDue =>
                    state.native_step_due = true,

                else => break,
            }
        }
    }

    /// Fire any owed gap resync, then tear the PTY down after the
    /// actor-global cancel token fires.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    async fn shutdown_from_cancel(&mut self, resync: &mut ResyncDebounce) {
        let _ = self.flush_final_gap_resync(resync);
        debug!("TerminalActor cancellation token fired");
        self.shutdown_pty().await;
    }

    /// Drain queued gap-resync requests and fire any owed snapshot now, so
    /// an EOF'd pane does not get reaped before its fenced pumps are served.
    pub(super) fn flush_final_gap_resync(&mut self, resync: &mut ResyncDebounce) -> bool {
        // Callers run outside the bootstrap guards, and both the drain and
        // the fire touch the canonical terminal, so land any in-flight
        // capture first.
        self.land_native_cuts();
        while let Ok(req) = self.resize_rx.try_recv() {
            for owed in self.apply_resize_request(req) {
                resync.include(owed);
            }
        }
        if !resync.pending {
            return false;
        }
        self.fire_owed_resync(resync);
        true
    }

    /// After PTY EOF: publish the last grid to everyone (queued gap requests
    /// ride it), then tell the exit watcher the child is gone. Exactly one
    /// replacement generation, and close never waits on mailbox room.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    async fn flush_exit_resync_if_needed(&mut self, resync: &mut ResyncDebounce) {
        if self.pty_rx.is_some() || self.core.exit_notify.is_none() || self.exit.is_none() {
            return;
        }
        let _ = self.flush_final_gap_resync(resync);
        // Offer the exit grid before notifying the watcher. Natural close
        // also captures a fresh final generation and awaits each pump;
        // a scheduling yield alone cannot order a backpressured publisher.
        self.broadcast_resync(ResyncReason::Exit, ResyncAudience::Everyone);
        tokio::task::yield_now().await;
        if let Some(exit) = self.exit.as_ref() {
            self.core.notify_exit(phux_core::process::ExitOutcome {
                status: exit.status,
                signal: exit.signal,
            });
        }
    }

    /// Advance native prefix capture by one record and clear the owed-step flag.
    fn service_cooperative_native_step(&mut self, native_step_due: &mut bool) {
        self.cooperative_native_step();
        *native_step_due = false;
    }

    /// Broadcast the settled-resize / gap snapshot this debounce window owed.
    fn fire_owed_resync(&self, resync: &mut ResyncDebounce) {
        let (reason, audience) = resync.take();
        self.broadcast_resync(reason, audience);
    }

    /// Service one combined native-control / PTY-output ingress turn, then
    /// publish the exit resync if that turn saw the PTY close.
    /// `Break` means the raw output sequence is exhausted.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    async fn service_ingress_turn(
        &mut self,
        ingress: NativeOrPty,
        state: &mut RunLoopState,
    ) -> std::ops::ControlFlow<()> {
        let flow = self
            .service_ingress(
                ingress,
                &mut state.prefer_native,
                &mut state.native_step_due,
            )
            .await;
        if flow.is_continue() {
            self.flush_exit_resync_if_needed(&mut state.resync).await;
        }
        flow
    }

    /// Service the native request or PTY event itself, alternating the
    /// combined arm's preference.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    async fn service_ingress(
        &mut self,
        ingress: NativeOrPty,
        prefer_native: &mut bool,
        native_step_due: &mut bool,
    ) -> std::ops::ControlFlow<()> {
        match ingress {
            NativeOrPty::Native(req) => {
                *prefer_native = false;
                self.handle_native_actor_request(req);
                *native_step_due = false;
            }
            NativeOrPty::Pty(evt) => {
                *prefer_native = true;
                // One bounded parse, then back to the combined arm so
                // control and output alternate.
                match self.service_pty_event(evt).await {
                    PtyTurn::Continue => {}
                    PtyTurn::Stepped(due) => *native_step_due = due,
                    PtyTurn::Shutdown => return std::ops::ControlFlow::Break(()),
                }
            }
        }
        std::ops::ControlFlow::Continue(())
    }

    /// Apply one resize request and arm the resync it earns.
    fn service_resize_request(
        &mut self,
        req: ResizeRequest,
        resync: &mut ResyncDebounce,
        deadline: std::pin::Pin<&mut tokio::time::Sleep>,
    ) {
        let mut deadline = deadline;
        for owed in self.apply_resize_request(req) {
            resync.arm(owed, deadline.as_mut());
        }
    }

    /// Reap a detached consumer and re-evaluate the tick cadence.
    fn service_consumer_detach(
        &mut self,
        req: ConsumerDetachRequest,
        tick: &mut tokio::time::Interval,
        tick_interval: &mut std::time::Duration,
    ) {
        let ConsumerDetachRequest { client_id, reply } = req;
        self.unregister_consumer(client_id);
        trace!(
            ?client_id,
            "consumer detached: per-consumer RenderState freed"
        );
        // Losing the fastest consumer can slow the shared cadence.
        Self::rearm_tick(tick, tick_interval, self.adaptive_tick_interval());
        let _ = reply.send(());
    }

    /// Fold one `FRAME_ACK` into its consumer; rebuild the shared tick only
    /// when the adaptive interval moves past the deadband.
    fn service_frame_ack(
        &mut self,
        req: &ConsumerAckRequest,
        tick: &mut tokio::time::Interval,
        tick_interval: &mut std::time::Duration,
    ) {
        let &ConsumerAckRequest {
            client_id,
            stream_id,
            bootstrap_id,
            seq,
        } = req;
        if self.on_generation_frame_ack(client_id, stream_id, bootstrap_id, seq) {
            Self::rearm_tick(tick, tick_interval, self.adaptive_tick_interval());
        }
    }

    /// One state-sync tick (ADR-0018): diff each tick-managed consumer
    /// against its own reference and ship non-empty deltas.
    pub(super) fn service_state_tick(&mut self) {
        // Close an idle output burst (independent of the emitter gate).
        self.maybe_emit_idle();
        self.tick_emit();
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        self.expire_native_cursors();
    }

    /// Whether the state-sync tick has work: an open output burst, a
    /// tick-managed consumer with dirty or owed work, or native cursors to
    /// expire. Otherwise the timer is not armed at all.
    ///
    /// Safe only because [`armed_interval`] uses `MissedTickBehavior::Delay`:
    /// with `Burst`, re-arming after a long quiet spell would owe every
    /// missed tick at once.
    pub(super) fn state_tick_armed(&self) -> bool {
        if self.in_output_burst {
            return true;
        }
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if !self.native_cursor_owners.is_empty() {
            return true;
        }
        if self.consumer_tick_emits {
            return true;
        }
        self.consumer_states.values().any(|state| {
            state.wants_state_sync
                && (self.terminal_dirty_since_tick || must_walk_when_clean(state))
        })
    }

    /// Whether the detector arm may run: installed, and no bootstrap pending.
    const fn detector_tick_armed(&self, bootstrap_pending: bool) -> bool {
        self.agent_detect.is_some() && !bootstrap_pending
    }

    /// Re-derive agent state and re-arm the detector cadence it asks for.
    fn service_detect_tick(
        &mut self,
        detect_tick: &mut tokio::time::Interval,
        detect_interval: &mut std::time::Duration,
    ) {
        if let Some(next) = self.detect_tick() {
            Self::rearm_tick(detect_tick, detect_interval, next);
        }
    }

    /// Install the agent detector (ADR-0046) at run start, so the grace
    /// anchors when the child begins painting. Only PTY-backed actors with a
    /// sink and a non-empty rule set get one.
    fn install_agent_detector(&mut self) {
        let rules = crate::agent_detect::rules::global();
        if self.pty.is_some() && self.agent_state_sink.is_some() && !rules.is_empty() {
            let mut detector = AgentDetector::new(rules, std::time::Instant::now());
            // ADR-0103 §5: only the spawn path can ask about a live
            // `AgentSession` child; without a probe the answer is "no".
            if let Some(probe) = self.live_session_probe.clone() {
                detector.set_live_session_probe(probe);
            }
            self.agent_detect = Some(detector);
        }
    }

    /// Service one encoded-input wakeup plus up to [`MAX_INPUT_COALESCE`]
    /// already-queued requests.
    fn service_encoded_input_batch(&mut self, request: EncodedInputRequest) {
        self.service_encoded_input(request);
        for _ in 1..MAX_INPUT_COALESCE {
            match self.encoded_input_rx.try_recv() {
                Ok(next) => self.service_encoded_input(next),
                Err(_) => break,
            }
        }
    }

    /// Service one inline-input wakeup plus queued followers.
    fn service_input_batch(&mut self, input: &TerminalInput) {
        self.service_input(input);
        for _ in 1..MAX_INPUT_COALESCE {
            match self.input_rx.try_recv() {
                Ok(next) => self.service_input(&next),
                Err(_) => break,
            }
        }
    }

    /// Ingest one PTY event: a bounded coalesced write and broadcast, or EOF.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    async fn service_pty_event(&mut self, evt: Option<PtyEvent>) -> PtyTurn {
        let Some(PtyEvent::Bytes {
            chunk: first,
            read_at,
        }) = evt
        else {
            // `Some(PtyEvent::Eof)` or a dropped sender (`None`).
            self.handle_pty_eof();
            return PtyTurn::Continue;
        };
        crate::perf::PTY_QUEUE_WAIT.record_elapsed(read_at);
        // Server-side echo latency; slower than the ceiling means the
        // program did not echo.
        if let Some(input_at) = self.last_input_at.take() {
            let since_input = input_at.elapsed();
            if since_input < crate::perf::ECHO_SAMPLE_CEILING {
                crate::perf::ECHO_SERVER.record_duration(since_input);
            }
        }
        self.last_output_at.set(Some(std::time::Instant::now()));
        let burst = self.coalesce_pty_burst(first);
        crate::perf::PTY_BURST_BYTES.record_len(burst.payload.len());
        crate::perf::PTY_BURST_CHUNKS.record(burst.chunks);
        debug!(
            bytes = burst.payload.len(),
            "vt_write: PTY chunk(s) -> Terminal"
        );
        let Some(seq) = self.core.next_seq() else {
            error!("resource output sequence exhausted");
            self.shutdown_pty().await;
            return PtyTurn::Shutdown;
        };
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let deferred = self.buffer_native_live_output(seq, &burst.payload);
        #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
        let deferred = false;
        if !deferred {
            self.ingest_pty_payload(&burst.payload);
        }
        let _ = self.core.output_tx.send(PaneOutput::Live {
            seq,
            bytes: burst.payload,
            at: read_at,
        });
        let native_step_due = self.native_work_pending();
        if burst.saw_eof {
            self.handle_pty_eof();
        } else if burst.hit_byte_cap {
            // Capped with more queued: yield so input and sibling tasks run.
            let yield_started = std::time::Instant::now();
            tokio::task::yield_now().await;
            crate::perf::PTY_YIELD_WAIT.record_elapsed(yield_started);
        }
        PtyTurn::Stepped(native_step_due)
    }

    /// Coalesce chunks queued behind `first` into one write and broadcast.
    /// A lone chunk is copy-free; the drain stops at the chunk cap, EOF, or
    /// before a chunk that would cross `MAX_PTY_COALESCE_BYTES`.
    fn coalesce_pty_burst(&mut self, first: Bytes) -> PtyBurst {
        let mut queued = std::mem::take(&mut self.pty_burst);
        let mut total = first.len();
        let mut saw_eof = false;
        let mut hit_byte_cap = false;
        for _ in 0..MAX_PTY_COALESCE {
            // The first chunk always lands; only coalescing is capped.
            if total >= MAX_PTY_COALESCE_BYTES {
                hit_byte_cap = true;
                break;
            }
            match self.pty_rx.as_mut().map(mpsc::Receiver::try_recv) {
                Some(Ok(PtyEvent::Bytes { chunk: more, .. })) => {
                    total += more.len();
                    queued.push(more);
                }
                // Flush coalesced bytes, then handle EOF.
                Some(Ok(PtyEvent::Eof)) => {
                    saw_eof = true;
                    break;
                }
                // Empty or disconnected (EOF arrives on the next wakeup).
                _ => break,
            }
        }
        let chunks = 1 + queued.len() as u64;
        let payload = join_pty_burst(first, &queued, total);
        queued.clear();
        self.pty_burst = queued;
        PtyBurst {
            payload,
            chunks,
            saw_eof,
            hit_byte_cap,
        }
    }

    /// Write one coalesced payload into the canonical `Terminal` and fan out
    /// everything derived from it (color queries, encoder snapshot, dirty
    /// bits, semantic events, native bootstrap advance). The OSC scanners
    /// skip plain text with `memchr`; the FFI reads are cheap enough to stay
    /// unconditional.
    fn ingest_pty_payload(&mut self, payload: &Bytes) {
        let _apply_timer = crate::perf::PTY_VT_APPLY.timer();
        let parse_started = std::time::Instant::now();
        self.terminal.borrow_mut().vt_write(payload);
        crate::perf::PTY_VT_PARSE.record_elapsed(parse_started);
        let _post_timer = crate::perf::PTY_POST_APPLY.timer();
        self.answer_color_queries(payload);
        self.publish_input_snapshot();
        self.terminal_dirty_since_tick = true;
        self.agent_dirty_since_detect = true;
        self.source_events_from_chunk(payload);
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        self.start_next_native_bootstrap();
    }

    /// Answer a bounded `SnapshotRequest` with replay bytes and their raw cut.
    fn reply_bounded_snapshot(&self, req: SnapshotRequest) {
        let byte_limit = req
            .max_frames
            .checked_sub(2)
            .map(|chunks| chunks.saturating_mul(req.chunk_bytes).min(req.max_bytes))
            .ok_or(crate::grid::SynthesisError::LimitExceeded);
        let snap = byte_limit.and_then(|max_bytes| {
            self.synthesize_with_scrollback_bounded(req.scrollback, max_bytes)
        });
        if let Err(err) = &snap {
            warn!(error = %err, "bounded snapshot synthesis failed");
        }
        let _ = req
            .reply
            .send(snap.map(|snapshot| (snapshot, self.core.seq())));
    }

    /// Install a client's reported default palette, then acknowledge.
    fn install_client_default_colors(&self, req: SetDefaultColorsRequest) {
        let result = match &mut *self.terminal.borrow_mut() {
            CanonicalTerminal::Plain(Some(terminal)) => {
                Self::install_default_colors(terminal, req.colors)
            }
            CanonicalTerminal::Plain(None) => Ok(()),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            CanonicalTerminal::Native(_) => {
                warn!("default palette update deferred while native cuts are active");
                Ok(())
            }
        };
        if let Err(err) = result {
            warn!(error = %err, "failed to install client default colors");
        }
        let _ = req.reply.send(());
    }

    /// Answer `GET_SCREEN`, falling back to an empty screen of the request's
    /// shape when projection fails.
    fn reply_screen_state(&self, req: ScreenRequest) {
        let want_cells = req.cells;
        let reply = match self.screen_state(req.pane, req.scrollback, req.cells, req.format) {
            Ok(screen) => ScreenReply::Projection(Box::new(screen)),
            // Budget refusal must reach the caller typed, not as an empty screen.
            Err(SynthesisError::RenderBudgetExceeded { required, budget }) => {
                ScreenReply::TooLarge {
                    required_bytes: required,
                    budget_bytes: budget,
                }
            }
            Err(err) => {
                warn!(error = %err, "screen projection failed; replying with empty");
                ScreenReply::Projection(Box::new(phux_core::screen::ScreenState {
                    schema_version: phux_core::screen::SCHEMA_VERSION,
                    pane: req.pane,
                    cols: self.cols,
                    rows: self.rows,
                    cursor: None,
                    lines: Vec::new(),
                    scrollback: Vec::new(),
                    // Honour the requested shape: empty cells, not `None`.
                    cells: want_cells.then(Vec::new),
                    ..phux_core::screen::ScreenState::default()
                }))
            }
        };
        let _ = req.reply.send(reply);
    }

    /// ADR-0032: hand the upgrade producer this pane's PTY descriptors and
    /// a full replay snapshot.
    fn reply_upgrade_handle(&self, req: UpgradeHandleRequest) {
        let snap = self
            .synthesize_with_scrollback(Some(0))
            .unwrap_or_else(|err| {
                warn!(error = %err, "upgrade snapshot synthesis failed; replying empty");
                SnapshotBytes {
                    cols: self.cols,
                    rows: self.rows,
                    bytes: Vec::new(),
                    scrollback: Vec::new(),
                }
            });
        let pty = self.pty.as_ref();
        let master_fd = pty.and_then(|p| {
            let master = p.master.lock().ok()?;
            let fd = master.as_raw_fd()?;
            // SAFETY: the master guard keeps `fd` open until `dup`
            // returns an independently owned descriptor.
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
            let duplicate = rustix::io::dup(borrowed).ok();
            drop(master);
            duplicate
        });
        let child_pid = pty
            .and_then(|p| p.child.process_id())
            .and_then(|id| i32::try_from(id).ok());
        let cwd = pty
            .and_then(|p| p.child.process_id())
            .and_then(crate::cwd_query::process_cwd)
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| {
                let last = self.last_known_cwd.borrow().clone();
                (!last.is_empty()).then_some(last)
            });
        let cell_px = (self.cell_px != (0, 0)).then_some(self.cell_px);
        let _ = req.reply.send(PaneUpgradeHandle {
            master_fd,
            child_pid,
            cols: self.cols,
            rows: self.rows,
            cell_px,
            osc_title: self.live_osc_title(),
            cwd,
            vt_replay_bytes: snap.bytes,
            scrollback_bytes: snap.scrollback,
        });
    }

    /// Answer with the PTY child's live cwd, or `None` (no PTY, no pid, or
    /// the query failed).
    fn reply_pane_cwd(&self, req: PwdRequest) {
        let cwd = self
            .pty
            .as_ref()
            .and_then(|p| p.child.process_id())
            .and_then(crate::cwd_query::process_cwd)
            .map(|p| p.to_string_lossy().into_owned());
        let _ = req.reply.send(cwd);
    }

    /// Apply one resize request, returning the resyncs to arm.
    ///
    /// A reflow owes everyone a resync; a `resync_only` gap request owes only
    /// the pump it names. No resync for the attach-time resize (the handshake
    /// snapshot covers it) or for an unchanged geometry (it would rotate the
    /// generation for nothing). A reflow without an everyone-resync still owes
    /// one to each native pump it tombstoned, or they stay frozen.
    pub(super) fn apply_resize_request(&mut self, req: ResizeRequest) -> Vec<OwedResync> {
        // `resync_only` carries no geometry.
        let reflowed = if req.resync_only {
            false
        } else {
            self.handle_resize(req.cols, req.rows, req.cell_px)
        };
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let tombstoned = std::mem::take(&mut self.reflow_tombstoned);
        #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
        let tombstoned: Vec<ResyncTarget> = Vec::new();
        if !req.resync_clients {
            return tombstoned
                .into_iter()
                .map(|target| OwedResync {
                    reason: ResyncReason::Resize,
                    target: Some(target),
                })
                .collect();
        }
        if !(req.resync_only || reflowed) {
            return Vec::new();
        }
        vec![if req.resync_only {
            OwedResync {
                reason: ResyncReason::OutboundGap,
                target: req.resync_for,
            }
        } else {
            OwedResync {
                reason: ResyncReason::Resize,
                target: None,
            }
        }]
    }

    /// One tick of the state-sync emitter (ADR-0018).
    ///
    /// For each tick-managed consumer: diff against its own reference (not
    /// libghostty's shared dirty bits), skip an empty body, else stamp the
    /// next `seq` and ship. Errors skip that consumer only. The reference
    /// advances on emit, so each change ships once.
    pub(super) fn tick_emit(&mut self) {
        // Debug-level span (free when disabled) carrying consumer count and
        // dirtiness; `emitted`/`total_out_bytes` are recorded at the end.
        let tick_span = tracing::debug_span!(
            "tick_emit",
            consumer_count = self.consumer_states.len(),
            dirty = self.terminal_dirty_since_tick,
            emitted = tracing::field::Empty,
            total_out_bytes = tracing::field::Empty,
        )
        .entered();

        let Some(force_all_consumers) = self.tick_emit_audience() else {
            return;
        };
        let Some(mutated) = self.take_tick_mutation() else {
            return;
        };
        let Some(walk) = self.emit_ready_tick(force_all_consumers, mutated) else {
            return;
        };
        tick_span.record("emitted", walk.emitted);
        tick_span.record("total_out_bytes", walk.total_out_bytes);
        crate::perf::TICK_EMIT.record_elapsed(walk.started);
        for client_id in walk.closed {
            self.consumer_states.remove(&client_id);
        }
    }

    /// Whether this tick has anyone to serve, plus the test gate that forces
    /// every consumer onto the tick. Only `StateSync` consumers are
    /// tick-managed; raw consumers get the broadcast pump.
    fn tick_emit_audience(&self) -> Option<bool> {
        let force_all_consumers = self.consumer_tick_emits;
        if !force_all_consumers && !self.consumer_states.values().any(|s| s.wants_state_sync) {
            // No tick-managed consumer: nothing to emit (dirty flag untouched).
            return None;
        }
        Some(force_all_consumers)
    }

    /// Take the "mutated since last tick" flag. On a clean terminal the walk
    /// is skipped unless some consumer still needs one.
    fn take_tick_mutation(&mut self) -> Option<bool> {
        let mutated = self.terminal_dirty_since_tick;
        self.terminal_dirty_since_tick = false;
        if !mutated && !self.consumer_states.values().any(must_walk_when_clean) {
            return None;
        }
        Some(mutated)
    }

    /// Render once and walk every consumer. `None` when `prepare_tick`
    /// fails. Timed here so cheap skipped ticks do not swamp the histogram.
    fn emit_ready_tick(
        &mut self,
        force_all_consumers: bool,
        mutated: bool,
    ) -> Option<TickEmitWalk> {
        let started = std::time::Instant::now();

        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            // Terminal on loan to a capture; the resync after repaints.
            trace!("tick skipped: canonical terminal is on loan to a capture");
            return None;
        };
        let mut synth = self.synth.borrow_mut();
        // Render once; each consumer only diffs against the result.
        let render = match synth.prepare_tick(terminal) {
            Ok((cols, rows, live_cm)) => TickRender {
                cols,
                rows,
                live_cm,
            },
            Err(err) => {
                warn!(error = %err, "state-sync tick: prepare_tick failed; skipping tick");
                return None;
            }
        };
        // Consumers with a closed mailbox are reaped after the loop.
        let mut closed: Vec<ClientId> = Vec::new();
        let mut emitted: u64 = 0;
        let mut total_out_bytes: usize = 0;
        for (client_id, state) in &mut self.consumer_states {
            match Self::emit_consumer_tick(
                *client_id,
                state,
                &synth,
                render,
                mutated,
                force_all_consumers,
            ) {
                TickOutcome::Skipped => {}
                TickOutcome::Closed => closed.push(*client_id),
                TickOutcome::Emitted(out_bytes) => {
                    emitted += 1;
                    total_out_bytes += out_bytes;
                }
            }
        }
        drop(synth);
        drop(canonical);
        Some(TickEmitWalk {
            emitted,
            total_out_bytes,
            closed,
            started,
        })
    }

    /// Serve one consumer: reserve its outbound slot, diff, ship.
    fn emit_consumer_tick(
        client_id: ClientId,
        state: &mut ConsumerSyncState,
        synth: &SnapshotSynthesizer<'_>,
        render: TickRender,
        mutated: bool,
        force_all_consumers: bool,
    ) -> TickOutcome {
        // Raw consumers are served by the broadcast pump.
        if !force_all_consumers && !state.wants_state_sync {
            return TickOutcome::Skipped;
        }
        if !*state.live_gate.borrow() {
            return TickOutcome::Skipped;
        }
        // Captured before the reset: a delta held back by backpressure.
        let was_behind = state.behind;
        state.needs_initial_emit = false;
        // Reserve before diffing: the diff advances the reference, so a
        // delta that then failed to send would be lost for good. A full
        // mailbox skips this consumer untouched; a closed one is reaped.
        let permit = match state.outbound.try_reserve() {
            Ok(permit) => permit,
            Err(tokio::sync::mpsc::error::TrySendError::Full(())) => {
                // Retry next tick without advancing; `behind` keeps the walk
                // alive even if the grid goes clean meanwhile.
                state.behind = true;
                crate::perf::CONSUMER_MAILBOX_FULL.incr();
                if let Some(suppressed) = crate::perf::MAILBOX_FULL_WARN.admit() {
                    warn!(
                        ?client_id,
                        wire_terminal_id = state.wire_terminal_id,
                        suppressed,
                        "state-sync tick: consumer mailbox full; skipping (reference held, retries next tick)",
                    );
                }
                return TickOutcome::Skipped;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(())) => {
                // The detach may have been dropped; reap now.
                crate::perf::CONSUMER_REAPED.incr();
                debug!(
                    ?client_id,
                    wire_terminal_id = state.wire_terminal_id,
                    "state-sync tick: consumer mailbox closed; reaping entry",
                );
                return TickOutcome::Closed;
            }
        };
        // Holding a permit: this consumer is served this tick.
        state.behind = false;
        let _synth_span = tracing::debug_span!(
            "synthesize",
            ?client_id,
            wire_terminal_id = state.wire_terminal_id,
        )
        .entered();
        let synth_started = std::time::Instant::now();
        let bytes = consumer_delta(
            synth,
            render,
            state.loss_tolerant,
            &state.acked_reference,
            &mut state.reference,
        );
        crate::perf::TICK_SYNTH.record_elapsed(synth_started);
        if bytes.is_empty() {
            // Unchanged; the permit drops unused.
            return TickOutcome::Skipped;
        }
        if state.loss_tolerant && holds_loss_tolerant_delta(state, mutated, was_behind) {
            return TickOutcome::Skipped;
        }
        let seq = state.next_seq;
        let out_bytes = bytes.len();
        crate::perf::TICK_OUT_BYTES.record_len(out_bytes);
        state.next_seq = state.next_seq.wrapping_add(1);
        let frame = FrameKind::ResourceOutput {
            terminal_id: phux_protocol::ids::ResourceId::local(state.wire_terminal_id),
            stream_id: state.stream_id,
            bootstrap_id: state.bootstrap_id,
            seq,
            bytes: bytes.into(),
        };
        // Infallible with a reserved permit.
        permit.send(Outbound::Frame(frame));
        record_emit_instant(&mut state.emit_instants, seq);
        // Loss-tolerant: remember the grid this `seq` shipped so a later
        // ack can advance the acked reference to it (bounded).
        if state.loss_tolerant {
            let snapshot = synth.snapshot_tick_reference(render.cols, render.rows, render.live_cm);
            record_pending_ref(&mut state.pending_refs, seq, snapshot);
        }
        trace!(
            ?client_id,
            wire_terminal_id = state.wire_terminal_id,
            seq,
            out_bytes,
            "state-sync tick: RESOURCE_OUTPUT emitted",
        );
        TickOutcome::Emitted(out_bytes)
    }
}

/// One consumer's delta against the just-rendered tick. Loss-tolerant
/// consumers diff against their last-acked reference (not advanced on emit),
/// so a dropped frame self-heals; others use emit-once `diff_consumer`.
fn consumer_delta(
    synth: &SnapshotSynthesizer<'_>,
    render: TickRender,
    loss_tolerant: bool,
    acked_reference: &crate::grid::ConsumerReference,
    reference: &mut crate::grid::ConsumerReference,
) -> Vec<u8> {
    if loss_tolerant {
        synth.diff_against_base(render.cols, render.rows, render.live_cm, acked_reference)
    } else {
        synth
            .diff_consumer(render.cols, render.rows, render.live_cm, reference)
            .bytes
    }
}

/// Whether a consumer must be walked on a clean terminal: it has never been
/// served (`needs_initial_emit`), a backpressured delta is owed (`behind`),
/// or a loss-tolerant frame is still un-acked.
fn must_walk_when_clean(state: &ConsumerSyncState) -> bool {
    state.needs_initial_emit || state.behind || !state.pending_refs.is_empty()
}

/// Loss-tolerant emit gate: the diff stays non-empty while a frame is
/// un-acked, so ship only on new content, a post-backpressure flush, or a
/// due retransmit.
fn holds_loss_tolerant_delta(state: &ConsumerSyncState, mutated: bool, was_behind: bool) -> bool {
    let now = tokio::time::Instant::now();
    let retransmit_due = !state.pending_refs.is_empty()
        && state.emit_instants.values().next_back().is_none_or(|last| {
            now.saturating_duration_since(*last) >= tick::retransmit_timeout(state.rtt.smoothed())
        });
    !mutated && !was_behind && !retransmit_due
}

/// Record the emit instant for `seq`, for RTT on its `FRAME_ACK`. Oldest
/// entries are evicted past [`MAX_EMIT_INSTANTS`] so a consumer that never
/// acks cannot grow the map.
fn record_emit_instant(
    emit_instants: &mut std::collections::BTreeMap<u64, tokio::time::Instant>,
    seq: u64,
) {
    emit_instants.insert(seq, tokio::time::Instant::now());
    while emit_instants.len() > MAX_EMIT_INSTANTS {
        emit_instants.pop_first();
    }
}

/// Retain the grid a loss-tolerant `seq` shipped, bounded like emit instants.
fn record_pending_ref(
    pending_refs: &mut std::collections::BTreeMap<u64, crate::grid::ConsumerReference>,
    seq: u64,
    snapshot: crate::grid::ConsumerReference,
) {
    pending_refs.insert(seq, snapshot);
    while pending_refs.len() > MAX_EMIT_INSTANTS {
        pending_refs.pop_first();
    }
}

/// Join one burst into a single payload. A lone chunk moves through as-is;
/// a burst (every burst on macOS, where reads cap at 1 KiB) is copied once
/// into a buffer sized to exactly `total` bytes, so there is no regrowth and
/// the broadcast ring retains no spare capacity.
fn join_pty_burst(first: Bytes, rest: &[Bytes], total: usize) -> Bytes {
    if rest.is_empty() {
        return first;
    }
    let mut joined = Vec::with_capacity(total);
    joined.extend_from_slice(&first);
    for chunk in rest {
        joined.extend_from_slice(chunk);
    }
    debug_assert_eq!(joined.len(), total, "burst total was exact");
    Bytes::from(joined)
}

/// A `MissedTickBehavior::Delay` interval with its immediate first tick
/// consumed. `Delay` spaces late ticks instead of bursting to catch up.
async fn armed_interval(period: std::time::Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let _ = interval.tick().await;
    interval
}

#[cfg(test)]
mod tick_rearm_tests {
    use super::{DEFAULT_TICK_INTERVAL, armed_interval};

    /// Re-arming a long-disarmed tick owes exactly one tick (`Delay`), not
    /// every missed period.
    #[tokio::test(start_paused = true)]
    async fn disarming_the_tick_does_not_bank_a_stampede_of_catch_up_ticks() {
        let mut tick = armed_interval(DEFAULT_TICK_INTERVAL).await;

        // An hour with the arm's precondition false: nothing polls the timer.
        tokio::time::advance(std::time::Duration::from_secs(3600)).await;

        // Count ticks ready without the paused clock advancing.
        let mut immediate = 0_u32;
        while tokio::time::timeout(std::time::Duration::ZERO, tick.tick())
            .await
            .is_ok()
        {
            immediate += 1;
            // Bail out rather than counting to 120,000 on a regression.
            if immediate > 8 {
                break;
            }
        }

        assert_eq!(
            immediate, 1,
            "re-arming a long-disarmed tick must yield one catch-up tick, not one per \
             missed period",
        );
    }
}

#[cfg(test)]
mod pty_burst_tests {
    use super::{Bytes, join_pty_burst};

    /// A lone chunk is forwarded without a copy; a burst joins in order into
    /// one buffer.
    #[test]
    fn a_burst_joins_in_order_and_a_lone_chunk_is_not_copied() {
        let first = Bytes::from_static(b"abc");
        let lone = join_pty_burst(first.clone(), &[], first.len());
        assert_eq!(lone.as_ptr(), first.as_ptr(), "a lone chunk moves through");

        let rest = [Bytes::from_static(b"de"), Bytes::from_static(b"fghi")];
        let joined = join_pty_burst(first, &rest, 9);
        assert_eq!(&joined[..], b"abcdefghi");
    }
}

#[cfg(test)]
mod resync_debounce_tests {
    use std::time::Duration;

    use super::{
        OwedResync, RESIZE_RESYNC_DEBOUNCE, ResyncAudience, ResyncDebounce, ResyncReason,
        ResyncTarget,
    };

    fn idle() -> ResyncDebounce {
        ResyncDebounce::idle()
    }

    fn pump(owner: u64) -> ResyncTarget {
        ResyncTarget {
            owner,
            stream_id: phux_protocol::ids::StreamId::new(1).expect("non-zero stream id"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("non-zero bootstrap id"),
        }
    }

    /// A gap resync owed to one pump.
    const fn gap_for(target: ResyncTarget) -> OwedResync {
        OwedResync {
            reason: ResyncReason::OutboundGap,
            target: Some(target),
        }
    }

    /// A gap resync that named no pump: owed to everyone.
    const fn gap() -> OwedResync {
        OwedResync {
            reason: ResyncReason::OutboundGap,
            target: None,
        }
    }

    const fn resize() -> OwedResync {
        OwedResync {
            reason: ResyncReason::Resize,
            target: None,
        }
    }

    /// Many lagged pumps retrying must not keep pushing the debounce out:
    /// the deadline is fixed by the first gap request.
    #[tokio::test(start_paused = true)]
    async fn a_pending_gap_resync_is_not_pushed_out_by_more_lagged_consumers() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut debounce = idle();

        debounce.arm(gap_for(pump(0)), sleep.as_mut());
        let first_deadline = sleep.deadline();
        assert!(debounce.pending);

        for round in 0..20_u64 {
            // Faster than the debounce, which is exactly the starving case.
            tokio::time::advance(RESIZE_RESYNC_DEBOUNCE / 4).await;
            debounce.arm(gap_for(pump(round % 10)), sleep.as_mut());
            assert_eq!(
                sleep.deadline(),
                first_deadline,
                "a gap resync already owed must coalesce onto the pending deadline, \
                 not restart it",
            );
        }

        // And it really does come due: the deadline is in the past by now.
        assert!(
            sleep.deadline() <= tokio::time::Instant::now(),
            "the coalesced snapshot must have become due despite the request storm",
        );
        let (reason, audience) = debounce.take();
        assert_eq!(reason, ResyncReason::OutboundGap);
        let expected: Vec<_> = (0..10).map(pump).collect();
        assert_eq!(
            audience,
            ResyncAudience::Only(expected.into()),
            "every lagged pump rides the one snapshot, each named once",
        );
    }

    /// EOF folds queued gap requests in without moving the deadline.
    #[tokio::test(start_paused = true)]
    async fn eof_flush_includes_queued_gap_requests_without_rearming_the_deadline() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut debounce = idle();
        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        let first_deadline = sleep.deadline();
        debounce.include(gap_for(pump(2)));
        assert_eq!(
            sleep.deadline(),
            first_deadline,
            "including a queued gap request at EOF must not wait another debounce",
        );
        let (reason, audience) = debounce.take();
        assert_eq!(reason, ResyncReason::OutboundGap);
        assert_eq!(
            audience,
            ResyncAudience::Only(vec![pump(1), pump(2)].into())
        );
    }

    /// A gap resync is owed only to the pumps that asked, and the next
    /// window starts empty.
    #[tokio::test(start_paused = true)]
    async fn a_gap_resync_is_addressed_only_to_the_pumps_that_asked() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut debounce = idle();

        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        debounce.arm(gap_for(pump(2)), sleep.as_mut());
        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        let (reason, audience) = debounce.take();
        assert_eq!(reason, ResyncReason::OutboundGap);
        assert_eq!(
            audience,
            ResyncAudience::Only(vec![pump(1), pump(2)].into())
        );
        assert!(!audience.includes(pump(3)), "a fresh pump is not addressed");

        debounce.arm(gap_for(pump(3)), sleep.as_mut());
        assert_eq!(
            debounce.take().1,
            ResyncAudience::Only(vec![pump(3)].into())
        );
    }

    /// A resize in the same window widens a gap resync to everyone, in either
    /// order; an untargeted gap request is everyone.
    #[tokio::test(start_paused = true)]
    async fn a_resize_or_an_unnamed_gap_widens_the_audience_to_everyone() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);

        let mut debounce = idle();
        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        debounce.arm(resize(), sleep.as_mut());
        assert_eq!(
            debounce.take(),
            (ResyncReason::Resize, ResyncAudience::Everyone)
        );

        let mut debounce = idle();
        debounce.arm(resize(), sleep.as_mut());
        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        assert_eq!(
            debounce.take(),
            (ResyncReason::Resize, ResyncAudience::Everyone)
        );

        let mut debounce = idle();
        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        debounce.arm(gap(), sleep.as_mut());
        assert_eq!(
            debounce.take(),
            (ResyncReason::OutboundGap, ResyncAudience::Everyone)
        );
    }

    /// A resize storm still re-arms every time.
    #[tokio::test(start_paused = true)]
    async fn a_resize_still_restarts_the_debounce() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut debounce = idle();

        debounce.arm(resize(), sleep.as_mut());
        let first_deadline = sleep.deadline();
        tokio::time::advance(RESIZE_RESYNC_DEBOUNCE / 4).await;
        debounce.arm(resize(), sleep.as_mut());
        assert!(
            sleep.deadline() > first_deadline,
            "a drag storm must settle on the last size",
        );
    }

    /// A resize arriving while a gap resync is owed re-arms with its reason.
    #[tokio::test(start_paused = true)]
    async fn a_resize_supersedes_a_pending_gap_resync() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut debounce = idle();

        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        let gap_deadline = sleep.deadline();
        tokio::time::advance(RESIZE_RESYNC_DEBOUNCE / 4).await;
        debounce.arm(resize(), sleep.as_mut());

        assert!(sleep.deadline() > gap_deadline);
        assert_eq!(debounce.take().0, ResyncReason::Resize);
    }
}
