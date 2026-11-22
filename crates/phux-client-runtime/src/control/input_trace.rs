//! Payload-free evidence of typed input, on the attempt clock.
impl super::ControlPlane {
    /// Record, payload-free, whether typed input reached the outbound queue
    /// (`input_injected`) or was turned away (`input_refused`).
    pub fn trace_input(&self, injected: bool) {
        if let Some(trace) = &self.attempt_trace {
            trace.record(if injected {
                "input_injected"
            } else {
                "input_refused"
            });
        }
    }

    /// The qualification probe observed `input_ready` and is about to type.
    /// Same attempt clock as [`Self::trace_input`], and nothing else changes.
    pub fn trace_input_ready(&self) {
        if let Some(trace) = &self.attempt_trace {
            trace.record("input_ready");
        }
    }

    pub(crate) const fn set_attempt_trace(
        &mut self,
        trace: Option<crate::connection::AttemptTrace>,
    ) {
        self.attempt_trace = trace;
    }
}
