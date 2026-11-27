//! Payload-free attempt phases for an embedder's opt-in tracing subscriber.
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use tokio::time::Instant;

pub(super) use super::AttemptTrace;

static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);
/// The driver and the UI thread both record; one at a time keeps every
/// attempt's rows in clock order on disk.
static ORDER: Mutex<()> = Mutex::new(());

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

    /// The attempt this task is recording for, if any.
    pub(crate) fn current() -> Option<Self> {
        CURRENT.try_with(|current| *current).ok()
    }

    pub(crate) fn record(&self, phase: &'static str) {
        let _order = ORDER.lock().unwrap_or_else(PoisonError::into_inner);
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
    reason = "native runtime consumers share this private diagnostic target"
)]
pub(crate) fn phase(phase: &'static str) {
    let _ = CURRENT.try_with(|current| current.record(phase));
}

/// Translate the dial module's closed vocabulary without carrying dial data.
pub(super) fn ws_phase(observed: phux_dial::ws::WsPhase) {
    use phux_dial::ws::WsPhase;
    phase(match observed {
        WsPhase::Resolve => "ws_resolve",
        WsPhase::TcpConnect => "ws_tcp_connect",
        WsPhase::TlsConfig => "ws_tls_config",
        WsPhase::TlsHandshake => "ws_tls_handshake",
        WsPhase::Upgrade => "ws_upgrade",
        WsPhase::Upgraded => "ws_upgraded",
    });
}
