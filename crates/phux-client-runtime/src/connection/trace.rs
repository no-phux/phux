//! Payload-free attempt phases for an embedder's opt-in tracing subscriber.
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::time::Instant;

pub(super) use super::AttemptTrace;

static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);

tokio::task_local! {
    static CURRENT: AttemptTrace;
}

impl AttemptTrace {
    pub(crate) fn connection() -> u64 {
        NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn new(connection: u64, attempt: u32) -> Self {
        Self {
            connection,
            attempt,
            started: Instant::now(),
        }
    }

    pub(super) async fn scope<F: Future>(&self, future: F) -> F::Output {
        let current = Self {
            connection: self.connection,
            attempt: self.attempt,
            started: self.started,
        };
        CURRENT.scope(current, future).await
    }

    pub(crate) fn record(&self, phase: &'static str) {
        tracing::debug!(
            target: "phux_client_runtime::connection_diagnostics",
            connection = self.connection,
            attempt = self.attempt,
            elapsed_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            phase,
        );
    }
}

/// Only static phase names enter this target: never errors, addresses or frames.
#[allow(
    clippy::redundant_pub_crate,
    reason = "control responses use this private target; no public diagnostic API"
)]
pub(crate) fn phase(phase: &'static str) {
    let _ = CURRENT.try_with(|current| current.record(phase));
}
