//! One publication slot and the read/stale handshake that paces grid
//! projection, plus the owner's catch-up hook every slot shares.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock};

use super::{CatchUp, FrameKey, Published};

/// The catch-up the current owner installed, shared by the table and every
/// slot so a held [`TerminalPublication`] reaches the owner too.
///
/// [`TerminalPublication`]: super::TerminalPublication
#[derive(Default)]
pub(super) struct Hook(pub(super) RwLock<Option<CatchUp>>);

impl Hook {
    pub(super) fn get(&self) -> Option<CatchUp> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl std::fmt::Debug for Hook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hook").finish_non_exhaustive()
    }
}

/// One presentation's or document's slot: the generation counter a consumer
/// polls, what is published behind it, and the read/stale pair that paces
/// grid publication.
#[derive(Debug)]
pub(super) struct Slot {
    key: FrameKey,
    hook: Arc<Hook>,
    pub(super) generation: AtomicU64,
    /// A consumer acquired the current frame since it was published.
    pub(super) read: AtomicBool,
    /// The owner deferred a projection past the current frame; the next
    /// acquire catches up.
    pub(super) stale: AtomicBool,
    pub(super) frame: RwLock<Option<Published>>,
    /// Signalled after every publish and on removal, for
    /// [`TerminalPublication::wait_past`].
    ///
    /// [`TerminalPublication::wait_past`]: super::TerminalPublication::wait_past
    pub(super) changed: (Mutex<()>, Condvar),
}

impl Slot {
    pub(super) const fn new(key: FrameKey, hook: Arc<Hook>) -> Self {
        Self {
            key,
            hook,
            generation: AtomicU64::new(0),
            read: AtomicBool::new(false),
            stale: AtomicBool::new(false),
            frame: RwLock::new(None),
            changed: (Mutex::new(()), Condvar::new()),
        }
    }

    /// Wake every [`TerminalPublication::wait_past`]. Taking the lock
    /// after the change orders it before a waiter's check, so no wake is
    /// lost.
    ///
    /// [`TerminalPublication::wait_past`]: super::TerminalPublication::wait_past
    pub(super) fn signal(&self) {
        let (lock, changed) = &self.changed;
        drop(lock.lock().unwrap_or_else(PoisonError::into_inner));
        changed.notify_all();
    }

    pub(super) fn current(&self) -> Option<Published> {
        self.frame
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// What is published with the generation stamped on it, read under the
    /// one lock a publish stamps both under.
    pub(super) fn current_stamped(&self) -> Option<(u64, Published)> {
        let frame = self.frame.read().unwrap_or_else(PoisonError::into_inner);
        let stamped = frame
            .clone()
            .map(|published| (self.generation.load(Ordering::Acquire), published));
        drop(frame);
        stamped
    }

    /// The consumer half of the read/stale handshake. `read` is raised after
    /// the frame is loaded, so a publish landing in between leaves `read`
    /// set on a frame nobody saw: the owner then publishes once more instead
    /// of deferring, the safe direction. `read` before `stale` pairs with
    /// the owner's `stale` before `read` in [`Self::defer`]: under `SeqCst`
    /// at least one side sees the other, so no deferral goes unanswered.
    /// A document is whole and never paced: it is handed back as it is, and
    /// is not a grid acquire in `perf`.
    pub(super) fn acquire(&self) -> Option<Published> {
        let frame = self.current();
        if matches!(frame, Some(Published::Document(_))) {
            return frame;
        }
        crate::perf::ACQUIRED.incr();
        self.read.store(true, Ordering::SeqCst);
        if !self.stale.swap(false, Ordering::SeqCst) {
            return frame;
        }
        // No publication lock is held here; the owner takes them to publish.
        let caught_up = self.hook.get().and_then(|catch_up| catch_up(&self.key));
        // A slot removed meanwhile stays removed, even when a replacement
        // owner answered for the same key.
        self.current()?;
        caught_up.map(Published::Grid).or(frame)
    }

    /// The owner half: skip this projection while the current frame is
    /// unread, leaving the slot stale for the next acquire.
    pub(super) fn defer(&self) -> bool {
        if self.generation.load(Ordering::Acquire) == 0 || self.read.load(Ordering::SeqCst) {
            return false;
        }
        self.stale.store(true, Ordering::SeqCst);
        // A consumer that read between the two loads may have missed
        // `stale`: publish for it instead. The publish clears `stale`.
        !self.read.load(Ordering::SeqCst)
    }
}
