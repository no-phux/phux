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
//! honour: only `ED2`, a reset or an alt-screen switch clear images there.
//! Images still go missing behind its back (Ghostty evicting the oldest under
//! its quota, a user clear such as Cmd-K, a terminal that ties images to the
//! cells they cover), so they heal on a bounded schedule: an outer focus-in
//! and [`HEAL_INTERVAL`] after a full send forget the residency (see
//! `pane_state::heal_kitty`), and terminals known to tie images to cells
//! ([`ties_images_to_cells`]) re-send on every paint, as phux did before.

use std::io::{self, Write};
use std::time::Duration;

use tokio::time::Instant;

use libghostty_vt::{Terminal as GhosttyTerminal, kitty::graphics::PlacementIterator};
use phux_protocol::kitty_replay::{self, KittyResidency};

use super::{FrontBuffer, RenderError};

/// The Unicode placeholder (U+10EEEE) a virtual placement's cells print.
const PLACEHOLDER: &str = "\u{10EEEE}";

/// Whether a cell's cluster is a Unicode placeholder (with its diacritics).
pub(super) fn is_placeholder(text: &str) -> bool {
    text.starts_with(PLACEHOLDER)
}

/// How long an image stays on screen before it is re-sent whole.
///
/// One minute bounds a hole a quota eviction or a user clear left to a
/// minute, and costs one compressed payload per visible image per minute
/// (about 4.8 KB a minute for the 96x96 image of `docs/assets/payload.sh`,
/// placement and row repaint included), next to the full re-send on every
/// paint it replaces. Focus-in covers the common case of coming back to the
/// terminal.
const HEAL_INTERVAL: Duration = Duration::from_mins(1);

/// Whether the outer terminal, judged by its environment, drops an image
/// from the cells text overwrites (`WezTerm`, Konsole) instead of keeping it
/// until a clear, so images must be re-placed on every paint.
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
    /// When the outer terminal was last sent every visible image whole;
    /// `None` while the pane shows no image or until the next send.
    sent_at: Option<Instant>,
}

impl PaneKitty<'_> {
    pub(super) fn new() -> Result<Self, RenderError> {
        Ok(Self {
            placements: PlacementIterator::new()?,
            residency: KittyResidency::shared(),
            sent_at: None,
        })
    }

    /// The residency, for the row painter's placeholder rewrite.
    pub(super) const fn residency(&mut self) -> &mut KittyResidency {
        &mut self.residency
    }

    /// Assume the outer terminal may have lost this pane's images.
    pub(super) fn forget(&mut self) {
        self.residency.forget();
        self.sent_at = None;
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
        self.kitty.sent_at = None;
        Ok(())
    }

    /// When this pane's images are next due a heal; `None` while it shows
    /// none.
    pub(in crate::attach) fn kitty_heal_at(&self) -> Option<Instant> {
        self.kitty.sent_at.map(|sent| sent + HEAL_INTERVAL)
    }

    /// Assume the outer terminal lost this pane's images, so the next paint
    /// re-sends them; cells are untouched.
    pub(in crate::attach) fn heal_kitty(&mut self) {
        self.kitty.forget();
    }

    /// How long after a full send this pane's images are due a heal.
    #[cfg(test)]
    pub(crate) const KITTY_HEAL_INTERVAL: Duration = HEAL_INTERVAL;

    /// Pretend the last full send happened one heal interval earlier.
    #[cfg(test)]
    pub(crate) fn backdate_kitty_send(&mut self) {
        self.kitty.sent_at = self
            .kitty
            .sent_at
            .and_then(|at| at.checked_sub(HEAL_INTERVAL));
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
            self.forget();
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
        if !self.residency.holds_images() {
            self.sent_at = None;
        } else if self.sent_at.is_none() {
            self.sent_at = Some(Instant::now());
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
