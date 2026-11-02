//! Agent-event sourcing and supervisory control for [`TerminalActor`]:
//! event sinks and subscribers, the agent-state detector tick, events
//! sourced from engine effects (OSC 133, bell, OSC 7501), dirty/idle coalescing, cwd tracking, and signal
//! delivery (ADR-0033, ADR-0046).

use super::{
    AgentDetectEvent, AgentEvent, AskMarker, ControlAction, ControlRequest, DetectOutcome,
    ResourceLifecycle, TerminalActor, TerminalSignal, effects, mpsc, trace,
};
use crate::agent_detect::DetectedState;
use phux_resource::agent_asked::AskedPayload;

impl TerminalActor {
    /// Wire the agent-event sink (SPEC §7.5); the runtime drains it into the
    /// event journal (ADR-0123). Set before `spawn_local`.
    pub fn set_event_sink(&mut self, sink: impl Into<phux_resource::event_sink::EventSink>) {
        self.event_sink = Some(sink.into());
    }

    /// Emit one agent event without blocking. A full sink drops and counts,
    /// journaled as a `source_gap`.
    pub(super) fn emit_event(&self, event: AgentEvent) {
        if let Some(sink) = self.event_sink.as_ref() {
            sink.emit(event);
        }
    }

    /// Emit a signal's `terminal_control`, attributed to the client that
    /// sent it and to its `operation_id` when keyed (L1 §5.1.1, §7.3).
    pub(super) fn emit_signal_control(
        &self,
        action: ControlAction,
        input_holder: Option<phux_protocol::ClientId>,
        by: phux_protocol::ClientId,
        operation_id: Option<phux_protocol::ids::IdempotencyKey>,
    ) {
        let event = AgentEvent::TerminalControl {
            lifecycle: self.lifecycle,
            exit_status: None,
            input_holder,
            action,
            actor: Some(by),
        };
        if let Some(sink) = self.event_sink.as_ref() {
            sink.emit_keyed(event, operation_id);
        }
    }

    /// Wire the detector's sink (ADR-0046), drained by
    /// `spawn_agent_state_drain`. Crate-private: [`AgentDetectEvent`] is not
    /// a wire type.
    pub fn set_agent_state_sink(&mut self, sink: mpsc::Sender<AgentDetectEvent>) {
        self.agent_state_sink = Some(sink);
    }

    /// Whether this actor drives a PTY child, the precondition for an agent
    /// detector (a no-PTY pane has no foreground process to identify).
    pub const fn has_pty(&self) -> bool {
        self.pty.is_some()
    }

    /// Subscribe to authoritative OSC 7501 state before starting this actor.
    pub fn subscribe_program_status(
        &mut self,
    ) -> tokio::sync::watch::Receiver<Vec<super::program_status::Record>> {
        let (tx, rx) = tokio::sync::watch::channel(self.program_status.snapshot());
        self.program_status_sink = Some(tx);
        rx
    }

    pub(super) fn publish_program_status(&self) {
        if let Some(sink) = &self.program_status_sink {
            sink.send_replace(self.program_status.snapshot());
        }
    }

    /// Keep only the program status a seed write reported. An upgrade seed
    /// carries the old image's reports after its grid replay.
    pub(super) fn restore_program_status(&mut self) {
        for effect in self.effects.take() {
            if let effects::Effect::ProgramStatus(update) = effect {
                self.program_status.apply(update);
            }
        }
    }

    /// Wire the live-`AgentSession`-child probe (ADR-0103 §5); it depends on
    /// `ServerState`, which the pane never holds.
    pub fn set_live_session_probe(
        &mut self,
        probe: crate::agent_detect::live_session::LiveSessionProbe,
    ) {
        self.live_session_probe = Some(probe);
    }

    /// Bind the producer channel of the `AgentSession` child just spawned
    /// under this Terminal (ADR-0103 §6). The latest session wins.
    pub(crate) fn bind_agent_session(
        &mut self,
        append: mpsc::Sender<phux_resource::agent_session::AppendRequest>,
    ) {
        self.agent_session_append = Some(append);
    }

    /// Best-effort detector emission; dropping is safe because the detector
    /// re-derives every tick.
    pub(super) fn emit_agent_state(&self, event: AgentDetectEvent) {
        let _ = self.try_emit_agent_state(event);
    }

    pub(super) fn try_emit_agent_state(&self, event: AgentDetectEvent) -> bool {
        if let Some(sink) = self.agent_state_sink.as_ref() {
            return sink.try_send(event).is_ok();
        }
        false
    }

    /// The engine's OSC 0/2 title, or `None` when unset. Read from the
    /// engine rather than `last_title`, which only syncs on PTY output (a
    /// seeded or freshly resumed pane has an engine title before any chunk);
    /// falls back to `last_title` while the terminal is on loan.
    pub(super) fn live_osc_title(&self) -> Option<String> {
        let engine = self
            .terminal
            .borrow()
            .try_terminal()
            .and_then(|terminal| terminal.title().ok().map(ToOwned::to_owned));
        Some(engine.unwrap_or_else(|| self.last_title.clone())).filter(|t| !t.is_empty())
    }

    /// Sync `last_title` with libghostty's OSC 0/2 title; `true` on change.
    pub(super) fn refresh_title(&mut self) -> bool {
        let next: Option<String> = {
            let canonical = self.terminal.borrow();
            // On loan: the title cannot have changed without a deferred write.
            let current = canonical
                .try_terminal()
                .and_then(|terminal| terminal.title().ok())
                .unwrap_or("");
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

    /// Right-trimmed live-viewport rows: the detector's only grid read.
    /// Reads through the state-sync tick's pooled render state without
    /// clearing its dirty flags, so an agent pane's next tick stays
    /// incremental (phux-69pq.14).
    pub(super) fn viewport_lines(&self) -> Option<Vec<String>> {
        let canonical = self.terminal.borrow();
        let terminal = canonical.try_terminal()?;
        let _timer = phux_resource::perf::AGENT_VIEWPORT.timer();
        let mut synth = self.synth.borrow_mut();
        match synth.screen_state_with_scrollback(terminal, 0, None, false) {
            Ok(state) => Some(state.lines),
            Err(err) => {
                trace!(error = %err, "agent-detect: viewport read failed; skipping tick");
                None
            }
        }
    }

    /// One detector tick (ADR-0046); returns the interval to re-arm at. The
    /// detector is taken out of its `Option` for the borrow and always put
    /// back.
    pub(super) fn detect_tick(&mut self) -> Option<std::time::Duration> {
        let mut detector = self.agent_detect.take()?;
        let _timer = phux_resource::perf::AGENT_DETECT.timer();
        let now = std::time::Instant::now();
        // The detector's own dirty flag (the tick's is cleared far more
        // often). Consumed only by a scan that happened: an unidentified pane
        // skips scans, and eating the flag then lost the mutation that
        // painted the dialog. A failed projection keeps it set too.
        let dirty = self.agent_dirty_since_detect;
        let screen = if self.program_status.is_empty() && detector.wants_screen(dirty) {
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

    /// Test-only: write a PTY chunk and source its events, like the ingest
    /// path.
    #[cfg(test)]
    pub(super) fn feed_for_test(&mut self, chunk: &[u8]) {
        self.vt_write_for_test(chunk);
        self.source_events();
    }

    /// Source semantic events and state from the effects the last write
    /// queued.
    pub(super) fn source_events(&mut self) {
        let effects = self.effects.take();
        // Unconditional, even with no listener: the detector reads the title.
        let title_changed = self.refresh_title();
        self.observe_effects(&effects);
        if self.event_sink.is_none() {
            return;
        }
        self.output_since_idle_tick = true;
        // OSC 133 marks first so a command boundary precedes its burst.
        for effect in &effects {
            match effect {
                effects::Effect::CommandStart => self.emit_event(AgentEvent::CommandStarted),
                effects::Effect::CommandEnd { exit_code } => {
                    self.emit_event(AgentEvent::CommandFinished {
                        exit_code: *exit_code,
                    });
                    // Back at a prompt: any `cd` has landed.
                    self.check_cwd_changed();
                }
                effects::Effect::PromptStart
                | effects::Effect::InputStart
                | effects::Effect::Progress(_)
                | effects::Effect::ProgramStatus(_)
                | effects::Effect::Bell => {}
            }
        }
        if effects
            .iter()
            .any(|effect| matches!(effect, effects::Effect::Bell))
        {
            self.emit_event(AgentEvent::Bell);
        }
        if title_changed {
            self.emit_event(AgentEvent::TitleChanged {
                title: self.last_title.clone(),
            });
        }
        // `phux-ask` title sentinel (ADR-0036 tier 2). `AskedDetector` in
        // `ServerState` ranks and coalesces; `last_ask` only keeps a stable
        // title from reporting per chunk. Both edges ship (a clear matters).
        // Re-parse only on a title change or an owed retry.
        if title_changed || self.ask_retry_owed {
            self.source_ask_marker();
        }
        // One `dirty` per burst; `idle` from the tick closes it.
        if !self.in_output_burst {
            self.in_output_burst = true;
            self.emit_event(AgentEvent::Dirty);
        }
    }

    /// Fold effects in stream order; a prompt only expires active records.
    /// A full reset reaches the records as a root clear.
    fn observe_effects(&mut self, effects: &[effects::Effect]) {
        let mut changed = false;
        for effect in effects {
            self.prompt.observe(effect);
            match effect {
                effects::Effect::Progress(progress) => self.last_progress.clone_from(progress),
                effects::Effect::ProgramStatus(update) => {
                    changed |= self.program_status.apply(update.clone());
                }
                effects::Effect::PromptStart => {
                    changed |= self.program_status.expire_active();
                }
                _ => {}
            }
        }
        // A native cut can replay the child's last bytes after EOF was observed.
        if self.exit.is_some() {
            changed |= self.program_status.expire_active();
        }
        if changed {
            self.publish_program_status();
        }
    }

    /// Re-derive the ask marker from the title and ship the edge.
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
            // Waiting time is rendered client-side from receipt.
            elapsed_seconds: None,
        });
        // Advance the mirror only once the edge is in flight: asks are
        // edge-triggered, so a refused one must be retried.
        if self.try_emit_agent_state(AgentDetectEvent::AskSentinel(ask))
            || self.agent_state_sink.is_none()
        {
            self.last_ask = current_ask;
            self.ask_retry_owed = false;
        } else {
            self.ask_retry_owed = true;
        }
    }

    /// Emit `idle` on the first tick after a burst with no new output.
    pub(super) fn maybe_emit_idle(&mut self) {
        let had_output = std::mem::take(&mut self.output_since_idle_tick);
        if self.in_output_burst && !had_output {
            self.in_output_burst = false;
            self.emit_event(AgentEvent::Idle);
            // Settling is the fallback prompt boundary for shells without
            // OSC 133.
            self.check_cwd_changed();
        }
    }

    /// Re-query the child's kernel cwd and emit [`AgentEvent::CwdChanged`]
    /// on a change. Best-effort; the first successful observation always
    /// emits so the starting directory is announced.
    pub(super) fn check_cwd_changed(&self) {
        let Some(pid) = self.pty.as_ref().and_then(|p| p.child.process_id()) else {
            return;
        };
        let query_started = std::time::Instant::now();
        let cwd = phux_resource::cwd_query::process_cwd(pid);
        phux_resource::perf::PROC_CWD_QUERY.record_elapsed(query_started);
        let Some(cwd) = cwd else {
            return;
        };
        let cwd = cwd.to_string_lossy().into_owned();
        // First observation always announces (late consumers need it).
        let first_observation = !self.cwd_announced.replace(true);
        if !first_observation && *self.last_known_cwd.borrow() == cwd {
            return;
        }
        self.last_known_cwd.borrow_mut().clone_from(&cwd);
        self.emit_event(AgentEvent::CwdChanged { cwd });
    }
}

mod control;

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "re-export for siblings"
)]
use control::*;
