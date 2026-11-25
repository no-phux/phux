//! A source's exact admission, predecessor fence and publication lifetime.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use phux_core::ids::ResourceId;
use phux_protocol::ids::BootstrapId;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::{CancellationToken, DropGuard};

use super::StreamHost;
use crate::state::Outbound;

#[derive(Debug, Clone)]
pub(crate) struct StreamLease {
    pub(crate) resource: ResourceId,
    pub(crate) bootstrap: BootstrapId,
    pub(crate) last_seq: Arc<AtomicU64>,
    pub(crate) cancellation: CancellationToken,
    host: StreamHost,
    connection: CancellationToken,
}

#[derive(Debug)]
pub(crate) struct StreamReservation {
    pub(crate) lease: StreamLease,
    pub(crate) prior: Option<(BootstrapId, Arc<AtomicU64>)>,
    pending: DropGuard,
    _completed: DropGuard,
}

impl StreamReservation {
    pub(super) fn new(host: StreamHost, resource: ResourceId, bootstrap: BootstrapId, connection: &CancellationToken, admitted: super::ReplacedPump) -> Self {
        let (cancellation, source_done, last_seq, prior) = admitted;
        Self {
            pending: cancellation.clone().drop_guard(), _completed: source_done.drop_guard(), prior,
            lease: StreamLease { resource, bootstrap, last_seq, cancellation, host, connection: connection.clone() },
        }
    }

    /// Every awaited fence remains in the host table until actually observed;
    /// superseding this source never loses a predecessor's completion.
    pub(crate) async fn ready(&self) -> bool {
        loop {
            let next = self.lease.host.state.with_mut(|s| {
                if !s.resources().pump_is_current(self.lease.host.client, self.lease.resource, &self.lease.last_seq) { return Err(()); }
                Ok(s.resources_mut().retiring_pump(self.lease.host.client, self.lease.resource, &self.lease.last_seq))
            });
            match next {
                Err(()) => return false,
                Ok(None) => return !self.lease.connection.is_cancelled(),
                Ok(Some(done)) => {
                    if self.lease.run(done.cancelled()).await.is_none() { return false; }
                }
            }
        }
    }

    /// No await separates the current-generation check from ownership handoff.
    pub(crate) fn published(self, handoff: impl FnOnce() -> bool) -> bool {
        let current = self.lease.host.state.with_mut(|s| {
            if self.lease.connection.is_cancelled() || !s.resources().pump_is_current(self.lease.host.client, self.lease.resource, &self.lease.last_seq) { return None; }
            if !handoff() || !s.resources_mut().publish_pump(self.lease.host.client, self.lease.resource, &self.lease.last_seq) { return None; }
            Some(s.resources().source_has_emitter(self.lease.host.client, self.lease.resource, &self.lease.last_seq))
        });
        if let Some(emitter_owns_close) = current { self.committed(emitter_owns_close); true } else { false }
    }

    pub(super) fn committed(self, emitter_owns_close: bool) {
        let _ = self.pending.disarm();
        if emitter_owns_close { let _ = self._completed.disarm(); }
    }
}

impl StreamLease {
    pub(crate) fn advance(&self) -> Option<BootstrapId> { self.host.advance(self.resource, &self.last_seq) }

    pub(crate) async fn run<F: Future>(&self, future: F) -> Option<F::Output> {
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => None,
            result = future => Some(result),
        }
    }

    /// Capacity is awaited off-lock; a stale permit cannot publish a frame.
    pub(crate) async fn queue(&self, out: &mpsc::Sender<Outbound>, frame: phux_protocol::wire::frame::FrameKind) -> bool {
        let Some(Ok(permit)) = self.run(out.reserve()).await else { return false; };
        self.host.state.with(|s| {
            if self.connection.is_cancelled() || !s.resources().pump_is_current(self.host.client, self.resource, &self.last_seq) { return false; }
            permit.send(Outbound::Frame(frame));
            true
        })
    }

    /// Fitting bootstraps retain contiguous mailbox admission without cloning
    /// payload frames. Oversized bounded runs stream through the same fence.
    pub(crate) async fn queue_frames(&self, out: &mpsc::Sender<Outbound>, frames: Vec<phux_protocol::wire::frame::FrameKind>) -> bool {
        if frames.len() > out.max_capacity() {
            for frame in frames { if !self.queue(out, frame).await { return false; } }
            return true;
        }
        let Some(Ok(mut permits)) = self.run(out.reserve_many(frames.len())).await else { return false; };
        self.host.state.with(|s| {
            if self.connection.is_cancelled() || !s.resources().pump_is_current(self.host.client, self.resource, &self.last_seq) { return false; }
            for frame in frames {
                let Some(permit) = permits.next() else { return false; };
                permit.send(Outbound::Frame(frame));
            }
            true
        })
    }

    pub(crate) fn spawn(&self, tasks: Option<&mut JoinSet<()>>, drain: Option<CancellationToken>, future: impl Future<Output = ()> + 'static) -> bool {
        let Some(source_done) = self.host.state.with(|s| s.resources().prepare_output_task(self.host.client, self.resource, &self.last_seq, drain.as_ref())) else { return false; };
        let source_guard = source_done.drop_guard();
        let done = CancellationToken::new();
        let guard = done.clone().drop_guard();
        let lease = self.clone();
        let lifetime = self.cancellation.clone().drop_guard();
        let task = async move {
            let _source = source_guard;
            let _done = guard;
            let _lifetime = lifetime;
            let _ = lease.run(future).await;
        };
        let abort = match tasks {
            Some(tasks) => {
                while tasks.try_join_next().is_some() {}
                tasks.spawn_local(task)
            }
            None => tokio::task::spawn_local(task).abort_handle(),
        };
        self.host.state.with_mut(|s| {
            s.resources_mut().track_reserved_output(self.host.client, self.resource, &self.last_seq, abort, done, drain)
        })
    }

    /// Retire exactly the stream whose transport ended, even after an awaited
    /// writer/error drain. A later control or QUIC attach is not its target.
    pub(crate) async fn detach(&self, wire: &phux_protocol::ids::ResourceId) {
        let completions = self.host.state.with_mut(|s| {
            if !s.resources().pump_matches(self.host.client, self.resource, &self.last_seq) { return Vec::new(); }
            s.unsubscribe_resource_events(self.host.client, wire);
            s.resources_mut().unsubscribe(self.host.client, self.resource);
            s.resources_mut().cancel_pump(self.host.client, self.resource);
            s.resources_mut().stop_output_pumps(self.host.client, self.resource)
        });
        for done in completions { done.cancelled().await; }
    }

    pub(crate) fn abort_connection(&self) -> bool {
        let detached = self.host.state.with_mut(|s| {
            if !s.resources().pump_is_current(self.host.client, self.resource, &self.last_seq) || self.connection.is_cancelled() { return None; }
            let from = crate::runtime::connections::detach_consumer_state(s, self.host.client);
            self.connection.cancel();
            Some(from)
        });
        if let Some(from) = detached {
            crate::runtime::connections::fire_client_detached(&self.host.state, self.host.client, from);
            true
        } else { false }
    }

    /// Creation cleanup may run after cancellation, but never after replacement.
    pub(crate) fn reap_spawn(&self, wire: &phux_protocol::ids::ResourceId, key: Option<phux_protocol::ids::IdempotencyKey>) {
        self.host.state.with_mut(|s| {
            if !s.resources().pump_matches(self.host.client, self.resource, &self.last_seq) { return; }
            crate::runtime::connections::reap_pane_journaling_close(s, self.resource);
            crate::runtime::idempotent_create::unbind_spawned(s, key, wire);
        });
    }

    pub(crate) fn retire_spawn_fault(&self, fault: crate::runtime::apps::terminal::attach::PumpFault) {
        use crate::runtime::apps::terminal::attach::PumpFault;
        fault.record();
        match fault {
            PumpFault::OutboundClosed | PumpFault::TombstoneNotQueued | PumpFault::ReplayAbandoned | PumpFault::PaneGone => {},
            PumpFault::GenerationLost => {
                self.host.state.with_mut(|s| {
                    if s.resources().pump_is_current(self.host.client, self.resource, &self.last_seq)
                        && crate::runtime::connections::reap_pane_journaling_close(s, self.resource) {
                        self.connection.cancel();
                    }
                });
            },
            PumpFault::PublicationNotActivated => { self.abort_connection(); },
        }
    }

    pub(crate) fn retire_terminal_fault(&self, fault: crate::runtime::apps::terminal::attach::PumpFault) {
        use crate::runtime::apps::terminal::attach::PumpFault;
        fault.record();
        match fault {
            PumpFault::OutboundClosed | PumpFault::ReplayAbandoned | PumpFault::PaneGone => {}
            PumpFault::TombstoneNotQueued | PumpFault::GenerationLost | PumpFault::PublicationNotActivated => { self.abort_connection(); }
        }
    }
}
