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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use crate::ViewId;
pub use phux_client_core::grid::{
    Cell, CellMetadata, Cursor, CursorStyle, CursorWidth, GridBuffer, GridDamage,
};
use phux_protocol::ResourceId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FrameKey {
    Default(ResourceId),
    View(ViewId),
}

/// The scrollable area behind a published viewport, in rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scrollbar {
    /// Rows in the scrollable area (history plus the active area).
    pub total: u64,
    /// The viewport's first row within that area.
    pub offset: u64,
    /// Rows the viewport shows.
    pub len: u64,
}

impl Scrollbar {
    /// Whether the viewport follows the live tail.
    #[must_use]
    pub const fn at_tail(self) -> bool {
        self.offset.saturating_add(self.len) >= self.total
    }
}

/// One RGB color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

/// The default colors and the palette every cell of a frame was resolved
/// against, copied out of libghostty's render state so a binding needs no
/// engine type to paint a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameColors {
    /// The effective render background.
    pub background: Rgb,
    /// The effective render foreground.
    pub foreground: Rgb,
    /// The terminal explicitly set a default background.
    pub has_background: bool,
    /// The terminal explicitly set a default foreground.
    pub has_foreground: bool,
    /// Reverse-colors mode was active when projected.
    pub reversed: bool,
    /// The cursor color the terminal set, if any.
    pub cursor: Option<Rgb>,
    /// The active 256-color palette.
    pub palette: Box<[Rgb; 256]>,
}

impl Default for FrameColors {
    fn default() -> Self {
        Self {
            background: Rgb::default(),
            foreground: Rgb::default(),
            has_background: false,
            has_foreground: false,
            reversed: false,
            cursor: None,
            palette: Box::new([Rgb::default(); 256]),
        }
    }
}

/// One published viewport: immutable once built, shared by `Arc`.
#[derive(Debug)]
pub struct GridFrame {
    /// The terminal this frame projects.
    pub terminal_id: ResourceId,
    /// Increases by one per publish of this presentation, starting at one.
    pub generation: u64,
    /// The logical subscription of the projected replica generation.
    pub stream_id: u64,
    /// The replica generation.
    pub bootstrap_id: u64,
    /// The highest live sequence applied to the replica.
    pub last_seq: u64,
    /// Viewport width in cells.
    pub cols: u16,
    /// Viewport height in cells.
    pub rows: u16,
    /// Cursor placement and shape.
    pub cursor: Cursor,
    /// The scrollable area behind the viewport.
    pub scrollbar: Scrollbar,
    /// The colors the cells were resolved against.
    pub colors: FrameColors,
    /// How much changed since the previous generation: `Full` on the first
    /// frame of a replica generation and after a global change, `Rows`
    /// when only `dirty_rows` changed, `Clean` when a publish carried no
    /// grid change (a scroll that did not move, for example).
    pub damage: GridDamage,
    /// The dense `rows * cols` cells, their UTF-8 arena, their provenance,
    /// and the per-row dirty flags for this generation.
    pub buffer: GridBuffer,
}

impl GridFrame {
    /// The rows libghostty reported changed for this generation; every row
    /// on a full projection.
    pub fn dirty_rows(&self) -> impl Iterator<Item = u16> + '_ {
        self.buffer
            .row_dirty
            .iter()
            .enumerate()
            .filter(|(_, dirty)| **dirty)
            .filter_map(|(row, _)| u16::try_from(row).ok())
    }

    /// Whether `row` changed in this generation.
    #[must_use]
    pub fn is_row_dirty(&self, row: u16) -> bool {
        self.buffer
            .row_dirty
            .get(usize::from(row))
            .copied()
            .unwrap_or(false)
    }

    /// The cell at `(row, col)`, if inside the viewport.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        self.buffer
            .cells
            .get(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }

    /// The text of the cell at `(row, col)` as a UTF-8 slice of the arena;
    /// empty for a blank cell, a wide spacer, or a position off the grid.
    #[must_use]
    pub fn cell_text(&self, row: u16, col: u16) -> &[u8] {
        if col >= self.cols || row >= self.rows {
            return &[];
        }
        self.buffer
            .cell_text(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }

    /// One row's text with blank cells rendered as spaces and trailing
    /// blanks trimmed. A convenience for tests and diagnostics; a renderer
    /// walks the cells.
    #[must_use]
    pub fn row_text(&self, row: u16) -> String {
        let mut text = String::new();
        for col in 0..self.cols {
            let cell = self.cell_text(row, col);
            if cell.is_empty() {
                let spacer = self
                    .cell(row, col)
                    .is_some_and(|cell| cell.wide == WIDE_SPACER_TAIL);
                if !spacer {
                    text.push(' ');
                }
            } else {
                text.push_str(&String::from_utf8_lossy(cell));
            }
        }
        text.truncate(text.trim_end().len());
        text
    }

    /// Every row's text joined by newlines, trailing blank rows trimmed.
    #[must_use]
    pub fn text(&self) -> String {
        let mut rows: Vec<String> = (0..self.rows).map(|row| self.row_text(row)).collect();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        rows.join("\n")
    }
}

/// libghostty's `CellWide::SpacerTail` discriminant, as `Cell::wide` carries
/// it.
const WIDE_SPACER_TAIL: u8 = 2;

/// One terminal's slot: the generation counter a consumer polls and the
/// frame behind it.
#[derive(Debug, Default)]
struct Slot {
    generation: AtomicU64,
    frame: RwLock<Option<Arc<GridFrame>>>,
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

    /// The current frame, if one is published.
    #[must_use]
    pub fn acquire(&self) -> Option<Arc<GridFrame>> {
        self.slot
            .frame
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
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
        self.slots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .map(|slot| TerminalPublication {
                slot: Arc::clone(slot),
            })
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

    /// Publish `frame` for `terminal`, stamping the next generation, and
    /// hand back the frame it replaced so the publisher can recycle its
    /// buffer when no consumer still holds it.
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
            Arc::clone(slots.entry(key).or_default())
        };
        let generation = slot.generation.load(Ordering::Acquire) + 1;
        frame.generation = generation;
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
mod tests {
    use super::*;

    fn frame(id: &ResourceId, text: &[u8]) -> GridFrame {
        let mut buffer = GridBuffer::default();
        buffer.utf8.extend_from_slice(text);
        for (index, _) in text.iter().enumerate() {
            buffer.cells.push(Cell {
                utf8_offset: u32::try_from(index).expect("small"),
                utf8_len: 1,
                ..Cell::default()
            });
            buffer.metadata.push(CellMetadata::default());
        }
        buffer.row_dirty.push(true);
        GridFrame {
            terminal_id: id.clone(),
            generation: 0,
            stream_id: 1,
            bootstrap_id: 1,
            last_seq: 0,
            cols: u16::try_from(text.len()).expect("small"),
            rows: 1,
            cursor: Cursor::default(),
            scrollbar: Scrollbar::default(),
            colors: FrameColors::default(),
            damage: GridDamage::Full,
            buffer,
        }
    }

    #[test]
    fn generations_advance_per_publish_and_held_frames_survive() {
        let table = Publication::new();
        let id = ResourceId::local(3);
        assert!(table.acquire(&id).is_none());
        assert_eq!(table.generation(&id), None);

        assert!(table.publish(&id, frame(&id, b"ab")).is_none());
        let first = table.acquire(&id).expect("published");
        assert_eq!(first.generation, 1);
        assert_eq!(table.generation(&id), Some(1));
        assert_eq!(first.text(), "ab");
        assert_eq!(first.dirty_rows().collect::<Vec<_>>(), vec![0]);

        let handle = table.slot(&id).expect("slot");
        let replaced = table.publish(&id, frame(&id, b"cd")).expect("replaced");
        assert_eq!(replaced.generation, 1);
        assert_eq!(handle.generation(), 2);
        assert_eq!(first.text(), "ab", "a held frame is never mutated");
        assert_eq!(handle.acquire().expect("current").text(), "cd");

        table.remove(&id);
        assert!(table.acquire(&id).is_none());
        assert_eq!(handle.generation(), 2);
        assert!(handle.acquire().is_none());
        assert_eq!(first.text(), "ab");
    }

    #[test]
    fn terminals_lists_only_published_slots() {
        let table = Publication::new();
        let id = ResourceId::local(9);
        assert!(table.terminals().is_empty());
        table.publish(&id, frame(&id, b"x"));
        assert_eq!(table.terminals(), vec![id]);
    }
}
