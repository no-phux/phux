//! Companions for surfaces only some kinds offer.
//!
//! A kind that offers one adds it with
//! [`ResourceHandle::with_companion`](crate::ResourceHandle::with_companion);
//! a handle without it answers as a resource that has no such surface.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{Delivery, LeaseChange};

/// An engine that takes a size in cells.
pub trait Resize: Send + Sync {
    /// Ask the engine to take `size`, returning whether it accepted.
    fn resize(&self, size: (u16, u16), cell_px: Option<(u16, u16)>) -> bool;
}

/// The companion of a resource sized in cells.
#[derive(Clone)]
pub struct Resizable(Arc<dyn Resize>);

impl Resizable {
    /// A companion that resizes through `engine`.
    pub fn new(engine: impl Resize + 'static) -> Self {
        Self(Arc::new(engine))
    }

    /// Ask the engine to take `size` in cells, returning whether it accepted.
    #[must_use]
    pub fn resize(&self, size: (u16, u16), cell_px: Option<(u16, u16)>) -> bool {
        self.0.resize(size, cell_px)
    }
}

impl std::fmt::Debug for Resizable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Resizable")
    }
}

/// An engine told when its input lease changes (ADR-0033).
pub trait LeaseSink: Send + Sync {
    /// Tell the engine its input lease changed, waiting for mailbox room.
    fn lease_changed(&self, change: LeaseChange) -> Delivery;

    /// [`Self::lease_changed`] for a caller that may not wait; a full
    /// mailbox drops the notice.
    fn try_lease_changed(&self, change: LeaseChange);
}

/// The companion of a resource with an input lease.
#[derive(Clone)]
pub struct Leasable(Arc<dyn LeaseSink>);

impl Leasable {
    /// A companion that tells `engine` about lease changes.
    pub fn new(engine: impl LeaseSink + 'static) -> Self {
        Self(Arc::new(engine))
    }

    /// Tell the engine its input lease changed, waiting for mailbox room.
    pub fn lease_changed(&self, change: LeaseChange) -> Delivery {
        self.0.lease_changed(change)
    }

    /// [`Self::lease_changed`] without waiting; a full mailbox drops it.
    pub fn try_lease_changed(&self, change: LeaseChange) {
        self.0.try_lease_changed(change);
    }
}

impl std::fmt::Debug for Leasable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Leasable")
    }
}

/// The companion of a record stream: whether it has ended yet.
#[derive(Debug, Clone)]
pub struct LiveStream {
    ended: Arc<AtomicBool>,
}

impl LiveStream {
    /// A stream that is live until its engine sets `ended`.
    #[must_use]
    pub const fn new(ended: Arc<AtomicBool>) -> Self {
        Self { ended }
    }

    /// Whether the stream has not ended.
    #[must_use]
    pub fn is_live(&self) -> bool {
        !self.ended.load(Ordering::Relaxed)
    }
}
