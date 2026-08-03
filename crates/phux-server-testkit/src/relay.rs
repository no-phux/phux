//! Reusable in-process production relay harness for connector tests.
//!
//! Owns bind-before-serve startup, resolved ephemeral addresses, and clean
//! shutdown so tests can deterministically drop and restart a relay without
//! shelling out or racing a port probe.

use std::net::SocketAddr;

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
        let bound: BoundRelay = RelayRuntime::new(config).bind().expect("bind relay");
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
