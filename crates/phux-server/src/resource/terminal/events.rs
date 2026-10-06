//! Agent-event sourcing and supervisory control for [`TerminalActor`]:
//! event sinks and subscribers, the agent-state detector tick, OSC-133
//! sourced events, dirty/idle coalescing, cwd tracking, and signal
//! delivery (ADR-0033, ADR-0046).

use super::{
    AgentDetectEvent, AgentEvent, AskMarker, ControlAction, ControlRequest, DetectOutcome,
    ResourceLifecycle, TerminalActor, TerminalSignal, mpsc, osc133, trace,
};
use crate::agent_asked::AskedPayload;
use crate::agent_detect::DetectedState;

impl TerminalActor {
    /// Wire the agent-event sink (SPEC §7.5); the runtime drains it into the
    /// event journal (ADR-0123). Set before `spawn_local`.
    pub fn set_event_sink(&mut self, sink: impl Into<crate::resource::event_sink::EventSink>) {
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
    pub(crate) fn set_agent_state_sink(&mut self, sink: mpsc::Sender<AgentDetectEvent>) {
        self.agent_state_sink = Some(sink);
    }

    /// Wire the live-`AgentSession`-child probe (ADR-0103 §5); it depends on
    /// `ServerState`, which the pane never holds.
    pub(crate) fn set_live_session_probe(
        &mut self,
        probe: crate::agent_detect::live_session::LiveSessionProbe,
    ) {
        self.live_session_probe = Some(probe);
    }

    /// Bind the producer channel of the `AgentSession` child just spawned
    /// under this Terminal (ADR-0103 §6). The latest session wins.
    pub(crate) fn bind_agent_session(
        &mut self,
        append: mpsc::Sender<crate::resource::agent_session::AppendRequest>,
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
        let _timer = crate::perf::AGENT_VIEWPORT.timer();
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
        let _timer = crate::perf::AGENT_DETECT.timer();
        let now = std::time::Instant::now();
        // The detector's own dirty flag (the tick's is cleared far more
        // often). Consumed only by a scan that happened: an unidentified pane
        // skips scans, and eating the flag then lost the mutation that
        // painted the dialog. A failed projection keeps it set too.
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

    /// Source agent events from a freshly written PTY chunk: `bell` (once per
    /// chunk), `title_changed`, one `dirty` per output burst, and OSC 133
    /// `command_started`/`command_finished` (with the `D` exit code the grid
    /// drops). Each `D` is also a prompt boundary that re-checks the cwd.
    pub(super) fn source_events_from_chunk(&mut self, chunk: &[u8]) {
        // Unconditional, even with no listener: the detector reads the title.
        let title_changed = self.refresh_title();
        let marks = self.osc133.feed(chunk);
        self.observe_marks(&marks);
        if self.event_sink.is_none() {
            return;
        }
        self.output_since_idle_tick = true;
        // OSC 133 marks first so a command boundary precedes its burst.
        for mark in marks {
            match mark {
                osc133::OscMark::CommandStart => self.emit_event(AgentEvent::CommandStarted),
                osc133::OscMark::CommandEnd { exit_code } => {
                    self.emit_event(AgentEvent::CommandFinished { exit_code });
                    // Back at a prompt: any `cd` has landed.
                    self.check_cwd_changed();
                }
                osc133::OscMark::PromptStart
                | osc133::OscMark::InputStart
                | osc133::OscMark::Progress(_) => {}
            }
        }
        if memchr::memchr(0x07, chunk).is_some() {
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

    /// Fold OSC marks into state kept regardless of listeners: the OSC 9;4
    /// progress mirror and the prompt machine.
    fn observe_marks(&mut self, marks: &[osc133::OscMark]) {
        for mark in marks {
            self.prompt.observe(mark);
            if let osc133::OscMark::Progress(progress) = mark {
                self.last_progress.clone_from(progress);
            }
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
        let cwd = crate::cwd_query::process_cwd(pid);
        crate::perf::PROC_CWD_QUERY.record_elapsed(query_started);
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

    /// Handle a supervisory [`ControlRequest`] (ADR-0033): lease broadcasts
    /// and signals.
    pub(super) fn handle_control_request(&mut self, req: ControlRequest) {
        match req {
            ControlRequest::LeaseChanged {
                input_holder,
                action,
                actor,
            } => {
                self.emit_terminal_control(action, input_holder, actor, None);
            }
            ControlRequest::AgentRecordInvalidated => {
                if let Some(detector) = self.agent_detect.as_mut() {
                    detector.invalidate_published();
                }
            }
            ControlRequest::ReportStreamState { state, reply } => {
                let state = state.map(|state| match state {
                    phux_protocol::wire::frame::ReportedAgentState::Working => {
                        crate::agent_detect::DetectedState::Working
                    }
                    phux_protocol::wire::frame::ReportedAgentState::Blocked => {
                        crate::agent_detect::DetectedState::Blocked
                    }
                    phux_protocol::wire::frame::ReportedAgentState::Done => {
                        crate::agent_detect::DetectedState::Done
                    }
                });
                let result = self
                    .agent_detect
                    .as_mut()
                    .ok_or_else(|| "agent detection is unavailable for this pane".to_owned())
                    .map(|detector| detector.report_stream_state(state, std::time::Instant::now()));
                match result {
                    Ok(report) => {
                        if let Some(report) = report {
                            self.emit_agent_state(AgentDetectEvent::State(report));
                        }
                        // A retraction hands the pane back to the screen.
                        self.agent_dirty_since_detect = true;
                        let _ = reply.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            ControlRequest::ReportAgentState { state, reply } => {
                let _ = reply.send(self.apply_hook_state(hook_state(state)));
            }
            ControlRequest::BindAgentSession { append } => self.bind_agent_session(append),
            ControlRequest::Retire => self.retire_after_exit(),
            ControlRequest::ReplaceChild { command, reply } => {
                let _ = reply.send(self.replace_child(command.0));
            }
            ControlRequest::SynthesizeAgentStateRecord { state, reply } => {
                let _ = reply.send(self.synthesize_state_record(hook_state(state)));
            }
            ControlRequest::Signal {
                signal,
                input_holder,
                by,
                operation_id,
                reply,
            } => {
                let result = self.deliver_signal(signal);
                if result.is_ok() {
                    // Only the reversible brake changes the lifecycle;
                    // terminal signals surface via EOF.
                    match signal {
                        TerminalSignal::Freeze => self.lifecycle = ResourceLifecycle::Frozen,
                        TerminalSignal::Resume => self.lifecycle = ResourceLifecycle::Running,
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
                    self.emit_signal_control(action, input_holder, by, operation_id);
                }
                let _ = reply.send(result);
            }
        }
    }

    /// Deliver a POSIX signal to the pane's process group (ADR-0033). The
    /// child is a session leader, so `killpg` reaches it and every
    /// descendant.
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

    /// Feed hook-reported state into the detector (ADR-0085), the path
    /// taken without a live `AgentSession` child. A swallowed edge is a
    /// success.
    pub(super) fn apply_hook_state(&mut self, state: DetectedState) -> Result<(), String> {
        let report = self
            .agent_detect
            .as_mut()
            .ok_or_else(|| "agent detection is unavailable for this pane".to_owned())?
            .report_hook_state(state, std::time::Instant::now());
        if let Some(report) = report {
            self.emit_agent_state(AgentDetectEvent::State(report));
        }
        // Force one derivation so the hook never latches on a quiet screen.
        self.agent_dirty_since_detect = true;
        Ok(())
    }

    /// Append a synthesized `{"type":"state","data":{"state":...,
    /// "source":"hook"}}` record to this Terminal's live `AgentSession` child
    /// (ADR-0103 §6).
    ///
    /// A missing or closed channel falls back to the ADR-0085 path rather
    /// than failing: a `blocked` report must reach the pane either way.
    pub(super) fn synthesize_state_record(&mut self, state: DetectedState) -> Result<(), String> {
        let record = format!(
            "{{\"type\":\"state\",\"data\":{{\"state\":\"{}\",\"source\":\"hook\"}}}}\n",
            state.as_str()
        );
        let (reply, _unread) = tokio::sync::oneshot::channel();
        let appended = self.agent_session_append.as_ref().is_some_and(|append| {
            append
                .try_send(crate::resource::agent_session::AppendRequest {
                    bytes: bytes::Bytes::from(record),
                    reply,
                })
                .is_ok()
        });
        if !appended {
            trace!(
                state = state.as_str(),
                "REPORT_AGENT_STATE: no agent-session stream to append to; using the detector path",
            );
            return self.apply_hook_state(state);
        }
        if let Some(detector) = self.agent_detect.as_mut()
            && let Some(report) =
                detector.report_stream_state(Some(state), std::time::Instant::now())
        {
            self.emit_agent_state(AgentDetectEvent::State(report));
        }
        // Still run one ordinary derivation behind it.
        self.agent_dirty_since_detect = true;
        Ok(())
    }

    /// The process exited and the pane is retained (ADR-0124): report
    /// `Exited`, stop the detector, release the PTY. Grid and consumers stay.
    pub(super) fn retire_after_exit(&mut self) {
        self.lifecycle = ResourceLifecycle::Exited;
        self.agent_detect = None;
        self.release_pty_after_exit();
    }

    /// Emit an [`AgentEvent::TerminalControl`] (ADR-0033) with the current
    /// lifecycle, attributed to `actor`.
    pub(super) fn emit_terminal_control(
        &self,
        action: ControlAction,
        input_holder: Option<phux_protocol::ClientId>,
        actor: Option<phux_protocol::ClientId>,
        exit_status: Option<i32>,
    ) {
        self.emit_event(AgentEvent::TerminalControl {
            lifecycle: self.lifecycle,
            exit_status,
            input_holder,
            action,
            actor,
        });
    }
}

/// Map a hook-reported state to the detector's. There is no `idle`: that is
/// the detector's call (ADR-0085).
const fn hook_state(state: phux_protocol::wire::frame::ReportedAgentState) -> DetectedState {
    use phux_protocol::wire::frame::ReportedAgentState;
    match state {
        ReportedAgentState::Working => DetectedState::Working,
        ReportedAgentState::Blocked => DetectedState::Blocked,
        ReportedAgentState::Done => DetectedState::Done,
    }
}
