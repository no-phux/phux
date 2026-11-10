//! Outer-terminal image ids for panes that share one outer terminal.
//!
//! Every pane's libghostty numbers its images from the same start, so two
//! panes replayed into one outer terminal would replace and delete each
//! other's images. A shared [`super::KittyResidency`] maps each pane image id
//! to an outer id drawn from this thread's allocator: the TUI paints its one
//! outer terminal from one thread.
//!
//! Only the low 24 bits are remapped. A Unicode placeholder cell spells those
//! in its foreground colour and the high byte in an optional third diacritic,
//! so rewriting the colour alone keeps a placeholder pointing at the mapped
//! image. Pane ids that differ only in the high byte map to outer ids that
//! differ only in the high byte, so the mapping stays one-to-one.

use std::{
    cell::RefCell,
    collections::HashSet,
    io::{self, Write},
};

use libghostty_vt::style::{RgbColor, StyleColor};

use super::write_delete_image;

const LOW: u32 = 0x00FF_FFFF;

/// Mappings kept before unused ones are pruned. Below it a mapping lives as
/// long as its residency, so placeholder cells painted once keep pointing at
/// the right outer image even if the pane deletes and re-sends it. Past it a
/// prune scan runs only once the map has doubled since the last, so its cost
/// stays amortised however many mappings stay in use.
const PRUNE_ABOVE: usize = 256;

thread_local! {
    static SPACE: RefCell<IdSpace> = RefCell::new(IdSpace::default());
}

/// The outer low-24-bit ids in use on this thread, and the outer images of
/// dropped residencies still waiting for their delete.
#[derive(Debug, Default)]
struct IdSpace {
    /// The last id handed out. Allocation walks forward and wraps, so a freed
    /// id is reused only after every other id has been tried.
    last: u32,
    live: HashSet<u32>,
    orphans: Vec<u32>,
}

impl IdSpace {
    fn allocate(&mut self) -> u32 {
        for _ in 0..LOW {
            self.last = if self.last >= LOW { 1 } else { self.last + 1 };
            if self.live.insert(self.last) {
                return self.last;
            }
        }
        // Sixteen million images on screen: share an id rather than fail.
        self.last
    }

    fn release(&mut self, low: u32) {
        if !self.orphans.iter().any(|outer| outer & LOW == low) {
            self.live.remove(&low);
        }
    }
}

/// One residency's pane-to-outer mapping of image ids' low 24 bits.
#[derive(Debug)]
pub(super) struct IdMap {
    /// Sorted by pane low.
    lows: Vec<Mapping>,
    /// The size past which the next prune scan runs.
    prune_at: usize,
}

impl Default for IdMap {
    fn default() -> Self {
        Self {
            lows: Vec::new(),
            prune_at: PRUNE_ABOVE,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Mapping {
    pane: u32,
    outer: u32,
    /// A placeholder cell painted since the last full walk spells it, so
    /// it may still be on screen: a prune must keep it, or the image coming
    /// back under a new outer id would stay invisible in those clean cells.
    painted: bool,
}

impl IdMap {
    /// The outer id for pane image `pane`, allocated on first use.
    pub(super) fn outer(&mut self, pane: u32) -> u32 {
        (pane & !LOW) | self.low(pane & LOW)
    }

    /// The outer id already mapped for pane image `pane`.
    pub(super) fn get(&self, pane: u32) -> Option<u32> {
        let index = self.find(pane & LOW).ok()?;
        Some((pane & !LOW) | self.lows[index].outer)
    }

    fn find(&self, low: u32) -> Result<usize, usize> {
        self.lows.binary_search_by_key(&low, |mapping| mapping.pane)
    }

    fn low(&mut self, pane_low: u32) -> u32 {
        self.mapping(pane_low).outer
    }

    fn mapping(&mut self, pane_low: u32) -> &mut Mapping {
        let index = self.find(pane_low).unwrap_or_else(|index| {
            let outer = SPACE.with(|space| space.borrow_mut().allocate());
            let mapping = Mapping {
                pane: pane_low,
                outer,
                painted: false,
            };
            self.lows.insert(index, mapping);
            index
        });
        &mut self.lows[index]
    }

    /// The outer low a placeholder cell spelling `pane_low` is painted with.
    fn painted_low(&mut self, pane_low: u32) -> u32 {
        let mapping = self.mapping(pane_low);
        mapping.painted = true;
        mapping.outer
    }

    /// Every on-screen placeholder cell is about to be painted again.
    pub(super) fn begin_full_walk(&mut self) {
        for mapping in &mut self.lows {
            mapping.painted = false;
        }
    }

    pub(super) const fn wants_prune(&self) -> bool {
        self.lows.len() > self.prune_at
    }

    /// Release the mappings whose pane low `keep` rejects and no painted
    /// placeholder cell spells.
    pub(super) fn prune(&mut self, keep: impl Fn(u32) -> bool) {
        SPACE.with(|space| {
            let mut space = space.borrow_mut();
            self.lows.retain(|mapping| {
                mapping.painted || keep(mapping.pane) || {
                    space.release(mapping.outer);
                    false
                }
            });
        });
        self.prune_at = PRUNE_ABOVE.max(self.lows.len() * 2);
    }
}

impl Drop for IdMap {
    fn drop(&mut self) {
        // A thread tearing down its locals may already have dropped the space.
        let _ = SPACE.try_with(|space| {
            let mut space = space.borrow_mut();
            for mapping in &self.lows {
                space.release(mapping.outer);
            }
        });
    }
}

/// Queue outer images a dropped residency left on the outer terminal for
/// [`write_orphaned_deletes`]. Their ids stay allocated until then.
pub(super) fn orphan(outer: impl IntoIterator<Item = u32>) {
    let _ = SPACE.try_with(|space| space.borrow_mut().orphans.extend(outer));
}

/// Delete (`d=I`) the outer images shared residencies dropped while holding.
///
/// A closed pane's renderer leaves its images on the outer terminal; this
/// deletes them and frees their ids. Returns whether anything was written.
/// Call it at the start of each composited frame, on the thread that owns
/// the residencies.
pub fn write_orphaned_deletes(out: &mut impl Write) -> io::Result<bool> {
    let orphans = SPACE.with(|space| std::mem::take(&mut space.borrow_mut().orphans));
    if orphans.is_empty() {
        return Ok(false);
    }
    let written = orphans
        .iter()
        .try_for_each(|&id| write_delete_image(out, id));
    SPACE.with(|space| {
        let mut space = space.borrow_mut();
        if written.is_err() {
            space.orphans.extend(&orphans);
            return;
        }
        for outer in &orphans {
            space.live.remove(&(outer & LOW));
        }
    });
    written.map(|()| true)
}

/// The image id's low 24 bits a placeholder cell with foreground `fg` spells,
/// as libghostty reads it: a palette index is the value itself.
fn placeholder_low(fg: StyleColor) -> u32 {
    match fg {
        StyleColor::None => 0,
        StyleColor::Palette(index) => u32::from(index.0),
        StyleColor::Rgb(rgb) => u32::from_be_bytes([0, rgb.r, rgb.g, rgb.b]),
    }
}

/// The truecolour foreground spelling low 24 bits `low`.
const fn low_color(low: u32) -> RgbColor {
    let [_, r, g, b] = low.to_be_bytes();
    RgbColor { r, g, b }
}

/// Map a placeholder foreground through `ids` (identity without a map).
pub(super) fn placeholder_color(ids: Option<&mut IdMap>, fg: StyleColor) -> RgbColor {
    let low = placeholder_low(fg);
    low_color(ids.map_or(low, |ids| ids.painted_low(low)))
}
