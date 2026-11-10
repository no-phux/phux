//! One pane renderer's kitty graphics on the shared outer terminal.
//!
//! Every pane's images are replayed into one outer terminal, so each pane
//! renderer maps its image ids into outer ids no other pane uses
//! ([`KittyResidency::shared`]). Classic placements carry the mapped id in
//! every command; a Unicode placeholder cell spells the id in its foreground
//! colour, which the row painter rewrites through [`is_placeholder`] and
//! [`KittyResidency::placeholder_fg`].

use std::io::{self, Write};

use libghostty_vt::{Terminal as GhosttyTerminal, kitty::graphics::PlacementIterator};
use phux_protocol::kitty_replay::{self, KittyResidency};

use super::{FrontBuffer, RenderError};

/// The Unicode placeholder (U+10EEEE) a virtual placement's cells print.
const PLACEHOLDER: &str = "\u{10EEEE}";

/// Whether a cell's cluster is a Unicode placeholder (with its diacritics).
pub(super) fn is_placeholder(text: &str) -> bool {
    text.starts_with(PLACEHOLDER)
}

/// The placement walk and the outer terminal's residency for one pane.
#[derive(Debug)]
pub(super) struct PaneKitty<'alloc> {
    placements: PlacementIterator<'alloc>,
    residency: KittyResidency,
}

impl PaneKitty<'_> {
    pub(super) fn new() -> Result<Self, RenderError> {
        Ok(Self {
            placements: PlacementIterator::new()?,
            residency: KittyResidency::shared(),
        })
    }

    /// The residency, for the row painter's placeholder rewrite.
    pub(super) const fn residency(&mut self) -> &mut KittyResidency {
        &mut self.residency
    }

    /// Assume the outer terminal may have lost this pane's images.
    pub(super) fn forget(&mut self) {
        self.residency.forget();
    }
}

impl super::TerminalRenderer<'_> {
    /// The pane left the visible layout (another window, a zoom): it is not
    /// painted again until it returns, so delete its images from the outer
    /// terminal, where they would stay over the panes shown there now, and
    /// record that it holds none.
    pub(in crate::attach) fn release_kitty(&mut self, out: &mut impl Write) -> io::Result<()> {
        if self.kitty.residency.release(out)? {
            self.front.invalidate_all();
        }
        Ok(())
    }

    #[cfg(test)]
    /// The outer id this pane's image `pane_id` is shown under, once mapped.
    pub(crate) fn kitty_outer_id(&self, pane_id: u32) -> Option<u32> {
        self.kitty.residency.outer_id(pane_id)
    }
}

impl<'alloc> PaneKitty<'alloc> {
    /// Bring the outer terminal's copy of the pane's kitty graphics up to
    /// date, emitting only what the residency says it lacks. Returns whether
    /// a classic placement was written or removed: that moved the cursor and
    /// drew over cells, so it forgets the front buffer. Unchanged images,
    /// payload uploads and Unicode-placeholder placements leave cells and
    /// front alone.
    pub(super) fn replay(
        &mut self,
        terminal: &GhosttyTerminal<'alloc, '_>,
        front: &mut FrontBuffer,
        out: &mut impl Write,
        at: ((u16, u16), (u16, u16)),
    ) -> Result<bool, RenderError> {
        let (origin, clip) = at;
        let touched_cells = kitty_replay::emit_kitty_graphics_replay(
            terminal,
            &mut self.placements,
            &mut self.residency,
            out,
            origin,
            clip,
        )?;
        if touched_cells {
            front.invalidate_all();
        }
        Ok(touched_cells)
    }
}
