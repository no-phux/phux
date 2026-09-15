//! The engine-to-runtime event sink, and the loss counter behind it
//! (ADR-0123 "no silent loss").
//!
//! An engine emits semantic events from its own task and must never stall
//! on them, so the sink is a bounded channel written with `try_send`. The
//! runtime drains it into the server-wide journal. The one place an event
//! can be lost before the journal is a full sink, so every such drop is
//! counted here; the drain reads the count and journals a `source_gap`
//! scoped to the resource, which turns a silent drop into a typed one.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use phux_protocol::wire::frame::AgentEvent;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

/// The engine's end: emits without blocking and counts what it had to drop.
#[derive(Debug, Clone)]
pub struct EventSink {
    tx: mpsc::Sender<AgentEvent>,
    dropped: Arc<AtomicU64>,
}

/// The runtime's end: the events in order, plus the drop count since the
/// last read.
#[derive(Debug)]
pub struct EventSource {
    rx: mpsc::Receiver<AgentEvent>,
    dropped: Arc<AtomicU64>,
}

/// A sink of `capacity` events and the source that drains it.
#[must_use]
pub fn event_sink(capacity: usize) -> (EventSink, EventSource) {
    let (tx, rx) = mpsc::channel(capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    (
        EventSink {
            tx,
            dropped: Arc::clone(&dropped),
        },
        EventSource { rx, dropped },
    )
}

impl EventSink {
    /// Queue `event`, or count it as dropped when the sink is full. A
    /// closed sink means nobody drains it any more, so there is nothing to
    /// report a loss to.
    pub fn emit(&self, event: AgentEvent) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(event) {
            // Relaxed: the engine and the drain share one current-thread
            // runtime (ADR-0014); the atomic only has to be shared.
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A bare channel is a sink whose drops nobody reads, which is what a test
/// that inspects the raw events wants.
impl From<mpsc::Sender<AgentEvent>> for EventSink {
    fn from(tx: mpsc::Sender<AgentEvent>) -> Self {
        Self {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl EventSource {
    /// The next event, or `None` once every sink is gone and the queue is
    /// empty.
    pub async fn recv(&mut self) -> Option<AgentEvent> {
        self.rx.recv().await
    }

    /// The next already-queued event, without waiting: how the exit watcher
    /// journals what a pane emitted before its close.
    pub fn try_recv(&mut self) -> Option<AgentEvent> {
        self.rx.try_recv().ok()
    }

    /// How many events were dropped since the last call, resetting the
    /// count.
    #[must_use]
    pub fn take_dropped(&self) -> u64 {
        self.dropped.swap(0, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_sink_counts_every_drop_once() {
        let (sink, mut source) = event_sink(1);
        sink.emit(AgentEvent::Bell);
        sink.emit(AgentEvent::Dirty);
        sink.emit(AgentEvent::Idle);
        assert_eq!(source.take_dropped(), 2);
        assert_eq!(source.take_dropped(), 0, "reading resets the count");
        assert!(matches!(source.rx.try_recv(), Ok(AgentEvent::Bell)));
    }

    #[test]
    fn a_closed_sink_is_not_a_loss() {
        let (sink, source) = event_sink(1);
        drop(source.rx);
        sink.emit(AgentEvent::Bell);
        assert_eq!(source.dropped.load(Ordering::Relaxed), 0);
    }
}
