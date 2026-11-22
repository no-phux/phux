//! The divider layer's cell buffer: one frame's chrome cells, each grapheme
//! stored inline, sorted and de-duplicated before emission.

use ratatui::style::{Color, Style};

use super::CELL_BYTES;
use crate::attach::multi_pane::PaneLayout;

/// One cell's grapheme, stored inline so composing a frame allocates nothing
/// per cell.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Sym {
    buf: [u8; CELL_BYTES],
    len: u8,
}

impl Sym {
    /// A positioned cell that paints nothing: the trailing half of a wide
    /// glyph. The emitter skips it and breaks its run, so the next cell
    /// re-anchors with its own CUP.
    pub(super) const EMPTY: Self = Self {
        buf: [0; CELL_BYTES],
        len: 0,
    };

    /// Store `s`, truncated to whole UTF-8 characters that fit.
    pub(super) fn new(s: &str) -> Self {
        let mut out = Self::EMPTY;
        for ch in s.chars() {
            out.push(ch);
        }
        out
    }

    /// Append `ch` if the remaining room holds it; otherwise drop it.
    pub(super) fn push(&mut self, ch: char) {
        let len = usize::from(self.len);
        let room = CELL_BYTES - len;
        if ch.len_utf8() > room {
            return;
        }
        let written = ch.encode_utf8(&mut self.buf[len..]).len();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "len + written <= CELL_BYTES = 16, which fits u8"
        )]
        {
            self.len = (len + written) as u8;
        }
    }

    pub(super) fn as_str(&self) -> &str {
        // Every push wrote whole chars, so this cannot fail; a failure would
        // degrade to an unpainted cell.
        std::str::from_utf8(&self.buf[..usize::from(self.len)]).unwrap_or("")
    }

    pub(super) const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// One painted chrome cell.
#[derive(Clone, Copy)]
pub(super) struct ChromeCell {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) sym: Sym,
    pub(super) style: Style,
}

/// The chrome cells of one frame in paint order: a few hundred cells, not a
/// viewport-sized buffer, so a title change is not an O(viewport) event.
#[derive(Default)]
pub(super) struct ChromeCells {
    pub(super) cells: Vec<ChromeCell>,
}

impl ChromeCells {
    pub(super) fn clear(&mut self) {
        self.cells.clear();
    }

    /// Lay `fill` under every chrome cell: [`Theme::ground_fill`], the
    /// surface when the terminal's background is unknown, so no rule or
    /// title ink sits on a ground it was not solved against.
    pub(super) fn fill_ground(&mut self, fill: Color) {
        if fill == Color::Reset {
            return;
        }
        for cell in &mut self.cells {
            cell.style = cell.style.bg(fill);
        }
    }

    /// Record one cell. A later write to the same coordinate wins, which
    /// is what lets a title overwrite the rule the rail laid down first.
    pub(super) fn put(&mut self, x: u16, y: u16, sym: Sym, style: Style) {
        self.cells.push(ChromeCell { x, y, sym, style });
    }

    pub(super) fn put_str(&mut self, x: u16, y: u16, symbol: &str, style: Style) {
        self.put(x, y, Sym::new(symbol), style);
    }

    /// Sort for emission and drop duplicates (last write wins) and anything
    /// inside a pane interior: the skip-cell carve-out, applied once here.
    pub(super) fn finalize(&mut self, layout: &PaneLayout) {
        let (cols, rows) = layout.viewport;
        self.cells
            .retain(|c| c.x < cols && c.y < rows && !in_any_pane(layout, c.x, c.y));
        // Stable, so the insertion order of equal coordinates survives
        // and "last write wins" is well defined.
        self.cells.sort_by_key(|c| (c.y, c.x));
        let mut kept: usize = 0;
        for i in 0..self.cells.len() {
            let last_of_run = i + 1 == self.cells.len()
                || (self.cells[i + 1].y, self.cells[i + 1].x) != (self.cells[i].y, self.cells[i].x);
            if last_of_run {
                self.cells[kept] = self.cells[i];
                kept += 1;
            }
        }
        self.cells.truncate(kept);
    }
}

/// `true` when `(x, y)` sits inside any pane's interior rectangle.
pub(super) fn in_any_pane(layout: &PaneLayout, x: u16, y: u16) -> bool {
    layout
        .rects
        .values()
        .any(|r| x >= r.x && y >= r.y && x < r.x.saturating_add(r.w) && y < r.y.saturating_add(r.h))
}
