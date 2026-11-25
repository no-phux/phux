//! Activate Terminal publications after the host's aggregate barrier.
#![allow(clippy::wildcard_imports, reason = "shared attach namespace")]

use super::*;

pub(super) async fn release_gates(
    lease: &crate::runtime::apps::StreamLease,
    gates: Vec<SnapshotGate>,
) -> Result<(), ()> {
    for gate in gates {
        let Some(cut) = gate.cut else {
            continue;
        };
        let mut start = OutputPumpStart {
            superseded: false,
            published_cut: cut,
            replay: Vec::new(),
            live: None,
            interest: None,
        };
        if let Some(capture) = gate.native_capture {
            let publication = match activate_native_publication(
                &gate.terminal,
                capture,
            )
            .await
            {
                Ok(publication) => publication,
                // The resource closed after capture: no generation can be
                // published, but RESOURCE_CLOSED ends it without an error.
                Err(failure) if failure.actor_gone() => continue,
                Err(_) => {
                    lease.abort_connection();
                    return Err(());
                }
            };
            start.adopt_native_publication(publication);
        }
        if gate.gate.send(start).is_err() {
            lease.abort_connection();
            return Err(());
        }
    }
    Ok(())
}

/// Captured before publication and retained until either the exact host lifetime
/// owns a tick consumer or the raw pump ends. Aborting an unread continuation
/// drops both its JoinSet and registration guards.
#[derive(Default)]
pub(super) struct TerminalPublication {
    pub(super) gates: Vec<SnapshotGate>,
    pub(super) pumps: JoinSet<()>,
    pub(super) registrations: Vec<phux_terminal_api::actor::ConsumerRegistrationGuard>,
}

impl TerminalPublication {
    pub(super) async fn run(
        mut self,
        lease: crate::runtime::apps::StreamLease,
    ) {
        if release_gates(&lease, self.gates).await.is_err() {
            return;
        }
        if self.pumps.is_empty() {
            // No task exists for tick consumers: their exact resource lease is
            // the published lifetime, and its actor-owned wait retires them.
            for registration in self.registrations {
                registration.published();
            }
        } else {
            // Raw consumers keep their exact guard until the real emitter ends.
            // No detach request or allocation is needed on this path.
            let _registrations = self.registrations;
            while self.pumps.join_next().await.is_some() {}
        }
    }
}
