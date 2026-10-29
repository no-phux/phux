//! The double-buffered grid (ADR-0133 decision 4).
//!
//! The engine owner thread projects a terminal's viewport into a back
//! [`GridBuffer`] after every render and publishes it here behind an `Arc`.
//! A consumer on any thread calls [`Publication::acquire`] and receives an
//! immutable [`GridFrame`] it may hold for as long as it likes: the next
//! publish swaps a new frame in and never touches the one already handed
//! out. There is no "valid until the next call" contract anywhere.
//!
//! Every frame carries a per-presentation generation that increases by one per
//! publish, so a consumer that remembers the generation it last painted
//! skips unchanged terminals for the cost of one atomic load
//! ([`TerminalPublication::generation`]), and the rows libghostty reported
//! dirty for this generation ([`GridFrame::dirty_rows`]) so it can repaint
//! incrementally. A full or first projection marks every row dirty.
//!
//! Publication is paced by reads. The owner projects a damaged presentation
//! at once when a consumer has acquired its current frame, so a keystroke's
//! echo into a caught-up consumer publishes with no added delay. While the
//! current frame is still unread, a projection would only replace a frame
//! nobody looked at, so the owner defers it and marks the slot stale; the
//! next acquire of that slot has the owner project the latest state before
//! it returns. A consumer therefore never receives a frame older than the
//! replica at the moment it asked, and the owner never projects faster than
//! consumers read. Deferral holds the generation still, and it only happens
//! while the current generation is unread, so a consumer that polls
//! generations still sees a change it has not painted. libghostty keeps
//! accumulating dirty rows across a deferral, so the caught-up frame's
//! [`GridFrame::dirty_rows`] cover everything since the frame before it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use crate::ViewId;
pub use phux_client_core::grid::{
    Cell, CellMetadata, Cursor, CursorStyle, CursorWidth, GridBuffer, GridDamage,
};
use phux_protocol::ResourceId;

mod model;
pub use model::{FrameColors, GridFrame, ImageData, ImagePlacement, Rgb, Scrollbar};

/// Which presentation a slot publishes: a terminal's default one or an
/// independent view.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum FrameKey {
    Default(ResourceId),
    View(ViewId),
}

/// The owner thread's catch-up: project the presentation behind a stale slot
/// now, mark the new frame read, and hand it back; `None` when no owner
/// answers.
pub(crate) type CatchUp = Arc<dyn Fn(&FrameKey) -> Option<Arc<GridFrame>> + Send + Sync>;

/// The catch-up the current owner installed, shared by the table and every
/// slot so a held [`TerminalPublication`] reaches the owner too.
#[derive(Default)]
struct Hook(RwLock<Option<CatchUp>>);

impl Hook {
    fn get(&self) -> Option<CatchUp> {
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

/// One presentation's slot: the generation counter a consumer polls, the
/// frame behind it, and the read/stale pair that paces publication.
#[derive(Debug)]
struct Slot {
    key: FrameKey,
    hook: Arc<Hook>,
    generation: AtomicU64,
    /// A consumer acquired the current frame since it was published.
    read: AtomicBool,
    /// The owner deferred a projection past the current frame; the next
    /// acquire catches up.
    stale: AtomicBool,
    frame: RwLock<Option<Arc<GridFrame>>>,
}

impl Slot {
    const fn new(key: FrameKey, hook: Arc<Hook>) -> Self {
        Self {
            key,
            hook,
            generation: AtomicU64::new(0),
            read: AtomicBool::new(false),
            stale: AtomicBool::new(false),
            frame: RwLock::new(None),
        }
    }

    fn current(&self) -> Option<Arc<GridFrame>> {
        self.frame
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The consumer half of the read/stale handshake. `read` is raised after
    /// the frame is loaded, so a publish landing in between leaves `read`
    /// set on a frame nobody saw: the owner then publishes once more instead
    /// of deferring, the safe direction. `read` before `stale` pairs with
    /// the owner's `stale` before `read` in [`Self::defer`]: under `SeqCst`
    /// at least one side sees the other, so no deferral goes unanswered.
    fn acquire(&self) -> Option<Arc<GridFrame>> {
        crate::perf::ACQUIRED.incr();
        let frame = self.current();
        self.read.store(true, Ordering::SeqCst);
        if !self.stale.swap(false, Ordering::SeqCst) {
            return frame;
        }
        // No publication lock is held here; the owner takes them to publish.
        let caught_up = self.hook.get().and_then(|catch_up| catch_up(&self.key));
        // A slot removed meanwhile stays removed, even when a replacement
        // owner answered for the same key.
        self.current()?;
        caught_up.or(frame)
    }

    /// The owner half: skip this projection while the current frame is
    /// unread, leaving the slot stale for the next acquire.
    fn defer(&self) -> bool {
        if self.read.load(Ordering::SeqCst) {
            return false;
        }
        self.stale.store(true, Ordering::SeqCst);
        // A consumer that read between the two loads may have missed
        // `stale`: publish for it instead. The publish clears `stale`.
        !self.read.load(Ordering::SeqCst)
    }
}

/// A consumer's handle on one terminal's slot: [`Self::generation`] is one
/// atomic load, and [`Self::acquire`] clones the current frame's `Arc`.
///
/// The handle stays valid after the terminal is removed: `generation`
/// reports the last published value and `acquire` returns `None`.
#[derive(Clone, Debug)]
pub struct TerminalPublication {
    slot: Arc<Slot>,
}

impl TerminalPublication {
    /// The generation of the current frame; zero before the first publish.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.slot.generation.load(Ordering::Acquire)
    }

    /// The current frame, if one is published. When the owner deferred a
    /// projection past it, this waits for the owner to project the latest
    /// state and returns that instead, so it must never run on the owner
    /// thread.
    #[must_use]
    pub fn acquire(&self) -> Option<Arc<GridFrame>> {
        self.slot.acquire()
    }
}

/// The table of published frames: one default slot per terminal and one slot
/// per explicitly created view. Existing terminal-keyed methods address the
/// default presentation only.
///
/// Shared between the owner thread that publishes and every consumer that
/// acquires; both sides hold it through an `Arc`.
#[derive(Debug, Default)]
pub struct Publication {
    slots: RwLock<HashMap<FrameKey, Arc<Slot>>>,
    hook: Arc<Hook>,
}

impl Publication {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current frame for `terminal`, if one is published.
    #[must_use]
    pub fn acquire(&self, terminal: &ResourceId) -> Option<Arc<GridFrame>> {
        self.slot(terminal).and_then(|slot| slot.acquire())
    }

    /// The generation of `terminal`'s current frame; `None` for a terminal
    /// with no slot.
    #[must_use]
    pub fn generation(&self, terminal: &ResourceId) -> Option<u64> {
        self.slot(terminal).map(|slot| slot.generation())
    }

    /// A handle on one terminal's slot, or `None` while nothing has been
    /// published for it.
    #[must_use]
    pub fn slot(&self, terminal: &ResourceId) -> Option<TerminalPublication> {
        self.slot_key(&FrameKey::Default(terminal.clone()))
    }

    /// The frame for an independent view, if it still exists.
    #[must_use]
    pub fn acquire_view(&self, view: ViewId) -> Option<Arc<GridFrame>> {
        self.view_slot(view).and_then(|slot| slot.acquire())
    }

    /// The independent view's publication generation.
    #[must_use]
    pub fn view_generation(&self, view: ViewId) -> Option<u64> {
        self.view_slot(view).map(|slot| slot.generation())
    }

    /// A polling handle that is cleared when the view is destroyed.
    #[must_use]
    pub fn view_slot(&self, view: ViewId) -> Option<TerminalPublication> {
        self.slot_key(&FrameKey::View(view))
    }

    fn slot_key(&self, key: &FrameKey) -> Option<TerminalPublication> {
        self.slot_arc(key).map(|slot| TerminalPublication { slot })
    }

    /// The terminals with a published frame.
    #[must_use]
    pub fn terminals(&self) -> Vec<ResourceId> {
        self.slots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, slot)| slot.generation.load(Ordering::Acquire) != 0)
            .filter_map(|(key, _)| match key {
                FrameKey::Default(id) => Some(id.clone()),
                FrameKey::View(_) => None,
            })
            .collect()
    }

    /// Install the owner thread that answers catch-ups, replacing the one
    /// before it.
    pub(crate) fn install_catch_up(&self, catch_up: CatchUp) {
        *self.hook.0.write().unwrap_or_else(PoisonError::into_inner) = Some(catch_up);
    }

    /// Whether the owner may skip projecting `key` because its current frame
    /// is unread; a skip leaves the slot stale for the next acquire.
    pub(crate) fn defer(&self, key: &FrameKey) -> bool {
        self.slot_arc(key).is_some_and(|slot| slot.defer())
    }

    /// Whether the owner deferred `key`'s projection past its current frame.
    pub(crate) fn stale(&self, key: &FrameKey) -> bool {
        self.slot_arc(key)
            .is_some_and(|slot| slot.stale.load(Ordering::SeqCst))
    }

    /// Hand `key`'s current frame to the consumer whose acquire asked for a
    /// catch-up, marking it read. Owner thread only, so no publish lands
    /// between the load and the mark.
    pub(crate) fn hand_off(&self, key: &FrameKey) -> Option<Arc<GridFrame>> {
        let slot = self.slot_arc(key)?;
        let frame = slot.current();
        slot.read.store(true, Ordering::SeqCst);
        frame
    }

    fn slot_arc(&self, key: &FrameKey) -> Option<Arc<Slot>> {
        self.slots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned()
    }

    /// Publish `frame` for `terminal`, stamping the next generation, and
    /// hand back the frame it replaced so the publisher can retire it and
    /// refill its buffer once no consumer holds it.
    pub(crate) fn publish(
        &self,
        terminal: &ResourceId,
        frame: GridFrame,
    ) -> Option<Arc<GridFrame>> {
        self.publish_key(FrameKey::Default(terminal.clone()), frame)
    }

    pub(crate) fn publish_view(&self, view: ViewId, frame: GridFrame) -> Option<Arc<GridFrame>> {
        self.publish_key(FrameKey::View(view), frame)
    }

    fn publish_key(&self, key: FrameKey, mut frame: GridFrame) -> Option<Arc<GridFrame>> {
        let slot = {
            let mut slots = self.slots.write().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(
                slots
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(Slot::new(key, Arc::clone(&self.hook)))),
            )
        };
        let generation = slot.generation.load(Ordering::Acquire) + 1;
        frame.generation = generation;
        // Cleared before the swap: a consumer that loads the new frame
        // raises `read` after this, never before it.
        slot.read.store(false, Ordering::SeqCst);
        slot.stale.store(false, Ordering::SeqCst);
        let previous = {
            let mut current = slot.frame.write().unwrap_or_else(PoisonError::into_inner);
            current.replace(Arc::new(frame))
        };
        slot.generation.store(generation, Ordering::Release);
        previous
    }

    /// Drop `terminal`'s slot. Consumers holding a frame keep it; a handle
    /// they hold keeps reporting the last generation and acquires `None`.
    pub(crate) fn remove(&self, terminal: &ResourceId) {
        self.remove_key(&FrameKey::Default(terminal.clone()));
    }

    pub(crate) fn remove_view(&self, view: ViewId) {
        self.remove_key(&FrameKey::View(view));
    }

    fn remove_key(&self, key: &FrameKey) {
        let slot = self
            .slots
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
        if let Some(slot) = slot {
            slot.frame
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
        }
    }
}

#[cfg(test)]
mod tests;
