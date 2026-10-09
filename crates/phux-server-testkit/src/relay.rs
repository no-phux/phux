//! Reusable in-process production relay harness for connector tests.
//!
//! Owns bind-before-serve startup, resolved ephemeral addresses, and clean
//! shutdown so tests can deterministically drop and restart a relay without
//! shelling out or racing a port probe.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use phux_relay::{BoundRelay, RelayConfig, RelayRuntime};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// One serving relay and its deterministic shutdown handles.
pub struct RelayHarness {
    /// Resolved relay listen address (including an OS-assigned port).
    pub addr: SocketAddr,
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<(), phux_relay::RelayError>>,
}

impl RelayHarness {
    /// Bind and begin serving one relay inside the caller's tokio runtime.
    pub fn start(config: RelayConfig) -> Self {
        Self::spawn(RelayRuntime::new(config))
    }

    /// Like [`Self::start`], and append each consumer SNI rustls accepted.
    pub fn start_observing(config: RelayConfig, observed: Arc<Mutex<Vec<String>>>) -> Self {
        Self::spawn(RelayRuntime::new(config).with_observed_consumer_sni(observed))
    }

    fn spawn(runtime: RelayRuntime) -> Self {
        let bound: BoundRelay = runtime.bind().expect("bind relay");
        let addr = bound.local_addr();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            bound
                .serve(async move {
                    let _ = stopped.await;
                })
                .await
        });
        Self { addr, stop, task }
    }

    /// Stop serving and wait until the UDP socket is released.
    pub async fn stop(self) {
        let _ = self.stop.send(());
        self.task
            .await
            .expect("relay task panicked")
            .expect("relay shutdown");
    }
}
