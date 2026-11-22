use super::{
    BootstrapPump, DEFAULT_TICK_INTERVAL, ResyncDebounce, RunLoopState, TerminalActor,
    armed_interval, debug, recv_native_or_pty, service,
};

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
    pub(super) async fn arm_run_loop(&mut self) -> RunLoopState {
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
    pub(super) async fn drive_run_loop(
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
                Some(req) = self.control_rx.recv() => self.handle_control_request(req),

                // The agent running here: bindings, evidence, invalidation.
                Some(req) = self.agent_parent_rx.recv() => self.handle_agent_parent_request(req),

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
}
