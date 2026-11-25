//! Native payload-free input evidence on the current driver's attempt clock.

impl super::Client {
    /// Record whether typed input reached the local queue, never its payload.
    pub fn trace_input(&self, injected: bool) {
        self.record_input_phase(if injected { "input_injected" } else { "input_refused" });
    }

    /// The qualification probe observed readiness and is about to type.
    pub fn trace_input_ready(&self) {
        self.record_input_phase("input_ready");
    }

    fn record_input_phase(&self, phase: &'static str) {
        let trace = *self.inner.trace.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(trace) = trace {
            trace.record(phase);
        }
    }
}
