//! Supervisory control: signals, hook state, and retirement.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;
use phux_agent_session::parent::AgentParentRequest;

impl TerminalActor {
    /// Handle a supervisory [`ControlRequest`] (ADR-0033): lease broadcasts,
    /// signals, and retirement.
    pub(in crate::actor) fn handle_control_request(&mut self, req: ControlRequest) {
        match req {
            ControlRequest::LeaseChanged(change) => {
                self.emit_terminal_control(change.action, change.input_holder, change.actor, None);
            }
            ControlRequest::Retire => {
                // Retiring stops the detector, so answer what is already
                // queued for it first: the mailboxes are separate, and a
                // report sent before the retire must still land.
                while let Ok(req) = self.agent_parent_rx.try_recv() {
                    self.handle_agent_parent_request(req);
                }
                self.retire_after_exit();
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

    /// Handle a request addressed to the agent running in this pane.
    pub(in crate::actor) fn handle_agent_parent_request(&mut self, req: AgentParentRequest) {
        match req {
            AgentParentRequest::RecordInvalidated => {
                if let Some(detector) = self.agent_detect.as_mut() {
                    detector.invalidate_published();
                }
                self.publish_program_status();
            }
            AgentParentRequest::ReportStreamState { state, reply } => {
                self.report_stream_state(state, reply);
            }
            AgentParentRequest::ReportState { state, reply } => {
                let _ = reply.send(self.apply_hook_state(hook_state(state)));
            }
            AgentParentRequest::BindSession { append } => self.bind_agent_session(append),
            AgentParentRequest::SynthesizeStateRecord { state, reply } => {
                let _ = reply.send(self.synthesize_state_record(hook_state(state)));
            }
        }
    }

    /// Feed a session child's state into the detector at the `Stream` rank.
    fn report_stream_state(
        &mut self,
        state: Option<phux_protocol::wire::frame::ReportedAgentState>,
        reply: phux_agent_session::parent::ReportReply,
    ) {
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
                self.publish_program_status();
                let _ = reply.send(Ok(()));
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    /// Deliver a POSIX signal to the pane's process group (ADR-0033). The
    /// child is a session leader, so `killpg` reaches it and every
    /// descendant.
    pub(in crate::actor) fn deliver_signal(&self, signal: TerminalSignal) -> Result<(), String> {
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
    pub(in crate::actor) fn apply_hook_state(
        &mut self,
        state: DetectedState,
    ) -> Result<(), String> {
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
    pub(in crate::actor) fn synthesize_state_record(
        &mut self,
        state: DetectedState,
    ) -> Result<(), String> {
        let record = format!(
            "{{\"type\":\"state\",\"data\":{{\"state\":\"{}\",\"source\":\"hook\"}}}}\n",
            state.as_str()
        );
        let (reply, _unread) = tokio::sync::oneshot::channel();
        let appended = self.agent_session_append.as_ref().is_some_and(|append| {
            append
                .try_send(phux_agent_session::session::AppendRequest {
                    bytes: bytes::Bytes::from(record),
                    reply,
                    publication: None,
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
    pub(in crate::actor) fn retire_after_exit(&mut self) {
        self.lifecycle = ResourceLifecycle::Exited;
        self.agent_detect = None;
        self.release_pty_after_exit();
    }

    /// Emit an [`AgentEvent::TerminalControl`] (ADR-0033) with the current
    /// lifecycle, attributed to `actor`.
    pub(in crate::actor) fn emit_terminal_control(
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
pub(in crate::actor) const fn hook_state(
    state: phux_protocol::wire::frame::ReportedAgentState,
) -> DetectedState {
    use phux_protocol::wire::frame::ReportedAgentState;
    match state {
        ReportedAgentState::Working => DetectedState::Working,
        ReportedAgentState::Blocked => DetectedState::Blocked,
        ReportedAgentState::Done => DetectedState::Done,
    }
}
