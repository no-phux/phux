//! Fixed, payload-free enrollment boundaries for isolated native qualification.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static NEXT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    RequestReceived,
    WorkQueued,
    WorkStarted,
    Issued,
    Refused,
    WorkJoined,
    TaskFailed,
    DeliveryAcknowledged,
    DeliveryPeerStopped,
    DeliveryFailed,
    DeliveryDeadline,
}

#[derive(Clone, Copy)]
pub(super) struct Trace {
    enrollment: u64,
    started: Instant,
}

impl Trace {
    pub(super) fn new() -> Self {
        Self {
            enrollment: NEXT.fetch_add(1, Ordering::Relaxed),
            started: Instant::now(),
        }
    }

    pub(super) fn record(self, phase: Phase) {
        // A field-free root prevents inherited remote/credential fields from
        // entering these events, including in tracing's JSON formatter.
        let span = tracing::debug_span!(
            target: "phux_server::enrollment_diagnostics", parent: None, "enrollment_diagnostic"
        );
        let _entered = span.enter();
        tracing::debug!(
            target: "phux_server::enrollment_diagnostics", parent: &span,
            enrollment = self.enrollment,
            elapsed_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            ?phase
        );
    }
}

pub(super) const fn delivery_phase(
    result: &Result<Result<Option<quinn::VarInt>, ()>, tokio::time::error::Elapsed>,
) -> Phase {
    match result {
        Err(_) => Phase::DeliveryDeadline,
        Ok(Err(())) => Phase::DeliveryFailed,
        Ok(Ok(Some(_))) => Phase::DeliveryPeerStopped,
        Ok(Ok(None)) => Phase::DeliveryAcknowledged,
    }
}

#[cfg(test)]
mod tests;
