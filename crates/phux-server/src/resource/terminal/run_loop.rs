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

#[path = "run_loop_drive.rs"]
mod drive;
#[path = "run_loop_tick.rs"]
mod emitter;
#[path = "run_loop_ingress.rs"]
mod ingress;
#[path = "run_loop_replies.rs"]
mod replies;
use emitter::must_walk_when_clean;
#[path = "run_loop_helpers.rs"]
mod helpers;
pub(super) use helpers::icount_join_pty_burst;
use helpers::{armed_interval, join_pty_burst};
#[cfg(test)]
#[path = "tests_run_loop.rs"]
mod tests;

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
    /// Fire any owed gap resync, then tear the PTY down after the
    /// actor-global cancel token fires.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    async fn shutdown_from_cancel(&mut self, resync: &mut ResyncDebounce) {
        let _ = self.flush_final_gap_resync(resync);
        self.land_native_cuts();
        debug!("TerminalActor cancellation token fired");
        self.shutdown_pty().await;
    }

    /// Drain queued gap-resync requests and fire any owed snapshot now, so
    /// an EOF'd pane does not get reaped before its fenced pumps are served.
    pub(super) fn flush_final_gap_resync(&mut self, resync: &mut ResyncDebounce) -> bool {
        // Callers run outside the bootstrap guards, and both the drain and
        // the fire touch the canonical terminal, so land any in-flight
        // capture first.
        self.land_native_capture();
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
        // Resume queued captures only after both final snapshots have read
        // the canonical terminal; starting one earlier would loan it again.
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        self.start_next_native_bootstrap();
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
}
