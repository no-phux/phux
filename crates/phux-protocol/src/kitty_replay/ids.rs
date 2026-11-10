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

use std::{cell::RefCell, collections::HashSet};

use libghostty_vt::style::{RgbColor, StyleColor};

const LOW: u32 = 0x00FF_FFFF;

/// Mappings kept before unused ones are pruned. Below it a mapping lives as
/// long as its residency, so placeholder cells painted once keep pointing at
/// the right outer image even if the pane deletes and re-sends it.
const PRUNE_ABOVE: usize = 256;

thread_local! {
    static SPACE: RefCell<IdSpace> = RefCell::new(IdSpace::default());
}

/// The outer low-24-bit ids in use on this thread.
#[derive(Debug, Default)]
struct IdSpace {
    /// The last id handed out. Allocation walks forward and wraps, so a freed
    /// id is reused only after every other id has been tried.
    last: u32,
    live: HashSet<u32>,
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
        self.live.remove(&low);
    }
}

/// One residency's pane-to-outer mapping of image ids' low 24 bits.
#[derive(Debug, Default)]
pub(super) struct IdMap {
    /// `(pane low, outer low)`, sorted by pane low.
    lows: Vec<(u32, u32)>,
}

impl IdMap {
    /// The outer id for pane image `pane`, allocated on first use.
    pub(super) fn outer(&mut self, pane: u32) -> u32 {
        (pane & !LOW) | self.low(pane & LOW)
    }

    /// The outer id already mapped for pane image `pane`.
    pub(super) fn get(&self, pane: u32) -> Option<u32> {
        let index = self.find(pane & LOW).ok()?;
        Some((pane & !LOW) | self.lows[index].1)
    }

    fn find(&self, low: u32) -> Result<usize, usize> {
        self.lows.binary_search_by_key(&low, |&(pane, _)| pane)
    }

    fn low(&mut self, pane_low: u32) -> u32 {
        match self.find(pane_low) {
            Ok(index) => self.lows[index].1,
            Err(index) => {
                let low = SPACE.with(|space| space.borrow_mut().allocate());
                self.lows.insert(index, (pane_low, low));
                low
            }
        }
    }

    pub(super) const fn wants_prune(&self) -> bool {
        self.lows.len() > PRUNE_ABOVE
    }

    /// Release the mappings whose pane low `keep` rejects.
    pub(super) fn prune(&mut self, keep: impl Fn(u32) -> bool) {
        SPACE.with(|space| {
            let mut space = space.borrow_mut();
            self.lows.retain(|&(pane, outer)| {
                keep(pane) || {
                    space.release(outer);
                    false
                }
            });
        });
    }
}

impl Drop for IdMap {
    fn drop(&mut self) {
        // A thread tearing down its locals may already have dropped the space.
        let _ = SPACE.try_with(|space| {
            let mut space = space.borrow_mut();
            for &(_, outer) in &self.lows {
                space.release(outer);
            }
        });
    }
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
    low_color(ids.map_or(low, |ids| ids.low(low)))
}
