//! The coalescing resize channel: [`ResizeSender`], [`ResizeReceiver`].

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

/// A resize request.
///
/// `resync_clients` re-broadcasts a snapshot after a live reflow (clients
/// reflow independently and may diverge); it is `false` for the attach-time
/// resize, whose handshake snapshot already covers it.
#[derive(Debug, Clone, Copy)]
pub struct ResizeRequest {
    /// New grid width in cells.
    pub cols: u16,
    /// New grid height in cells.
    pub rows: u16,
    /// Donor cell size in pixels. `None` keeps the last-known size, so a
    /// pixel-less resize cannot zero established geometry.
    pub cell_px: Option<(u16, u16)>,
    /// Re-broadcast a snapshot after the reflow.
    pub resync_clients: bool,
    /// Skip the resize and only schedule a resync (a lagged pump's
    /// request); geometry fields are ignored.
    pub resync_only: bool,
    /// With `resync_only`, the one pump owed the resync; `None` means
    /// everyone.
    pub resync_for: Option<ResyncTarget>,
}

/// Latest geometry plus a bounded, lossless-under-backpressure recovery queue.
///
/// Geometry is state, not a work item: replacing it must never lose the final
/// size, pixel donor, or an already-owed live resync. Gap recovery remains
/// ordered and bounded independently, including its target generation.
#[derive(Debug, Clone)]
pub struct ResizeSender {
    geometry: std::sync::Arc<std::sync::Mutex<Option<ResizeRequest>>>,
    wake: mpsc::Sender<()>,
    recovery: mpsc::Sender<ResizeRequest>,
}

/// Actor-side receiver for [`ResizeSender`].
#[derive(Debug)]
pub struct ResizeReceiver {
    geometry: std::sync::Arc<std::sync::Mutex<Option<ResizeRequest>>>,
    wake: mpsc::Receiver<()>,
    recovery: mpsc::Receiver<ResizeRequest>,
}

impl ResizeSender {
    /// Keep at most one pending geometry and `capacity` recovery requests.
    #[must_use]
    pub fn channel(capacity: usize) -> (Self, ResizeReceiver) {
        let geometry = std::sync::Arc::new(std::sync::Mutex::new(None));
        let (wake, wake_rx) = mpsc::channel(1);
        let (recovery, recovery_rx) = mpsc::channel(capacity);
        (
            Self {
                geometry: geometry.clone(),
                wake,
                recovery,
            },
            ResizeReceiver {
                geometry,
                wake: wake_rx,
                recovery: recovery_rx,
            },
        )
    }

    /// Geometry never fails for lack of capacity. Recovery callers retain
    /// the ordinary bounded-mailbox contract.
    pub fn try_send(
        &self,
        mut request: ResizeRequest,
    ) -> Result<(), mpsc::error::TrySendError<ResizeRequest>> {
        if request.resync_only {
            return self.recovery.try_send(request);
        }
        let mut pending = self
            .geometry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.wake.try_send(()) == Err(mpsc::error::TrySendError::Closed(())) {
            return Err(mpsc::error::TrySendError::Closed(request));
        }
        if let Some(previous) = pending.as_ref() {
            request.cell_px = request.cell_px.or(previous.cell_px);
            request.resync_clients |= previous.resync_clients;
        }
        *pending = Some(request);
        drop(pending);
        Ok(())
    }

    /// Await room only for recovery; live geometry is coalesced immediately.
    pub async fn send(
        &self,
        request: ResizeRequest,
    ) -> Result<(), mpsc::error::SendError<ResizeRequest>> {
        if request.resync_only {
            return self.recovery.send(request).await;
        }
        self.try_send(request)
            .map_err(|error| mpsc::error::SendError(error.into_inner()))
    }

    /// Whether the actor has dropped its receiver.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.wake.is_closed()
    }
}

impl ResizeReceiver {
    fn take_geometry(&self) -> Option<ResizeRequest> {
        self.geometry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Geometry precedes recovery so the replacement cut has the latest size.
    pub async fn recv(&mut self) -> Option<ResizeRequest> {
        loop {
            tokio::select! {
                biased;
                Some(()) = self.wake.recv() => {
                    if let Some(request) = self.take_geometry() {
                        return Some(request);
                    }
                }
                Some(request) = self.recovery.recv() => return Some(request),
                else => return None,
            }
        }
    }

    /// Drain geometry first, retaining every queued targeted recovery.
    pub fn try_recv(&mut self) -> Result<ResizeRequest, mpsc::error::TryRecvError> {
        while self.wake.try_recv().is_ok() {
            if let Some(request) = self.take_geometry() {
                return Ok(request);
            }
        }
        self.recovery.try_recv()
    }
}
