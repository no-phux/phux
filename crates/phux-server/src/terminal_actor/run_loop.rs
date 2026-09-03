//! The [`TerminalActor`] event loop (`run`) and the state-sync tick
//! emitter (`tick_emit`).

use super::{
    AgentDetector, Bytes, CanonicalTerminal, ClientId, ConsumerAckRequest, ConsumerDetachRequest,
    ConsumerSyncState, DEFAULT_TICK_INTERVAL, EncodedInputRequest, FrameKind, MAX_EMIT_INSTANTS,
    MAX_INPUT_COALESCE, MAX_PTY_COALESCE, MAX_PTY_COALESCE_BYTES, NativeOrPty, Outbound,
    PaneOutput, PaneUpgradeHandle, PtyEvent, PwdRequest, RESIZE_RESYNC_DEBOUNCE, ResizeRequest,
    ResyncReason, ScreenRequest, SetDefaultColorsRequest, SnapshotBytes, SnapshotRequest,
    TerminalActor, TerminalInput, UpgradeHandleRequest, debug, error, mpsc, recv_native_or_pty,
    tick, trace, warn,
};
use crate::grid::SnapshotSynthesizer;
use crate::grid::reference::ReferenceCursorMode;

/// What the `run` loop must do after one PTY-ingress turn.
enum PtyTurn {
    /// Keep looping with `native_step_due` untouched (the EOF path).
    Continue,
    /// Keep looping with a freshly recomputed `native_step_due`.
    Stepped(bool),
    /// The actor-global raw output sequence is exhausted; the PTY has already
    /// been torn down and the loop must return.
    Shutdown,
}

/// One bounded PTY read burst: the coalesced payload plus why the drain
/// stopped.
struct PtyBurst {
    /// The chunks to write to the `Terminal` and broadcast as one frame.
    payload: Bytes,
    /// Reader chunks folded into `payload` (`pty.burst.chunks`).
    chunks: u64,
    /// A queued EOF was observed while draining; handle it after the flush.
    saw_eof: bool,
    /// `true` when the drain stopped because the next chunk would cross the
    /// byte cap (more output is likely queued) rather than because the queue
    /// emptied. Drives the post-broadcast yield so a sustained burst hands the
    /// scheduler a turn between bounded parses.
    hit_byte_cap: bool,
}

/// The consumer-independent render products of one state-sync tick, as
/// returned by [`SnapshotSynthesizer::prepare_tick`].
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
    /// Nothing shipped: not tick-managed, gated off, backpressured, or
    /// byte-identical to this consumer's reference.
    Skipped,
    /// The consumer's outbound mailbox is closed; reap the entry.
    Closed,
    /// A `TerminalOutput` frame shipped, carrying this many payload bytes.
    Emitted(usize),
}

/// The cooperative native-bootstrap pump's position for one `run` turn.
///
/// While a native bootstrap is in flight the loop alternates a yield to the
/// runtime with exactly one record step, so prefix capture advances between
/// ingress turns without starving sibling `LocalSet` tasks. The two pump
/// arms are the two halves of that alternation; this enum names which half
/// (if either) the current turn owes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BootstrapPump {
    /// No bootstrap in flight: both pump arms stay disabled.
    Idle,
    /// This turn owes the in-flight bootstrap one record step.
    StepDue,
    /// This turn owes the runtime a yield before the next record step.
    YieldDue,
}

impl BootstrapPump {
    /// Resolve the pump from the actor's bootstrap state plus whether the
    /// previous turn left a step owed.
    const fn resolve(bootstrap_pending: bool, step_owed: bool) -> Self {
        if !bootstrap_pending {
            return Self::Idle;
        }
        if step_owed {
            Self::StepDue
        } else {
            Self::YieldDue
        }
    }
}

/// phux-8v1 drag fix: the debounced post-resize client resync owned by the
/// `run` loop. (Re)armed on each resync-requesting resize; when the timer
/// fires we broadcast ONE snapshot at the settled size.
struct ResyncDebounce {
    /// A resync is owed once the debounce deadline lands. False until a
    /// resize arms it, which is why the idle far-future deadline the loop
    /// starts with is never observed.
    pending: bool,
    /// Why the owed resync was armed; broadcast when the deadline fires.
    reason: ResyncReason,
}

impl ResyncDebounce {
    /// Whether the settled-resize snapshot may fire this turn: one is owed,
    /// and no native bootstrap is holding the loop's broadcast arms closed.
    const fn may_fire(&self, bootstrap_pending: bool) -> bool {
        self.pending && !bootstrap_pending
    }

    /// Owe a resync for `reason` and (re)start the debounce, so a drag storm
    /// coalesces into a single snapshot rather than flooding the client.
    ///
    /// A gap resync that is *already* owed deliberately does not push the
    /// deadline out again. The two callers want opposite things: a resize
    /// storm wants the last size, so every resize must re-arm, but every
    /// fenced pump on a pane asks for the same single snapshot, and each of
    /// those requests resetting the timer is a livelock. N pumps retrying
    /// independently arrive at a mean interval of `retry / N`, which at around
    /// ten consumers on one pane beats the 50 ms debounce every time: the
    /// snapshot they are all waiting for would never fire, and none of them
    /// would ever unfence. Coalescing onto the first deadline is what makes
    /// the fleet converge instead of starve.
    fn arm(&mut self, reason: ResyncReason, deadline: std::pin::Pin<&mut tokio::time::Sleep>) {
        if self.pending && reason == ResyncReason::OutboundGap {
            return;
        }
        self.pending = true;
        self.reason = reason;
        deadline.reset(tokio::time::Instant::now() + RESIZE_RESYNC_DEBOUNCE);
    }

    /// Clear the owed resync and hand back the reason to broadcast it with.
    const fn take_reason(&mut self) -> ResyncReason {
        self.pending = false;
        self.reason
    }
}

impl TerminalActor {
    /// Run the actor's event loop until shutdown.
    ///
    /// Native prefix capture advances by one record between ingress turns.
    ///
    /// Every arm is a one-line dispatch into a named handler; what remains
    /// inline is the `select!`'s own arm/guard scaffolding plus the
    /// rationale comments that must sit next to the guard they explain.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    #[allow(
        clippy::cognitive_complexity,
        reason = "select! macro expansion inflates the score; every arm body is a single handler call"
    )]
    pub async fn run(mut self) {
        debug!(
            cols = self.cols,
            rows = self.rows,
            has_pty = self.pty.is_some(),
            "TerminalActor started",
        );

        // State-sync tick driver (phux-q0e.3 / phux-q0e.5). RTT-adaptive
        // cadence: starts at the `DEFAULT_TICK_INTERVAL` cold-start value and
        // is rebuilt toward each consumer's measured `RTT/2` (clamped to
        // [`MIN_TICK_INTERVAL`, `MAX_TICK_INTERVAL`]) as `FRAME_ACK`
        // round-trips land. The shared timer runs at the minimum desired
        // interval across consumers (see [`Self::adaptive_tick_interval`]).
        // The timer's missed-tick behavior and the eaten first tick are
        // [`armed_interval`]'s; the rationale for both lives there.
        let mut tick_interval = DEFAULT_TICK_INTERVAL;
        let mut tick = armed_interval(tick_interval).await;

        self.install_agent_detector();
        let mut detect_interval = crate::agent_detect::TICK_UNIDENTIFIED;
        let mut detect_tick = armed_interval(detect_interval).await;

        // Init the debounce deadline far out — `resync.pending` is false
        // until a resize arms it, and arming always resets the deadline, so
        // the initial instant is never observed.
        let resync_deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
        tokio::pin!(resync_deadline);
        let mut resync = ResyncDebounce {
            pending: false,
            reason: ResyncReason::Resize,
        };
        // Native control and PTY output are one outer select arm so the actor
        // never borrows either receiver twice. Preference swaps after every
        // selected ingress, but both sources remain enabled: a silent PTY can
        // never park bootstrap or consecutive history requests.
        let mut prefer_native = false;
        let mut native_step_due = false;

        loop {
            // Resolved once per turn. `select!` evaluates every precondition
            // below in one pass as it is entered, and nothing runs between
            // here and there, so one read stands in for the ~ten separate
            // reads the guards used to make.
            let bootstrap_pending = self.native_bootstrap_pending();
            let pump = BootstrapPump::resolve(bootstrap_pending, native_step_due);

            tokio::select! {
                biased;

                () = self.token.cancelled() => {
                    debug!("TerminalActor cancellation token fired");
                    self.shutdown_pty().await;
                    return;
                }

                // Bytes already encoded on the dedicated input lane. This
                // bounded mailbox is the production input path and shares the
                // actor's highest scheduling priority.
                Some(request) = self.encoded_input_rx.recv() =>
                    self.service_encoded_input_batch(request),

                // Legacy inline input → PTY, retained for direct-drive tests
                // that intentionally construct the runtime without a lane.
                // Polled before the PTY-output arm (biased
                // order) so a queued keystroke is serviced this turn
                // rather than waiting behind an output burst — the fix for
                // load-correlated input starvation. Bounded by
                // `MAX_INPUT_COALESCE`: the arm fires on the first ready
                // event, then drains up to a capped batch via `try_recv`
                // so a paste the encoder expands cannot inflate one turn
                // without limit. The PTY-output arm's structural bound is
                // `MAX_PTY_COALESCE_BYTES`.
                Some(input) = self.input_rx.recv() => self.service_input_batch(&input),

                () = std::future::ready(()), if pump == BootstrapPump::StepDue => {
                    self.cooperative_native_step();
                    native_step_due = false;
                }

                ingress = recv_native_or_pty(
                    &mut self.native_requests,
                    self.pty_rx.as_mut(),
                    prefer_native,
                ) => {
                    if self
                        .service_ingress_turn(ingress, &mut prefer_native, &mut native_step_due)
                        .await
                        .is_break()
                    {
                        return;
                    }
                }

                Some(req) = self.snapshot_rx.recv(), if !bootstrap_pending =>
                    self.reply_bounded_snapshot(req),

                Some(req) = self.set_default_colors_rx.recv(), if !bootstrap_pending =>
                    self.install_client_default_colors(req),

                Some(req) = self.screen_rx.recv(), if !bootstrap_pending =>
                    self.reply_screen_state(req),

                Some(req) = self.upgrade_rx.recv(), if !bootstrap_pending =>
                    self.reply_upgrade_handle(req),

                Some(req) = self.pwd_rx.recv() => self.reply_pane_cwd(req),

                Some(req) = self.resize_rx.recv(), if !bootstrap_pending =>
                    self.service_resize_request(req, &mut resync, resync_deadline.as_mut()),

                // phux-8v1: debounced resize resync — fires once the
                // resize storm settles (RESIZE_RESYNC_DEBOUNCE after the
                // last resync-requesting resize). Guarded by the owed-resync
                // flag so the idle far-future timer never fires spuriously.
                () = &mut resync_deadline, if resync.may_fire(bootstrap_pending) =>
                    self.broadcast_resync(resync.take_reason()),

                Some(req) = self.consumer_attach_rx.recv(), if !bootstrap_pending =>
                    self.handle_consumer_attach(req),

                Some(req) = self.consumer_detach_rx.recv() =>
                    self.service_consumer_detach(req, &mut tick, &mut tick_interval),

                // ADR-0018 / phux-q0e.4: inbound FRAME_ACK. Clears the
                // per-consumer dirty cache so the next tick re-diffs
                // against the just-acked reference. Loss tolerance: a
                // dropped ack just means the next tick re-emits a larger
                // diff against the same older reference — no
                // retransmit machinery here.
                Some(req) = self.consumer_ack_rx.recv() =>
                    self.service_frame_ack(&req, &mut tick, &mut tick_interval),

                // Semantic event subscription request. Register the subscriber
                // and begin broadcasting matching events to their outbound mailbox.
                Some(req) = self.subscribe_to_events_rx.recv() => self.subscribe_to_events(req),

                // Semantic event unsubscription request. Remove the subscriber
                // from the broadcast list. Silent no-op if already unsubscribed.
                Some(req) = self.unsubscribe_from_events_rx.recv() =>
                    self.unsubscribe_from_events(&req),

                // Supervisory control (ADR-0033): lease-change broadcasts and
                // process signals. The lease itself lives in `ServerState`; the
                // actor is the emitter (it owns the subscriber list + lifecycle)
                // and the signal deliverer (it owns the PTY child pid).
                Some(req) = self.control_rx.recv() => self.handle_control_request(req),

                // Disarmed while there is nothing for a tick to do (see
                // `state_tick_armed`). A `select!` arm whose precondition is
                // false is never polled, so a disarmed tick registers no
                // timer and produces no wakeup at all — an idle pane with no
                // state-sync consumer costs zero, where it used to wake the
                // whole actor 33 times a second to discover that. The guard
                // is re-evaluated every loop turn, and every event that can
                // make the tick relevant (a consumer attaching, a PTY chunk
                // opening an output burst, a native cursor binding) is itself
                // a loop turn, so re-arming is immediate.
                _ = tick.tick(), if !bootstrap_pending && self.state_tick_armed() =>
                    self.service_state_tick(),

                // Agent-state detector (ADR-0046). This interval is the SOLE
                // driver: PTY bytes deliberately do NOT wake it. A chatty
                // agent spewing megabytes must cost zero extra detector work
                // — the whole design is a periodic re-derivation, not a
                // reaction to output. The cadence is adaptive (500 ms while
                // unidentified, 300 ms once identified, 100 ms while
                // confirming a working -> idle transition) and is re-armed
                // through the existing `rearm_tick`, whose deadband keeps a
                // steady cadence from churning the scheduler.
                _ = detect_tick.tick(), if self.detector_tick_armed(bootstrap_pending) =>
                    self.service_detect_tick(&mut detect_tick, &mut detect_interval),

                () = tokio::task::yield_now(), if pump == BootstrapPump::YieldDue => {
                    native_step_due = true;
                }

                else => break,
            }
        }
    }

    /// Service one combined native-control / PTY-output ingress turn.
    ///
    /// Owns the source-preference swap and the cooperative-step bookkeeping
    /// that follow each selected ingress. Returns `ControlFlow::Break` when
    /// the actor-global raw output sequence is exhausted: the PTY has already
    /// been torn down and the loop must return.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    async fn service_ingress_turn(
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
                // PTY -> Terminal + broadcast. One bounded parse
                // returns to this combined ingress arm so native
                // control and live output alternate when both are
                // continuously ready.
                match self.service_pty_event(evt).await {
                    PtyTurn::Continue => {}
                    PtyTurn::Stepped(due) => *native_step_due = due,
                    PtyTurn::Shutdown => return std::ops::ControlFlow::Break(()),
                }
            }
        }
        std::ops::ControlFlow::Continue(())
    }

    /// Apply one resize request and arm the debounced resync it earns.
    ///
    /// Arming the debounce timer is this caller's half of the resync
    /// decision; `apply_resize_request` owns the reflow and the "does this
    /// deserve a resync" rules.
    fn service_resize_request(
        &mut self,
        req: ResizeRequest,
        resync: &mut ResyncDebounce,
        deadline: std::pin::Pin<&mut tokio::time::Sleep>,
    ) {
        if let Some(reason) = self.apply_resize_request(req) {
            resync.arm(reason, deadline);
        }
    }

    /// Reap a detached consumer's per-consumer state and re-evaluate the
    /// shared tick cadence.
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
        // phux-q0e.5: losing a consumer can raise the minimum
        // desired interval (e.g. the fastest peer left), so
        // re-evaluate the shared cadence.
        Self::rearm_tick(tick, tick_interval, self.adaptive_tick_interval());
        let _ = reply.send(());
    }

    /// Fold one inbound `FRAME_ACK` into its consumer's state.
    ///
    /// phux-q0e.5: a fresh RTT sample may shift the adaptive cadence. Rebuild
    /// the shared tick only when the new minimum-desired interval moves
    /// beyond the deadband, so a steady RTT does not churn the scheduler.
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

    /// One state-sync tick (phux-q0e.3, phux-ia4, ADR-0018): iterate each
    /// attached consumer, diff the live terminal against that consumer's own
    /// reference grid, and push a `TerminalOutput` frame onto its outbound
    /// mailbox whenever `synthesize_against_reference` returns non-empty
    /// bytes.
    pub(super) fn service_state_tick(&mut self) {
        // phux-y2t: close an output burst with an `idle` event
        // when no PTY output arrived since the previous tick.
        // This bookkeeping is independent of the state-sync
        // emitter gate, so headless watchers settle raw panes too.
        self.maybe_emit_idle();
        self.tick_emit();
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        self.expire_native_cursors();
    }

    /// Whether the state-sync tick arm has any work to do this turn.
    ///
    /// [`Self::service_state_tick`] does exactly three things, and all three
    /// are conditional:
    ///
    /// * `maybe_emit_idle` closes an output burst — only relevant while one
    ///   is open (`in_output_burst`).
    /// * `tick_emit` returns immediately unless some consumer is
    ///   tick-managed, which is the same gate spelled out here.
    /// * `expire_native_cursors` expires history bindings — only relevant
    ///   while at least one exists.
    ///
    /// With none of those true the tick was a pure wakeup: a timer fire, a
    /// `debug_span!` construction, and two early returns, repeated 33 times a
    /// second for every pane on the server whether or not anyone was
    /// attached. Naming the precondition lets the `select!` skip arming the
    /// timer entirely.
    ///
    /// **This is only safe because [`armed_interval`] sets
    /// `MissedTickBehavior::Delay`.** A disarmed arm is not polled, so its
    /// `Interval` accumulates missed periods for as long as the pane stays
    /// quiet. Under tokio's default `Burst` those would all be owed on
    /// re-arm: an hour of silence at the 30 ms cold-start cadence is ~120,000
    /// back-to-back `service_state_tick` calls, on the shared current-thread
    /// runtime, the instant someone attaches. `Delay` discards them and
    /// yields exactly one catch-up tick, which is what makes disarming a
    /// saving rather than a deferred stampede. `disarming_the_tick_does_not_\
    /// bank_a_stampede_of_catch_up_ticks` pins that.
    pub(super) fn state_tick_armed(&self) -> bool {
        if self.in_output_burst {
            return true;
        }
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if !self.native_cursor_owners.is_empty() {
            return true;
        }
        self.consumer_tick_emits || self.consumer_states.values().any(|s| s.wants_state_sync)
    }

    /// Whether the agent-state detector arm may run this turn: a detector was
    /// installed (see [`Self::install_agent_detector`]) and no native
    /// bootstrap is holding the loop's non-ingress arms closed.
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

    /// Agent-state detector (ADR-0046). Constructed HERE, not in `build`,
    /// for two reasons: `started` then anchors the startup grace window at
    /// the moment the child actually begins painting, and no existing
    /// constructor or test actor grows a detector it never asked for. Only
    /// a PTY-backed actor with a wired sink and a non-empty rule set gets
    /// one — everything else pays exactly nothing.
    fn install_agent_detector(&mut self) {
        let rules = crate::agent_detect::rules::global();
        if self.pty.is_some() && self.agent_state_sink.is_some() && !rules.is_empty() {
            self.agent_detect = Some(AgentDetector::new(rules, std::time::Instant::now()));
        }
    }

    /// Service one encoded-input wakeup: the ready request plus up to
    /// [`MAX_INPUT_COALESCE`] more already queued behind it.
    ///
    /// Bounded by `MAX_INPUT_COALESCE`: the arm fires on the first ready
    /// event, then drains up to a capped batch via `try_recv` so a paste the
    /// encoder expands cannot inflate one turn without limit.
    fn service_encoded_input_batch(&mut self, request: EncodedInputRequest) {
        self.service_encoded_input(request);
        for _ in 1..MAX_INPUT_COALESCE {
            match self.encoded_input_rx.try_recv() {
                Ok(next) => self.service_encoded_input(next),
                Err(_) => break,
            }
        }
    }

    /// Service one legacy inline-input wakeup: the ready event plus up to
    /// [`MAX_INPUT_COALESCE`] more already queued behind it.
    fn service_input_batch(&mut self, input: &TerminalInput) {
        self.service_input(input);
        for _ in 1..MAX_INPUT_COALESCE {
            match self.input_rx.try_recv() {
                Ok(next) => self.service_input(&next),
                // Empty (nothing more ready) or Disconnected —
                // stop draining.
                Err(_) => break,
            }
        }
    }

    /// Ingest one PTY-ingress event: a bounded, coalesced write into the
    /// `Terminal` plus its broadcast, or EOF.
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
        // Server-side echo: the first output after an input handoff on this
        // pane. Anything slower than the ceiling is a program that did not
        // echo, not a slow server.
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
        // Debug level deliberately (was trace): this is
        // the pump's only witness line, and the lost-echo
        // forensics (phux-dacb follow-up) need it inside
        // the test capture's debug filter. Per-wakeup, so
        // it costs one line per coalesced read, not per
        // byte.
        debug!(
            bytes = burst.payload.len(),
            "vt_write: PTY chunk(s) -> Terminal"
        );
        let Some(seq) = self.raw_seq.checked_add(1) else {
            error!("actor-global raw output sequence exhausted");
            self.shutdown_pty().await;
            return PtyTurn::Shutdown;
        };
        self.raw_seq = seq;
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let deferred = self.buffer_native_live_output(seq, &burst.payload);
        #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
        let deferred = false;
        if !deferred {
            let apply_started = std::time::Instant::now();
            self.ingest_pty_payload(&burst.payload);
            crate::perf::PTY_VT_APPLY.record_elapsed(apply_started);
        }
        let _ = self.output_tx.send(PaneOutput::Live {
            seq,
            bytes: burst.payload,
        });
        let native_step_due = self.native_bootstrap_pending();
        if burst.saw_eof {
            self.handle_pty_eof();
        } else if burst.hit_byte_cap {
            // A capped payload with more output queued:
            // yield so the runtime re-polls (input arm
            // first) and sibling LocalSet tasks advance,
            // bounding the output arm at the thread level.
            // The next loop turn coalesces the next
            // bounded payload, so throughput is preserved.
            tokio::task::yield_now().await;
        }
        PtyTurn::Stepped(native_step_due)
    }

    /// Coalesce any chunks already queued behind `first`
    /// into a single Terminal write + broadcast frame
    /// (phux-ahk burst path). A lone chunk takes the
    /// fast path below: its `Vec` moves into `Bytes`
    /// with no copy. Only a genuine burst (several
    /// reads queued) allocates a join buffer. The drain
    /// stops on the chunk-count cap, on EOF, or once the
    /// payload would cross `MAX_PTY_COALESCE_BYTES` — in
    /// the byte-cap case the crossing chunk is left
    /// queued for the next turn (mpsc has no peek, so the
    /// length is checked before `try_recv`).
    fn coalesce_pty_burst(&mut self, first: Bytes) -> PtyBurst {
        let mut coalesced: Vec<u8> = Vec::new();
        let mut saw_eof = false;
        let mut hit_byte_cap = false;
        let mut chunks: u64 = 1;
        for _ in 0..MAX_PTY_COALESCE {
            // Length so far: the lone `first` chunk before
            // any coalescing, else the join buffer. Stop
            // before consuming a chunk that would push the
            // payload past the byte cap so each `vt_write`
            // is a bounded synchronous parse. The first
            // chunk always lands; only coalescing is capped.
            let current_len = if coalesced.is_empty() {
                first.len()
            } else {
                coalesced.len()
            };
            if current_len >= MAX_PTY_COALESCE_BYTES {
                hit_byte_cap = true;
                break;
            }
            match self.pty_rx.as_mut().map(mpsc::Receiver::try_recv) {
                Some(Ok(PtyEvent::Bytes { chunk: more, .. })) => {
                    if coalesced.is_empty() {
                        coalesced.reserve(first.len() + more.len());
                        coalesced.extend_from_slice(&first);
                    }
                    coalesced.extend_from_slice(&more);
                    chunks += 1;
                }
                // A queued EOF: flush the coalesced bytes
                // first, then handle EOF below.
                Some(Ok(PtyEvent::Eof)) => {
                    saw_eof = true;
                    break;
                }
                // Empty (nothing more ready) or the sender
                // dropped — stop draining. A dropped sender
                // surfaces as EOF on the next pump wakeup.
                _ => break,
            }
        }
        // The lone-chunk path is now genuinely copy-free: the reader thread
        // already hands over a refcounted `Bytes`, so a single chunk moves
        // through with no re-buffering at all. The join buffer, and its
        // copy, remains for the short-read bursts it was written for —
        // which on macOS is every burst, because the line discipline caps a
        // PTY read at 1024 bytes (see `spawn::PTY_READ_CHUNK`).
        let payload: Bytes = if coalesced.is_empty() {
            first
        } else {
            Bytes::from(coalesced)
        };
        PtyBurst {
            payload,
            chunks,
            saw_eof,
            hit_byte_cap,
        }
    }

    /// Write one coalesced PTY payload into the canonical `Terminal` and fan
    /// out everything derived from it (color queries, encoder snapshot, dirty
    /// bits, semantic events, native bootstrap advance).
    ///
    /// Every step here was measured before being kept or dropped. Over 4 KiB
    /// plain-text chunks, in milliseconds of CPU per MB ingested (release
    /// build, macOS, `TerminalActor::new(200, 50)`):
    ///
    /// | step | before | after |
    /// |---|---|---|
    /// | libghostty `vt_write` | 0.71 – 0.76 | unchanged |
    /// | OSC scanners (`answer_color_queries` + `osc133`) | 1.36 – 1.39 | **0.03** |
    /// | `publish_input_snapshot` (≈10 FFI reads + a `watch` send) | 0.02 | unchanged |
    /// | `refresh_title` (FFI read + string compare) | 0.00 | unchanged |
    ///
    /// The two FFI-shaped steps that look expensive are not — libghostty's
    /// mode and title reads disappear into the noise — so they stay
    /// unconditional. An earlier revision gated them behind a "did this
    /// payload contain an escape byte" scan; that scan cost ~0.5 ms/MB, as
    /// much as the whole VT parse, to avoid 0.02, and was removed.
    ///
    /// The scanners were where the money was: two byte-at-a-time state
    /// machines walking every byte of output to find an introducer that plain
    /// text never contains. The fix that survived therefore lives inside them
    /// (a ground-state `memchr` skip), not around them, and takes the actor's
    /// whole per-chunk ingest cost on plain output from ~2.1 to ~0.6 ms/MB.
    fn ingest_pty_payload(&mut self, payload: &Bytes) {
        self.terminal.borrow_mut().vt_write(payload);
        self.answer_color_queries(payload);
        self.publish_input_snapshot();
        self.terminal_dirty_since_tick = true;
        self.agent_dirty_since_detect = true;
        self.source_events_from_chunk(payload);
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        self.start_next_native_bootstrap();
    }

    /// Answer one bounded `SnapshotRequest` with the pane's replay bytes and
    /// the actor-global raw cut they were taken at.
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
            .send(snap.map(|snapshot| (snapshot, self.raw_seq)));
    }

    /// Install the effective default palette an interactive client reported,
    /// then acknowledge it.
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

    /// Answer one `GET_SCREEN` projection request, falling back to an empty
    /// screen of the request's shape when projection fails.
    fn reply_screen_state(&self, req: ScreenRequest) {
        let want_cells = req.cells;
        let screen = self
            .screen_state(req.pane, req.scrollback, req.cells)
            .unwrap_or_else(|err| {
                warn!(error = %err, "screen projection failed; replying with empty");
                phux_core::screen::ScreenState {
                    schema_version: phux_core::screen::SCHEMA_VERSION,
                    pane: req.pane,
                    cols: self.cols,
                    rows: self.rows,
                    cursor: None,
                    lines: Vec::new(),
                    scrollback: Vec::new(),
                    // Honour the request shape even on the error
                    // path: an empty cells vec, not a misleading
                    // `None`, when the caller asked for cells.
                    cells: want_cells.then(Vec::new),
                    ..phux_core::screen::ScreenState::default()
                }
            });
        let _ = req.reply.send(screen);
    }

    /// ADR-0032: hand the upgrade producer this pane's PTY
    /// descriptors + a full replay snapshot. Read-only; mirrors
    /// the snapshot/pwd paths.
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
        let title = (!self.last_title.is_empty()).then(|| self.last_title.clone());
        let _ = req.reply.send(PaneUpgradeHandle {
            master_fd,
            child_pid,
            cols: self.cols,
            rows: self.rows,
            cell_px,
            title,
            cwd,
            vt_replay_bytes: snap.bytes,
            scrollback_bytes: snap.scrollback,
        });
    }

    /// Resolve the pane's live working directory by asking
    /// the kernel for the PTY child's CWD (the shell's
    /// directory *now*, after any `cd`). `None` when there
    /// is no PTY (no-PTY actor), the child has no pid, or
    /// the query is unsupported/denied — the caller then
    /// falls back to a non-inherited default.
    fn reply_pane_cwd(&self, req: PwdRequest) {
        let cwd = self
            .pty
            .as_ref()
            .and_then(|p| p.child.process_id())
            .and_then(crate::cwd_query::process_cwd)
            .map(|p| p.to_string_lossy().into_owned());
        let _ = req.reply.send(cwd);
    }

    /// Apply one resize request, returning the [`ResyncReason`] the caller
    /// should arm the debounce timer with, or `None` when this resize earns no
    /// client resync.
    ///
    /// phux-8v1: re-broadcast a full snapshot for live
    /// resizes so client mirrors reconverge after their
    /// independent reflow. Suppressed for the ATTACH-time
    /// resize (the handshake snapshot covers it). Debounced
    /// (`RESIZE_RESYNC_DEBOUNCE`) so a drag storm — or a burst of
    /// lag-resync requests — coalesces into a single snapshot
    /// rather than flooding the client.
    ///
    /// phux-a5xj: also suppressed when the geometry did not
    /// move. There is no independent reflow to reconverge from
    /// if nothing reflowed, and the resync is what rotates the
    /// bootstrap generation — so a client confirming the size
    /// it already asked for at spawn must not cost the pane the
    /// checkpoint it just published.
    fn apply_resize_request(&mut self, req: ResizeRequest) -> Option<ResyncReason> {
        // A `resync_only` request (from a lagged output pump)
        // carries no geometry — skip the resize and only schedule
        // the resync broadcast below.
        let reflowed = if req.resync_only {
            false
        } else {
            self.handle_resize(req.cols, req.rows, req.cell_px)
        };
        if !req.resync_clients || !(req.resync_only || reflowed) {
            return None;
        }
        Some(if req.resync_only {
            ResyncReason::OutboundGap
        } else {
            ResyncReason::Resize
        })
    }

    /// One tick of the state-sync emission driver (phux-q0e.3, phux-ia4).
    ///
    /// Walks every attached consumer in turn. For each:
    ///
    /// 1. Call [`SnapshotSynthesizer::synthesize_against_reference`] using
    ///    the actor's shared synthesizer and the consumer's *own*
    ///    reference grid. The reference is per-consumer and independent of
    ///    the shared `Terminal` dirty bits, so every consumer on a shared
    ///    pane gets its own correct diff this tick — even though
    ///    libghostty's `RenderState::update` consumes the shared dirty
    ///    state on the first read (the phux-ia4 fix). Synthesis errors are
    ///    logged and that consumer is skipped for this tick (no kill: a
    ///    transient FFI error on one consumer must not poison the others).
    /// 2. If the body is empty, skip — the viewport is byte-identical to
    ///    that consumer's reference (steady state between writes).
    /// 3. Stamp the per-consumer monotonic `seq` (starting at `1`,
    ///    incrementing per emission) and ship a `TerminalOutput` frame
    ///    via the per-consumer outbound mailbox.
    ///
    /// Emit-once (phux-ia4): `synthesize_against_reference` advances the
    /// consumer's reference before returning a non-empty body, so a given
    /// change is emitted exactly once and an unchanged terminal produces no
    /// re-emission on the next tick. This is the v0.1 reliable-transport
    /// model (proto.md §8); the loss-tolerance re-diff property is a future
    /// lossy-transport concern (ADR-0018) and is not wired here.
    pub(super) fn tick_emit(&mut self) {
        // Per-tick observation span (hot path, so debug level: the default
        // `phux=info` filter leaves it disabled and effectively free —
        // `tracing` skips a disabled span without evaluating its fields).
        // The correlation fields a trace reader greps for to localize
        // server-side lag: how many consumers this tick must serve and
        // whether the grid is dirty. `consumer_count` is read before the
        // gate so the span is consistent on the gated-off / idle-skip
        // return paths too; `emitted` + `total_out_bytes` are recorded at
        // the end of a productive tick.
        let tick_span = tracing::debug_span!(
            "tick_emit",
            consumer_count = self.consumer_states.len(),
            dirty = self.terminal_dirty_since_tick,
            // Filled in at the end of a productive tick via `record`; declared
            // `Empty` so they exist on the span for later assignment.
            emitted = tracing::field::Empty,
            total_out_bytes = tracing::field::Empty,
        )
        .entered();

        // Emission gate (phux-0q8 / phux-3uv / phux-ia4 / phux-fseo). The tick
        // emits only for a *tick-managed* consumer — one that negotiated
        // `OutputMode::StateSync` (`state.wants_state_sync`), or any consumer
        // when the global test gate forces it; the runtime suppresses its
        // broadcast pump for exactly those (see `ConsumerAttachOutcome`). A
        // raw consumer is served by the pump, so the tick stays silent for it
        // to avoid double-painting. `force_all_consumers` is captured here so
        // the loop below reads it without re-borrowing `self` while it holds
        // `&mut self.consumer_states`.
        let force_all_consumers = self.consumer_tick_emits;
        if !force_all_consumers && !self.consumer_states.values().any(|s| s.wants_state_sync) {
            // No tick-managed consumer: nothing to emit (dirty flag untouched).
            return;
        }

        // Idle short-circuit (phux-4l0). The per-consumer reference diff
        // walks + renders every viewport row into a throwaway `Vec<u8>`
        // for every consumer, every tick — pure waste when nothing has
        // changed. Take and reset the "mutated since last tick" flag here;
        // if the terminal is unchanged AND no consumer is awaiting its
        // first emission, skip the entire per-consumer loop.
        let mutated = self.terminal_dirty_since_tick;
        self.terminal_dirty_since_tick = false;
        if !mutated && !self.consumer_states.values().any(must_walk_when_clean) {
            return;
        }
        // Timed from here so gated-off and idle ticks, which are the common
        // case and nearly free, do not swamp the histogram.
        let tick_started = std::time::Instant::now();

        // Borrow the terminal + shared synthesizer once per tick. The
        // synthesizer's `RenderState`/iterators are reused across
        // consumers; the per-consumer state lives in each `reference`.
        let terminal = self.terminal.borrow();
        let mut synth = self.synth.borrow_mut();
        // phux-ahk.2: render the grid ONCE for this tick (the consumer-
        // independent snapshot + per-row cell render + cursor/mode FFI +
        // epilogue/screen-toggle precompute). Each consumer below then only
        // DIFFS against the shared result via `diff_consumer`, so a pane with
        // N state-sync consumers renders once, not N times.
        let render = match synth.prepare_tick(&terminal) {
            Ok((cols, rows, live_cm)) => TickRender {
                cols,
                rows,
                live_cm,
            },
            Err(err) => {
                warn!(error = %err, "state-sync tick: prepare_tick failed; skipping tick");
                return;
            }
        };
        // Consumers whose outbound mailbox is `Closed` (receiver dropped)
        // are reaped after the loop so a missed detach (phux-ddg) does not
        // leave a dead `ConsumerReference` to be re-rendered forever.
        let mut closed: Vec<ClientId> = Vec::new();
        // Per-tick emission tally recorded onto the tick span on the way out
        // (frames actually shipped + their total byte volume) — the headline
        // "frame N for terminal T was Y bytes" reconstruction signal.
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
        drop(terminal);
        // Record the per-tick emission tally on the tick span so a reader
        // can reconstruct "tick served N consumers, shipped M frames /
        // B bytes" without re-deriving it from the per-consumer trace lines.
        tick_span.record("emitted", emitted);
        tick_span.record("total_out_bytes", total_out_bytes);
        crate::perf::TICK_EMIT.record_elapsed(tick_started);
        for client_id in closed {
            self.consumer_states.remove(&client_id);
        }
    }

    /// Serve one consumer within a state-sync tick: reserve its outbound
    /// slot, diff the just-rendered grid against its reference, and ship the
    /// delta.
    fn emit_consumer_tick(
        client_id: ClientId,
        state: &mut ConsumerSyncState,
        synth: &SnapshotSynthesizer<'_>,
        render: TickRender,
        mutated: bool,
        force_all_consumers: bool,
    ) -> TickOutcome {
        // phux-fseo: serve only tick-managed consumers. A raw consumer
        // sharing this pane is served by the broadcast pump; emitting here
        // too would double-paint it, so skip it (reference left untouched
        // for a later mode flip).
        if !force_all_consumers && !state.wants_state_sync {
            return TickOutcome::Skipped;
        }
        if !*state.live_gate.borrow() {
            return TickOutcome::Skipped;
        }
        // Captured before the `behind` reset below: whether a prior tick
        // held this consumer's delta back for a full mailbox. The
        // loss-tolerant emit gate treats a drained-after-backpressure
        // consumer as "has new content to ship" (phux-v45.8).
        let was_behind = state.behind;
        // This consumer is being serviced this tick; it no longer needs
        // a forced first pass.
        state.needs_initial_emit = false;
        // Reserve an outbound permit BEFORE synthesizing
        // (phux-wave-hunt/server-lifecycle). `synthesize_against_reference`
        // commits the per-consumer reference to the just-rendered grid
        // *before* it returns the bytes (emit-once, grid.rs), so once we
        // synthesize the delta is the only copy and the reference has
        // moved past it. If the send then failed `Full` we would drop the
        // delta and never re-emit it (the next tick diffs against the
        // already-advanced reference), silently losing content and
        // diverging the client mirror forever.
        //
        // Reserving first inverts the ordering: a `Full` mailbox means we
        // skip this consumer entirely this tick WITHOUT synthesizing, so
        // the reference (and `next_seq`) stay put and the delta is
        // re-diffed intact on the next tick once the client drains. A
        // `Closed` mailbox reaps the entry (phux-ddg self-heal). Only when
        // we hold a permit — which guarantees the subsequent send cannot
        // fail — do we synthesize, advance the reference, and ship.
        let permit = match state.outbound.try_reserve() {
            Ok(permit) => permit,
            Err(tokio::sync::mpsc::error::TrySendError::Full(())) => {
                // Backpressure: the consumer mailbox is wedged. Skip
                // without advancing the reference so no content is lost;
                // the next tick retries the same delta. Mark `behind` so
                // the idle short-circuit keeps walking this consumer even
                // if the grid goes `Clean` before the client drains — the
                // retry must not depend on a fresh write. At debug so a
                // stall is visible at the recommended `phux=debug` level.
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
                // The receiver is gone. A `ConsumerDetachRequest` may have
                // been dropped (best-effort `try_send` on a full detach
                // mailbox, runtime.rs) so `unregister_consumer` never ran.
                // Self-heal: reap the entry now so we stop re-rendering a
                // dead consumer every tick (phux-ddg).
                crate::perf::CONSUMER_REAPED.incr();
                debug!(
                    ?client_id,
                    wire_terminal_id = state.wire_terminal_id,
                    "state-sync tick: consumer mailbox closed; reaping entry",
                );
                return TickOutcome::Closed;
            }
        };
        // We hold a permit: the mailbox has room, so this consumer is
        // about to be fully serviced this tick (a delta ships, or the
        // diff is empty and the reference is already at the live grid).
        // Either way it is no longer behind.
        state.behind = false;
        // Per-consumer synthesis span (debug; the per-tick CPU sink —
        // its duration is the key server-side lag signal). Carries the
        // consumer correlation fields; the diff size lands in
        // `synthesize_against_reference`'s own child span.
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
            // Byte-identical to this consumer's reference; nothing to
            // send this tick. The reserved permit drops unused. A closed
            // mailbox was already reaped by the `try_reserve` arm above,
            // so no extra liveness probe is needed here.
            return TickOutcome::Skipped;
        }
        if state.loss_tolerant && holds_loss_tolerant_delta(state, mutated, was_behind) {
            return TickOutcome::Skipped;
        }
        let seq = state.next_seq;
        let out_bytes = bytes.len();
        crate::perf::TICK_OUT_BYTES.record_len(out_bytes);
        // Wrapping_add for paranoia; `u64` will not realistically
        // roll over at 33 Hz, but the existing `runtime.rs` pump
        // uses the same idiom and we match it.
        state.next_seq = state.next_seq.wrapping_add(1);
        let frame = FrameKind::TerminalOutput {
            terminal_id: phux_protocol::ids::TerminalId::local(state.wire_terminal_id),
            stream_id: state.stream_id,
            bootstrap_id: state.bootstrap_id,
            seq,
            bytes: bytes.into(),
        };
        // Infallible: we hold a reserved permit, so this cannot block,
        // drop, or fail. This preserves the actor's single-poll-budget
        // invariant (the tick arm never yields the loop) while keeping
        // emit-once consistent — a synthesized delta always ships.
        permit.send(Outbound::Frame(frame));
        record_emit_instant(&mut state.emit_instants, seq);
        // phux-v45.8: for a loss-tolerant consumer, snapshot the grid state
        // this `seq` shipped so a later cumulative `FRAME_ACK` can advance
        // the acked reference to exactly it. Bounded the same way as
        // `emit_instants` (oldest-evicted past the cap) so a wedged leg
        // cannot grow it without bound. Empty/untouched on the emit-once
        // path.
        if state.loss_tolerant {
            let snapshot = synth.snapshot_tick_reference(render.cols, render.rows, render.live_cm);
            record_pending_ref(&mut state.pending_refs, seq, snapshot);
        }
        trace!(
            ?client_id,
            wire_terminal_id = state.wire_terminal_id,
            seq,
            out_bytes,
            "state-sync tick: TERMINAL_OUTPUT emitted",
        );
        TickOutcome::Emitted(out_bytes)
    }
}

/// One consumer's delta bytes against the just-rendered tick.
///
/// `diff_consumer` is infallible (the fallible render happened once in
/// `prepare_tick`); it returns this consumer's delta bytes.
/// phux-v45.8: a loss-tolerant consumer re-diffs against its
/// last-ACKED reference (which does NOT advance on emit), so a
/// dropped/un-acked frame self-heals — its rows still differ from the
/// acked reference on the next emission. The reliable-transport
/// default stays on the emit-once `diff_consumer` path (reference
/// advances on emit), byte-for-byte unchanged.
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

/// Whether a consumer must still be walked on a `Clean` terminal.
///
/// Correctness: a `Clean` terminal cannot have diverged from any
/// consumer's last-emitted reference (the reference advanced to the
/// terminal state on the prior emit, and nothing has mutated the
/// terminal since), so skipping is sound. Two carve-outs suppress the
/// short-circuit even on a `Clean` terminal:
///
/// - `needs_initial_emit` preserves the phux-ia4 multi-consumer
///   guarantee: a consumer registered *after* the last write sits on a
///   clean terminal yet has never had a synthesis pass, so it must be
///   walked once even though the global flag is clear.
/// - `behind` preserves the backpressure retry: a consumer skipped on
///   a prior tick because its mailbox was full has a reference behind
///   the live grid. The grid can stay `Clean` indefinitely, so without
///   this the held-back delta would never be retried once the client
///   drains (the wave-hunt/server-lifecycle backpressure leak).
///
/// phux-v45.8: a loss-tolerant consumer with un-acked frames in
/// flight must keep being walked even on a Clean terminal, so its
/// retransmit timer can fire and re-diff a suspected-lost frame
/// against the acked reference. `pending_refs` is empty for the
/// reliable emit-once path, so this adds nothing there.
fn must_walk_when_clean(state: &ConsumerSyncState) -> bool {
    state.needs_initial_emit || state.behind || !state.pending_refs.is_empty()
}

/// phux-v45.8 loss-tolerant emit gate. Because a loss-tolerant diff
/// is against the last-acked (not last-emitted) reference, it stays
/// non-empty every tick while a frame is un-acked. Only actually
/// (re)ship when there is genuinely new content this tick
/// (`mutated`), a drained-after-backpressure delta to flush
/// (`was_behind`), or a retransmit is due for a still-un-acked frame
/// (suspected loss). Otherwise hold: re-shipping the same cumulative
/// delta every tick would flood the leg. The reserved permit drops.
fn holds_loss_tolerant_delta(state: &ConsumerSyncState, mutated: bool, was_behind: bool) -> bool {
    let now = tokio::time::Instant::now();
    let retransmit_due = !state.pending_refs.is_empty()
        && state.emit_instants.values().next_back().is_none_or(|last| {
            now.saturating_duration_since(*last) >= tick::retransmit_timeout(state.rtt.smoothed())
        });
    !mutated && !was_behind && !retransmit_due
}

/// Stamp the emit instant for this seq so the matching `FRAME_ACK`
/// can be turned into an RTT sample (phux-q0e.5). Recorded only
/// for shipped frames — empty/skipped ticks have no round-trip to
/// measure. Pruned on ack, so the map stays as small as the
/// in-flight window.
///
/// Defensive bound: ack-pruning keeps this map tiny for a
/// well-behaved consumer, but one that opts into state sync and
/// never sends `FRAME_ACK` (or a transport that drops acks) would
/// otherwise grow it one entry per emitted tick without bound
/// (~50/s at the 20ms floor cadence). Evict the oldest (lowest-seq)
/// samples past the cap; an unacked sample this stale is already
/// useless for RTT, so dropping it costs nothing and bounds the
/// map to a few KB per consumer. See `MAX_EMIT_INSTANTS`.
fn record_emit_instant(
    emit_instants: &mut std::collections::BTreeMap<u64, tokio::time::Instant>,
    seq: u64,
) {
    emit_instants.insert(seq, tokio::time::Instant::now());
    while emit_instants.len() > MAX_EMIT_INSTANTS {
        emit_instants.pop_first();
    }
}

/// Retain the grid snapshot a loss-tolerant `seq` shipped, bounded the same
/// way as the emit instants (oldest-evicted past [`MAX_EMIT_INSTANTS`]).
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

/// Build a `MissedTickBehavior::Delay` interval and eat its first tick.
///
/// `Delay` — if the actor falls behind under heavy PTY traffic we want
/// subsequent ticks spaced by the interval from when they ran, not bunched up
/// to "catch up" (which would defeat the rate limit's purpose). `Burst` (the
/// default) would spam emissions when a long PTY chunk delays us past several
/// tick boundaries.
///
/// Eat the first immediate tick (Interval fires synchronously on
/// first poll). Without this, the very first iteration would
/// tick before any other branch has a chance to react.
async fn armed_interval(period: std::time::Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let _ = interval.tick().await;
    interval
}

#[cfg(test)]
mod tick_rearm_tests {
    use super::{DEFAULT_TICK_INTERVAL, armed_interval};

    /// The state-sync tick arm is disarmed while a pane has nothing to emit
    /// (see [`TerminalActor::state_tick_armed`]), which means its `Interval`
    /// can sit unpolled across thousands of missed periods. Re-arming must
    /// cost exactly one tick.
    ///
    /// This is a guard on [`armed_interval`]'s `MissedTickBehavior`, not on
    /// the `select!`: switch it to tokio's default `Burst` and this test
    /// reports ~120,000 immediately-ready ticks instead of one, which on a
    /// shared current-thread runtime is a stall for every pane on the server.
    #[tokio::test(start_paused = true)]
    async fn disarming_the_tick_does_not_bank_a_stampede_of_catch_up_ticks() {
        let mut tick = armed_interval(DEFAULT_TICK_INTERVAL).await;

        // An hour with the arm's precondition false: nothing polls the timer.
        tokio::time::advance(std::time::Duration::from_secs(3600)).await;

        // Re-armed. Count the ticks that are ready with no further time
        // passing; a zero-length timeout resolves against the paused clock
        // without letting it advance to the next deadline.
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
mod resync_debounce_tests {
    use std::time::Duration;

    use super::{RESIZE_RESYNC_DEBOUNCE, ResyncDebounce, ResyncReason};

    fn idle() -> ResyncDebounce {
        ResyncDebounce {
            pending: false,
            reason: ResyncReason::Resize,
        }
    }

    /// A pane with many lagged consumers must still get its one snapshot.
    ///
    /// Every fenced output pump asks the actor for the same in-band resync,
    /// and they retry independently. If each request restarted the 50 ms
    /// debounce, N pumps arriving at a mean interval of `retry / N` would keep
    /// pushing the deadline out faster than it could fire — the snapshot they
    /// are all waiting for would never be broadcast and none of them would
    /// ever unfence. That is a livelock, not a slowdown: it gets *worse* the
    /// more consumers a pane has.
    ///
    /// Ten consumers, each asking twice, on a clock that only ever advances by
    /// less than the debounce window. The deadline must not move after the
    /// first request.
    #[tokio::test(start_paused = true)]
    async fn a_pending_gap_resync_is_not_pushed_out_by_more_lagged_consumers() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut debounce = idle();

        debounce.arm(ResyncReason::OutboundGap, sleep.as_mut());
        let first_deadline = sleep.deadline();
        assert!(debounce.pending);

        for _ in 0..20 {
            // Faster than the debounce, which is exactly the starving case.
            tokio::time::advance(RESIZE_RESYNC_DEBOUNCE / 4).await;
            debounce.arm(ResyncReason::OutboundGap, sleep.as_mut());
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
        assert_eq!(debounce.take_reason(), ResyncReason::OutboundGap);
    }

    /// The other half: a resize storm still re-arms every time, because there
    /// the *last* size is the one worth synthesizing. Only the gap path
    /// coalesces.
    #[tokio::test(start_paused = true)]
    async fn a_resize_still_restarts_the_debounce() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut debounce = idle();

        debounce.arm(ResyncReason::Resize, sleep.as_mut());
        let first_deadline = sleep.deadline();
        tokio::time::advance(RESIZE_RESYNC_DEBOUNCE / 4).await;
        debounce.arm(ResyncReason::Resize, sleep.as_mut());
        assert!(
            sleep.deadline() > first_deadline,
            "a drag storm must settle on the last size",
        );
    }

    /// A resize arriving while a gap resync is owed takes over: it re-arms and
    /// carries the resize reason, and its snapshot unfences the gapped pumps
    /// just as well.
    #[tokio::test(start_paused = true)]
    async fn a_resize_supersedes_a_pending_gap_resync() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut debounce = idle();

        debounce.arm(ResyncReason::OutboundGap, sleep.as_mut());
        let gap_deadline = sleep.deadline();
        tokio::time::advance(RESIZE_RESYNC_DEBOUNCE / 4).await;
        debounce.arm(ResyncReason::Resize, sleep.as_mut());

        assert!(sleep.deadline() > gap_deadline);
        assert_eq!(debounce.take_reason(), ResyncReason::Resize);
    }
}
