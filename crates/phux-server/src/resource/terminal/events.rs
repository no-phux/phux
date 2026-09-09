//! Agent-event sourcing and supervisory control for [`TerminalActor`]:
//! event sinks and subscribers, the agent-state detector tick, OSC-133
//! sourced events, dirty/idle coalescing, cwd tracking, and signal
//! delivery (ADR-0033, ADR-0046).

use super::{
    AgentDetectEvent, AgentEvent, AskMarker, ControlAction, ControlRequest, DetectOutcome,
    TerminalActor, TerminalLifecycle, TerminalSignal, mpsc, osc133, trace,
};
use crate::agent_asked::AskedPayload;

impl TerminalActor {
    /// Wire an agent-event sink (SPEC §7.5, phux-y2t). The actor emits
    /// `bell` / `title_changed` / `dirty` / `idle` / `command_*` events to
    /// `sink`; the runtime drains it and fans each event out to
    /// event-stream subscribers scoped to this pane. Called by the spawn
    /// path before the actor is handed to `spawn_local`.
    pub fn set_event_sink(&mut self, sink: mpsc::Sender<AgentEvent>) {
        self.event_sink = Some(sink);
    }

    /// Best-effort agent-event emission (SPEC §7.5). `try_send` so a full
    /// sink drops the event rather than stalling the actor — the event
    /// stream is an accelerator, never a guarantee. No-op when no sink is
    /// wired (the common test path).
    pub(super) fn emit_event(&self, event: AgentEvent) {
        if let Some(sink) = self.event_sink.as_ref() {
            let _ = sink.try_send(event);
        }
    }

    /// Wire the agent-state detector's sink (ADR-0046). The actor's detector
    /// timer emits edge-filtered [`AgentDetectEvent`]s here; the runtime's
    /// `spawn_agent_state_drain` owns `ServerState` and performs the
    /// arbitration + `metadata_set`. Called by the spawn path before the
    /// actor is handed to `spawn_local`, exactly like [`Self::set_event_sink`].
    ///
    /// `pub(crate)`, unlike `set_event_sink`: `AgentEvent` is a wire type, but
    /// [`AgentDetectEvent`] is deliberately server-internal — the detector
    /// introduces no wire surface.
    pub(crate) fn set_agent_state_sink(&mut self, sink: mpsc::Sender<AgentDetectEvent>) {
        self.agent_state_sink = Some(sink);
    }

    /// Best-effort detector emission. `try_send`: a full sink drops the
    /// event rather than stalling the actor. Safe to drop — the detector is
    /// level-triggered, so the next tick re-derives and re-publishes.
    pub(super) fn emit_agent_state(&self, event: AgentDetectEvent) {
        let _ = self.try_emit_agent_state(event);
    }

    pub(super) fn try_emit_agent_state(&self, event: AgentDetectEvent) -> bool {
        if let Some(sink) = self.agent_state_sink.as_ref() {
            return sink.try_send(event).is_ok();
        }
        false
    }

    /// Sync `last_title` with libghostty's tracked OSC 0/2 title. Returns
    /// `true` on a real change.
    ///
    /// The `RefCell` borrow of `self.terminal` MUST be released before
    /// `self.last_title` is written, hence the two-step. No allocation on the
    /// steady path: the common case compares and returns `false`.
    pub(super) fn refresh_title(&mut self) -> bool {
        let next: Option<String> = {
            let terminal = self.terminal.borrow();
            let current = terminal.title().unwrap_or("");
            (current != self.last_title).then(|| current.to_owned())
        }; // borrow released here — required
        match next {
            Some(title) => {
                self.last_title = title;
                true
            }
            None => false,
        }
    }

    /// Right-trimmed live-viewport rows, top to bottom. The detector's ONLY
    /// grid read.
    ///
    /// Routes through the synthesizer's fresh-`RenderState` projection
    /// (`scrollback = None`, so the LIVE screen and never history). That
    /// projection deliberately allocates its own `RenderState` per call
    /// rather than reusing the pooled one, precisely so a read like this does
    /// not consume the shared libghostty dirty bits the per-consumer
    /// state-sync tick needs (the phux-ia4 bug). Do not "optimize" it.
    pub(super) fn viewport_lines(&self) -> Option<Vec<String>> {
        let terminal = self.terminal.borrow();
        let synth = self.synth.borrow();
        match synth.screen_state_with_scrollback(&terminal, 0, None, false) {
            Ok(state) => Some(state.lines),
            Err(err) => {
                trace!(error = %err, "agent-detect: viewport read failed; skipping tick");
                None
            }
        }
    }

    /// One agent-detector tick (ADR-0046). Returns the interval the caller
    /// should re-arm the detector timer at.
    ///
    /// The detector is taken out of its `Option` for the duration: it needs
    /// `&mut` while `viewport_lines` needs `&self`, and the borrow checker
    /// will not have both. Put back before returning, always.
    pub(super) fn detect_tick(&mut self) -> Option<std::time::Duration> {
        let mut detector = self.agent_detect.take()?;
        let now = std::time::Instant::now();
        // The detector's OWN dirty flag, not `terminal_dirty_since_tick`:
        // that one is cleared every ~30 ms by `tick_emit`, so a detector
        // ticking at 100-500 ms would observe it as `false` almost always and
        // skip every single scan.
        //
        // The flag is CONSUMED only by a scan that actually happened. A tick
        // that skips the scan must not eat the evidence that a scan is owed:
        // `wants_screen` is false for the whole of a pane's unidentified life,
        // so consuming it unconditionally threw away every grid mutation an
        // agent made before we noticed it existed — including the one that
        // painted the permission dialog we were supposed to see. Likewise a
        // failed projection leaves the flag set, so the next tick retries.
        let dirty = self.agent_dirty_since_detect;
        let screen = if detector.wants_screen(dirty) {
            let lines = self.viewport_lines();
            if lines.is_some() {
                self.agent_dirty_since_detect = false;
            }
            lines
        } else {
            None
        };
        let master_fd = self
            .pty
            .as_ref()
            .and_then(|p| p.master.lock().ok().and_then(|m| m.as_raw_fd()));
        let pane_child_pid = self
            .pty
            .as_ref()
            .and_then(|p| p.child.process_id().and_then(|pid| i32::try_from(pid).ok()));
        let outcome = detector.tick_for_pane(
            now,
            master_fd,
            pane_child_pid,
            &self.last_title,
            &self.last_progress,
            screen.as_deref(),
        );
        let occupant = detector.take_occupant_update();
        let next = detector.interval();

        if let Some(occupant) = occupant {
            if self.try_emit_agent_state(AgentDetectEvent::Occupant(occupant.clone())) {
                detector.occupant_update_sent(occupant);
            } else {
                detector.retry_occupant_update(occupant);
            }
        }
        self.agent_detect = Some(detector);

        match outcome {
            DetectOutcome::Quiet => {}
            DetectOutcome::Publish(report) => {
                self.emit_agent_state(AgentDetectEvent::State(report));
            }
            DetectOutcome::Reidentified { kind, name } => {
                self.emit_agent_state(AgentDetectEvent::Reidentified { kind, name });
            }
            DetectOutcome::Retract => self.emit_agent_state(AgentDetectEvent::Retract),
        }
        Some(next)
    }

    /// Source agent events from a freshly-applied PTY chunk (phux-y2t),
    /// called right after `vt_write`. Sources, in order:
    ///
    /// - `bell` — a BEL (`0x07`) anywhere in the chunk. Emitted once per
    ///   chunk even if several BELs arrive together (a burst of bells is
    ///   one alert from the consumer's perspective).
    /// - `title_changed` — the libghostty-tracked OSC 0 / OSC 2 title now
    ///   differs from the last observed value.
    /// - `dirty` — the chunk mutated the grid (a new output burst began).
    ///   Coalesced: at most one `dirty` per burst; the settling `idle`
    ///   fires from the tick arm.
    /// - `OutputReceived` — broadcast to semantic event subscribers.
    /// - `GridChanged` — broadcast to semantic event subscribers.
    ///
    /// `command_started` / `command_finished` (phux-foz.4) — sourced from a
    /// direct OSC-133 scan of the raw chunk (see [`osc133`]): `C` emits
    /// `command_started`, `D` emits `command_finished` with the shell's
    /// exit code when reported. The grid projection cannot yield the
    /// `D`-mark exit code, so the byte stream is the honest source. Each
    /// `D` mark is also a prompt boundary: the pane's kernel cwd is
    /// re-queried there and a change emits `cwd_changed`.
    pub(super) fn source_events_from_chunk(&mut self, chunk: &[u8]) {
        // UNCONDITIONAL, and deliberately ahead of the no-listener guard
        // below (ADR-0046). The agent-state detector reads `last_title` on
        // its own timer, and the OSC title is its highest-priority signal —
        // but this function used to return early for a pane nobody was
        // watching, which left `last_title` stale forever for exactly the
        // panes the sidebar most wants to describe. Refreshing here keeps one
        // title parser (libghostty's) and one mirror. Costs one FFI read plus
        // one compare per chunk, and allocates only when the title changes;
        // measured at under 0.01 ms/MB, which is why no attempt is made to
        // skip it (see `ingest_pty_payload`).
        let title_changed = self.refresh_title();
        let marks = self.osc133.feed(chunk);
        for mark in &marks {
            if let osc133::OscMark::Progress(progress) = mark {
                self.last_progress.clone_from(progress);
            }
        }
        if self.event_sink.is_none() && self.core.has_no_event_subscribers() {
            return;
        }
        self.output_since_idle_tick = true;
        // OSC-133 prompt marks (phux-foz.4). Scanned before the coalesced
        // dirty/title sources below so a command boundary and its dirty
        // burst arrive in stream order.
        for mark in marks {
            match mark {
                osc133::OscMark::CommandStart => {
                    self.emit_event(AgentEvent::CommandStarted);
                    self.broadcast_agent_event(&AgentEvent::CommandStarted);
                }
                osc133::OscMark::CommandEnd { exit_code } => {
                    self.emit_event(AgentEvent::CommandFinished { exit_code });
                    self.broadcast_agent_event(&AgentEvent::CommandFinished { exit_code });
                    // The command just finished: the shell is back at a
                    // prompt and any `cd` has landed. Re-query the kernel
                    // cwd and announce a change.
                    self.check_cwd_changed();
                }
                osc133::OscMark::Progress(_) => {}
            }
        }
        if memchr::memchr(0x07, chunk).is_some() {
            self.emit_event(AgentEvent::Bell);
        }
        // Title: `refresh_title` (above) already synced the mirror; emit on a
        // real change.
        if title_changed {
            self.emit_event(AgentEvent::TitleChanged {
                title: self.last_title.clone(),
            });
        }
        // Asked: source a pending human-answerable question from a `phux-ask`
        // title sentinel (phux-2sl6), tier 2 of the ADR-0036 ladder.
        //
        // The actor reports the marker; it does not decide what a subscriber
        // sees. `AskedDetector` (out in `ServerState`, where the hook reports
        // land too) ranks the sources and owns the coalescing, so an agent
        // driving both this sentinel and the hook produces one `Asked`
        // instead of two. `last_ask` survives as the TRANSPORT edge filter:
        // its only job is to keep a pane whose title is a stable `phux-ask`
        // from pushing a message per PTY chunk. Both edges travel — a marker
        // appearing or changing as `Some`, retitling away as `None` — because
        // the detector cannot infer a cleared marker, and without the clear
        // the same question asked twice would coalesce into silence.
        //
        // The marker is a function of `last_title` alone, so it can only move
        // when the title moved — or when a previous chunk's emission was
        // refused and the retry is still owed (`ask_retry_owed`). Re-parsing
        // on every chunk of a pane whose title has not changed in an hour
        // re-derives an answer that provably cannot differ.
        if title_changed || self.ask_retry_owed {
            self.source_ask_marker();
        }
        // Dirty: a chunk arrived, so the grid mutated. Coalesce to one
        // `dirty` per burst; `idle` (from the tick arm) closes the burst.
        if !self.in_output_burst {
            self.in_output_burst = true;
            self.emit_event(AgentEvent::Dirty);
            if !self.dirty_event_emitted_this_burst {
                self.broadcast_agent_event(&AgentEvent::Dirty);
                self.dirty_event_emitted_this_burst = true;
            }
        }
    }

    /// Re-derive the in-pane ask marker from the current title and ship the
    /// edge (phux-2sl6).
    ///
    /// Split out of [`Self::source_events_from_chunk`] so the caller can gate
    /// it on the two conditions under which the answer can differ from last
    /// time: the title changed, or a previous emission was refused.
    fn source_ask_marker(&mut self) {
        let current_ask = AskMarker::parse(&self.last_title);
        if current_ask == self.last_ask {
            self.ask_retry_owed = false;
            return;
        }
        let ask = current_ask.as_ref().map(|marker| AskedPayload {
            id: marker.id.clone(),
            question: marker.question.clone(),
            suggestions: marker.suggestions.clone(),
            // Elapsed-since-ask is not tracked server-side in v1; the
            // consumer renders a live waiting counter from receipt.
            elapsed_seconds: None,
        });
        // Advance the mirror only once the edge is actually in flight. An ask
        // is edge-triggered — unlike the level-triggered detector, nothing
        // re-derives it — so a full sink would otherwise swallow it outright;
        // leaving the mirror stale re-attempts on the next chunk. With no
        // sink wired at all there is nothing to deliver and nothing to retry.
        if self.try_emit_agent_state(AgentDetectEvent::AskSentinel(ask))
            || self.agent_state_sink.is_none()
        {
            self.last_ask = current_ask;
            self.ask_retry_owed = false;
        } else {
            self.ask_retry_owed = true;
        }
    }

    /// Emit `idle` when an output burst has settled (phux-y2t), called from
    /// the tick arm. A burst is "settled" when no PTY output chunk arrived
    /// since the previous tick. Idempotent: only the first settled tick after
    /// a `dirty` emits `idle`; subsequent idle ticks are silent until the next
    /// burst.
    pub(super) fn maybe_emit_idle(&mut self) {
        let had_output = std::mem::take(&mut self.output_since_idle_tick);
        if self.in_output_burst && !had_output {
            self.in_output_burst = false;
            self.dirty_event_emitted_this_burst = false;
            self.emit_event(AgentEvent::Idle);
            self.broadcast_agent_event(&AgentEvent::Idle);
            // phux-foz.4: an output burst settling is the fallback prompt
            // boundary for shells without OSC-133 integration — a `cd`
            // echoes a prompt (burst), settles (idle), and the kernel cwd
            // re-query below announces the change. One best-effort syscall
            // per settled burst.
            self.check_cwd_changed();
        }
    }

    /// phux-foz.4: re-query the PTY child's kernel cwd and emit
    /// [`AgentEvent::CwdChanged`] when it differs from the last
    /// observation. Best-effort and coalesced: no PTY / dead child /
    /// denied query all yield silence, and an unchanged directory emits
    /// nothing. Called at OSC-133 `D` prompt boundaries and on output
    /// settle.
    pub(super) fn check_cwd_changed(&self) {
        let Some(pid) = self.pty.as_ref().and_then(|p| p.child.process_id()) else {
            return;
        };
        let Some(cwd) = crate::cwd_query::process_cwd(pid) else {
            return;
        };
        let cwd = cwd.to_string_lossy().into_owned();
        if *self.last_known_cwd.borrow() == cwd {
            return;
        }
        self.last_known_cwd.borrow_mut().clone_from(&cwd);
        self.emit_event(AgentEvent::CwdChanged { cwd: cwd.clone() });
        self.broadcast_agent_event(&AgentEvent::CwdChanged { cwd });
    }

    /// Fan an `AgentEvent` out to every event subscriber whose filter admits
    /// it. The subscriber registry and the wire id stamped on the frame are
    /// the core's; this engine only decides *when* an event happens.
    pub(super) fn broadcast_agent_event(&self, event: &AgentEvent) {
        self.core.fan_out_event(event);
    }

    /// Handle a supervisory [`ControlRequest`] (ADR-0033): a lease-change
    /// broadcast or a process signal. The input lease lives in `ServerState`;
    /// this actor is the emitter (it owns the event-subscriber list and the
    /// lifecycle) and the signal deliverer (it owns the PTY child pid).
    pub(super) fn handle_control_request(&mut self, req: ControlRequest) {
        match req {
            ControlRequest::LeaseChanged {
                input_holder,
                action,
                actor,
            } => {
                self.emit_terminal_control(action, input_holder, Some(actor), None);
            }
            ControlRequest::AgentRecordInvalidated => {
                if let Some(detector) = self.agent_detect.as_mut() {
                    detector.invalidate_published();
                }
            }
            ControlRequest::ReportAgentState { state, reply } => {
                let state = match state {
                    phux_protocol::wire::frame::ReportedAgentState::Working => {
                        crate::agent_detect::DetectedState::Working
                    }
                    phux_protocol::wire::frame::ReportedAgentState::Blocked => {
                        crate::agent_detect::DetectedState::Blocked
                    }
                    phux_protocol::wire::frame::ReportedAgentState::Done => {
                        crate::agent_detect::DetectedState::Done
                    }
                };
                let result = self
                    .agent_detect
                    .as_mut()
                    .ok_or_else(|| "agent detection is unavailable for this pane".to_owned())
                    .map(|detector| detector.report_hook_state(state, std::time::Instant::now()));
                match result {
                    Ok(report) => {
                        if let Some(report) = report {
                            self.emit_agent_state(AgentDetectEvent::State(report));
                        }
                        // Force one normal derivation after the edge so hook
                        // evidence never becomes a latch on a quiet screen.
                        self.agent_dirty_since_detect = true;
                        let _ = reply.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            ControlRequest::Signal {
                signal,
                input_holder,
                by,
                reply,
            } => {
                let result = self.deliver_signal(signal);
                if result.is_ok() {
                    // Reflect the reversible brake in the lifecycle the next
                    // broadcast reports. Terminating signals leave it
                    // `Running` until the EOF path fires `PaneClosed`.
                    match signal {
                        TerminalSignal::Freeze => self.lifecycle = TerminalLifecycle::Frozen,
                        TerminalSignal::Resume => self.lifecycle = TerminalLifecycle::Running,
                        TerminalSignal::Interrupt
                        | TerminalSignal::Terminate
                        | TerminalSignal::Kill => {}
                    }
                    let action = match signal {
                        TerminalSignal::Interrupt => ControlAction::Interrupted,
                        TerminalSignal::Freeze => ControlAction::Frozen,
                        TerminalSignal::Resume => ControlAction::Resumed,
                        TerminalSignal::Terminate => ControlAction::Terminated,
                        TerminalSignal::Kill => ControlAction::Killed,
                    };
                    self.emit_terminal_control(action, input_holder, Some(by), None);
                }
                let _ = reply.send(result);
            }
        }
    }

    /// Deliver a POSIX signal to the pane's process group (ADR-0033).
    ///
    /// `portable_pty` spawns the child as a session/process-group leader (it
    /// calls `setsid` + `TIOCSCTTY` to give the PTY a controlling terminal),
    /// so the child's pid *is* its process-group id. Signaling the group
    /// (`killpg`) reaches the child and every subprocess it spawned — the
    /// agent and all its descendants — which is what "freeze/kill the agent"
    /// must mean.
    pub(super) fn deliver_signal(&self, signal: TerminalSignal) -> Result<(), String> {
        use nix::sys::signal::{Signal as NixSignal, killpg};
        use nix::unistd::Pid;

        let pid = self
            .pty
            .as_ref()
            .and_then(|p| p.child.process_id())
            .and_then(|id| i32::try_from(id).ok())
            .ok_or_else(|| "no PTY child to signal".to_owned())?;

        let nix_signal = match signal {
            TerminalSignal::Interrupt => NixSignal::SIGINT,
            TerminalSignal::Freeze => NixSignal::SIGSTOP,
            TerminalSignal::Resume => NixSignal::SIGCONT,
            TerminalSignal::Terminate => NixSignal::SIGTERM,
            TerminalSignal::Kill => NixSignal::SIGKILL,
        };

        killpg(Pid::from_raw(pid), nix_signal).map_err(|err| format!("killpg failed: {err}"))
    }

    /// Build and broadcast an [`AgentEvent::TerminalControl`] (ADR-0033)
    /// stamped with this actor's current lifecycle.
    pub(super) fn emit_terminal_control(
        &self,
        action: ControlAction,
        input_holder: Option<phux_protocol::ClientId>,
        actor: Option<phux_protocol::ClientId>,
        exit_status: Option<i32>,
    ) {
        self.broadcast_agent_event(&AgentEvent::TerminalControl {
            lifecycle: self.lifecycle,
            exit_status,
            input_holder,
            action,
            actor,
        });
    }
}
