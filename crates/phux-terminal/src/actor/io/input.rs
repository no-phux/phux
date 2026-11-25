use super::{EncodedInputRequest, SnapshotBytes, TerminalActor, TerminalInput, debug, trace, warn};

impl TerminalActor {
    /// Synthesize an ATTACH snapshot, optionally priming scrollback (see
    /// [`crate::grid::SnapshotSynthesizer::synthesize_with_scrollback`]).
    pub(in crate::actor) fn synthesize_with_scrollback(
        &self,
        scrollback: Option<u32>,
    ) -> Result<SnapshotBytes, crate::grid::SynthesisError> {
        let _timer = crate::perf::BOOTSTRAP_SYNTH.timer();
        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            return Err(crate::grid::SynthesisError::TerminalUnavailable);
        };
        // A full snapshot uses a fresh render state; a shared borrow suffices.
        let synth = self.synth.borrow();
        synth.synthesize_with_scrollback(terminal, scrollback)
    }

    pub(in crate::actor) fn synthesize_with_scrollback_bounded(
        &self,
        scrollback: Option<u32>,
        max_bytes: usize,
        image_protocols: phux_protocol::caps::ImageProtocolSet,
    ) -> Result<SnapshotBytes, crate::grid::SynthesisError> {
        let _timer = crate::perf::BOOTSTRAP_SYNTH.timer();
        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            return Err(crate::grid::SynthesisError::TerminalUnavailable);
        };
        let synth = self.synth.borrow();
        synth.synthesize_with_scrollback_bounded(terminal, scrollback, max_bytes, image_protocols)
    }

    /// Project the grid into a [`phux_core::screen::ScreenState`] for
    /// `GET_SCREEN`, stamping `pane`. `format` also renders through
    /// libghostty's Formatter (`0` none, `1` HTML, `2` VT).
    ///
    /// A render failure is non-fatal (`rendered_error` says why), except
    /// [`SynthesisError::RenderBudgetExceeded`], which propagates so the
    /// caller refuses with `RESOURCE_EXHAUSTED`. With a rendering the plain
    /// text fields are omitted (the capture carries the same content).
    pub(in crate::actor) fn screen_state(
        &self,
        pane: u32,
        scrollback: Option<u32>,
        cells: bool,
        format: u8,
    ) -> Result<phux_core::screen::ScreenState, crate::grid::SynthesisError> {
        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            return Err(crate::grid::SynthesisError::TerminalUnavailable);
        };
        // Exclusive: the projection walks the tick's pooled render state.
        let mut synth = self.synth.borrow_mut();
        let mut screen = synth.screen_state_with_scrollback(terminal, pane, scrollback, cells)?;
        match synth.render_screen(terminal, scrollback, format) {
            Ok(rendered) => screen.rendered = rendered,
            Err(err @ crate::grid::SynthesisError::RenderBudgetExceeded { .. }) => return Err(err),
            Err(err) => {
                warn!(
                    error = %err,
                    format,
                    "GET_SCREEN rendered-capture failed; keeping the plain projection"
                );
                screen.rendered_error = Some(err.to_string());
            }
        }
        let selector = format & phux_protocol::wire::frame::GET_SCREEN_FORMAT_SELECTOR_MASK;
        if selector != 0 {
            screen.lines = Vec::new();
            screen.scrollback = Vec::new();
            screen.soft_wrap = None;
            screen.truncated = false;
            screen.truncated_reason = None;
        }
        Ok(screen)
    }

    /// Publish the complete terminal-derived encoder state after a terminal
    /// mutation. Capture failures retain the previous good snapshot.
    pub(in crate::actor) fn publish_input_snapshot(&self) {
        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            // On loan to a capture; the resync after its return covers it.
            trace!("input snapshot skipped: canonical terminal is on loan");
            return;
        };
        self.process.publish_input_snapshot(terminal, self.cell_px);
    }

    /// Encode a [`TerminalInput`] into PTY bytes. `Ok(None)` means
    /// deliberately dropped (focus reports off, a rejected paste); `Err` is an
    /// encoder failure the caller logs.
    pub(in crate::actor) fn encode_input(
        &self,
        input: &TerminalInput,
    ) -> Result<Option<Vec<u8>>, libghostty_vt::Error> {
        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            // Encoding reads terminal modes, so drop while on loan; the gated
            // input arms keep this off the production path.
            trace!("input dropped: canonical terminal is on loan to a capture");
            return Ok(None);
        };
        self.process.encode_input(input, terminal, self.cell_px)
    }

    /// Encode one input event and forward it to the PTY writer. Failures
    /// log and drop; one bad event must not kill the actor.
    pub(in crate::actor) fn service_input(&self, input: &TerminalInput) {
        // Log every outcome: ROUTE_INPUT acks regardless, so this is the only
        // witness when a key vanishes.
        match self.encode_input(input) {
            Ok(Some(bytes)) => {
                if bytes.is_empty() {
                    debug!(?input, "input encoded to zero bytes; nothing to write");
                    return;
                }
                self.service_encoded_input(EncodedInputRequest::legacy_probe(
                    bytes,
                    super::super::echo_probe_for(input),
                ));
            }
            Ok(None) => {
                debug!(?input, "input gated/dropped by encoder");
            }
            Err(err) => {
                warn!(error = %err, "input encode failed; dropping event");
            }
        }
    }

    /// Forward lane input through the process-owned admission gate.
    pub(in crate::actor) fn service_encoded_input(&self, request: EncodedInputRequest) {
        self.process.submit(request);
    }
}
