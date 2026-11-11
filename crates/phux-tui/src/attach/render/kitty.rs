//! One pane renderer's kitty graphics on the shared outer terminal.
//!
//! Every pane's images are replayed into one outer terminal, so each pane
//! renderer maps its image ids into outer ids no other pane uses
//! ([`KittyResidency::shared`]). Classic placements carry the mapped id in
//! every command; a Unicode placeholder cell spells the id in its foreground
//! colour, which the row painter rewrites through [`is_placeholder`] and
//! [`KittyResidency::placeholder_fg`].
//!
//! The residency never re-sends an unchanged image, which kitty and Ghostty
//! honour; images lost behind its back heal on the schedule in
//! `attach::kitty_heal`, through [`super::TerminalRenderer::kitty_sends`]
//! and [`super::TerminalRenderer::kitty_resend`]. Terminals known to tie
//! images to cells ([`ties_images_to_cells`]) re-send on every paint.

use std::io::{self, Write};

use libghostty_vt::{Terminal as GhosttyTerminal, kitty::graphics::PlacementIterator};
use phux_protocol::kitty_replay::{self, ImageSend, KittyResidency};

use super::{FrontBuffer, RenderError};

/// The Unicode placeholder (U+10EEEE) a virtual placement's cells print.
const PLACEHOLDER: &str = "\u{10EEEE}";

/// Whether a cell's cluster is a Unicode placeholder (with its diacritics).
pub(super) fn is_placeholder(text: &str) -> bool {
    text.starts_with(PLACEHOLDER)
}

/// Whether the outer terminal, judged by its environment, drops an image
/// from the cells text overwrites (`WezTerm`, Konsole) instead of keeping it
/// until a clear, so images must be re-sent on every paint.
///
/// The environment is all the TUI knows of the terminal (it sends no
/// `XTVERSION` query), so this misses such a terminal behind tmux or screen
/// (`TERM_PROGRAM` names the multiplexer), over SSH (neither variable is
/// forwarded by default), or under a renamed `TERM_PROGRAM`; those fall back
/// to the timed heal. A variable inherited from an outer session (Konsole
/// launching another terminal) can also turn it on needlessly.
fn ties_images_to_cells(env: impl Fn(&str) -> Option<String>) -> bool {
    env("TERM_PROGRAM").is_some_and(|program| program.eq_ignore_ascii_case("wezterm"))
        || env("KONSOLE_VERSION").is_some_and(|version| !version.is_empty())
}

/// [`ties_images_to_cells`] for this process.
#[cfg(not(test))]
fn heals_every_paint() -> bool {
    static TIES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TIES.get_or_init(|| ties_images_to_cells(|key| std::env::var(key).ok()))
}

/// Unit tests keep the byte savings whatever terminal runs them.
#[cfg(test)]
const fn heals_every_paint() -> bool {
    false
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

    /// A forced paint: the screen may have been cleared, and every row,
    /// placeholder cells included, is painted again.
    pub(super) fn begin_full_paint(&mut self) {
        self.residency.forget();
        self.residency.begin_full_walk();
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

    /// The images the outer terminal holds for this pane, with when and at
    /// what cost each was last sent.
    pub(in crate::attach) fn kitty_sends(&self) -> impl Iterator<Item = ImageSend> + '_ {
        self.kitty.residency.sends()
    }

    /// Re-send pane image `pane_id` on the next paint; cells are untouched.
    pub(in crate::attach) fn kitty_resend(&mut self, pane_id: u32) {
        self.kitty.residency.resend(pane_id);
    }
    /// The outer id this pane's image `pane_id` is shown under, once mapped.
    #[cfg(test)]
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
        if heals_every_paint() {
            self.residency.resend_all();
        }
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

#[cfg(test)]
mod tests {
    use super::ties_images_to_cells;

    /// Terminals that drop an image from the cells text overwrites re-send on
    /// every paint; kitty, Ghostty and the rest keep the savings.
    #[test]
    fn only_terminals_that_tie_images_to_cells_heal_every_paint() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert!(ties_images_to_cells(env(&[("TERM_PROGRAM", "WezTerm")])));
        assert!(ties_images_to_cells(env(&[("KONSOLE_VERSION", "230804")])));
        for other in [
            &[("TERM_PROGRAM", "ghostty")][..],
            &[("TERM", "xterm-kitty")],
            &[("TERM_PROGRAM", "iTerm.app")],
            &[("KONSOLE_VERSION", "")],
            &[],
        ] {
            assert!(!ties_images_to_cells(env(other)), "{other:?}");
        }
    }
}
