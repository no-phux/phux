//! When the TUI re-sends kitty images the outer terminal may have dropped.
//!
//! The pane renderers never re-send an unchanged image, which kitty and
//! Ghostty honour: only `ED2`, a reset or an alt-screen switch clear images
//! there. Images still go missing behind their back (Ghostty evicting the
//! oldest under its quota, a user clear such as Cmd-K), so they heal:
//!
//! - each image [`HEAL_INTERVAL`] after it was last sent, while the outer
//!   terminal is focused or has never reported focus (an unfocused window's
//!   holes cannot be seen, and focus-in heals them);
//! - on focus-in, each image sent more than [`FOCUS_MIN_AGE`] ago;
//! - at most [`HEAL_BUDGET`] bytes per heal, oldest first; the rest is owed
//!   and healed [`HEAL_SPACING`] later, so a surface of hundreds of tiles
//!   never bursts megabytes at once.
//!
//! A probe (`a=p,q=1` and an `ENOENT` reply) could re-send only what the
//! outer terminal lost, but the input parser drops APC replies, so the heal
//! re-sends blind.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use phux_protocol::ids::ResourceId;

use super::pane_state::PaneSlot;

/// How long an image stays on screen before it is re-sent whole.
///
/// One minute bounds a hole a quota eviction or a user clear left to a
/// minute, and costs one compressed payload per visible image per minute
/// (about 4.4 KB a minute for the 96x96 image of `docs/assets/payload.sh`).
pub(super) const HEAL_INTERVAL: Duration = Duration::from_mins(1);

/// How old an image must be for focus-in to re-send it: an alt-tab bounce or
/// focus following the mouse re-sends nothing, a real absence heals.
pub(super) const FOCUS_MIN_AGE: Duration = Duration::from_secs(5);

/// Bytes one heal may re-send; the oldest image always goes, however big.
///
/// 256 KiB is a minute's heal of about 60 typical inline images in one
/// go, and a fifth of a second on a 10 Mbit/s SSH link.
pub(super) const HEAL_BUDGET: usize = 256 * 1024;

/// The least time between two heals, which paces an owed remainder at
/// [`HEAL_BUDGET`] a second.
pub(super) const HEAL_SPACING: Duration = Duration::from_secs(1);

/// The heal schedule across every pane of one attached session.
#[derive(Debug, Default)]
pub(in crate::attach) struct KittyHeal {
    /// The outer terminal's focus, once it has reported one.
    focused: Option<bool>,
    /// When the last heal (or a deferred one) ran.
    last: Option<Instant>,
    /// Images sent at or before this still owe a heal the budget cut short.
    owed: Option<Instant>,
}

/// One held image a heal may re-send.
struct Due {
    sent: Instant,
    bytes: usize,
    pane: ResourceId,
    image: u32,
}

impl KittyHeal {
    /// The outer terminal reported its focus.
    pub(in crate::attach) const fn note_focus(&mut self, focused: bool) {
        self.focused = Some(focused);
    }

    /// When the next timed heal is due; `None` while nothing is or the
    /// outer terminal is unfocused.
    pub(in crate::attach) fn deadline(
        &self,
        panes: &HashMap<ResourceId, PaneSlot>,
    ) -> Option<Instant> {
        if self.focused == Some(false) {
            return None;
        }
        let due = match self.owed {
            Some(_) => self.last.map(|last| last + HEAL_SPACING),
            None => oldest_send(panes).map(|sent| sent + HEAL_INTERVAL),
        }?;
        Some(self.not_before().map_or(due, |earliest| due.max(earliest)))
    }

    /// Mark the images the timer owes a heal for re-sending; `true` when any
    /// was, and the visible panes must repaint.
    pub(in crate::attach) fn heal_due(
        &mut self,
        panes: &mut HashMap<ResourceId, PaneSlot>,
        now: Instant,
    ) -> bool {
        if self.focused == Some(false) {
            return false;
        }
        self.heal(panes, now, now.checked_sub(HEAL_INTERVAL))
    }

    /// Mark the images focus-in heals for re-sending; `true` when any was.
    pub(in crate::attach) fn heal_on_focus(
        &mut self,
        panes: &mut HashMap<ResourceId, PaneSlot>,
        now: Instant,
    ) -> bool {
        self.heal(panes, now, now.checked_sub(FOCUS_MIN_AGE))
    }

    /// A heal came due while nothing can paint (an overlay, a held switch):
    /// try again a spacing later instead of marking images now.
    pub(in crate::attach) const fn defer(&mut self, now: Instant) {
        self.last = Some(now);
    }

    fn not_before(&self) -> Option<Instant> {
        self.last.map(|last| last + HEAL_SPACING)
    }

    /// Re-send, oldest first and within [`HEAL_BUDGET`], every held image
    /// sent at or before `cutoff` or still owed.
    fn heal(
        &mut self,
        panes: &mut HashMap<ResourceId, PaneSlot>,
        now: Instant,
        cutoff: Option<Instant>,
    ) -> bool {
        if self.not_before().is_some_and(|earliest| now < earliest) {
            return false;
        }
        let Some(cutoff) = cutoff.max(self.owed) else {
            return false;
        };
        self.owed = None;
        let mut due = due_images(panes, cutoff);
        if due.is_empty() {
            return false;
        }
        due.sort_unstable_by_key(|image| image.sent);
        self.last = Some(now);
        let mut spent = 0;
        for (index, image) in due.into_iter().enumerate() {
            if index > 0 && spent + image.bytes > HEAL_BUDGET {
                self.owed = Some(cutoff);
                break;
            }
            spent += image.bytes;
            if let Some(slot) = panes.get_mut(&image.pane) {
                slot.renderer.kitty_resend(image.image);
            }
        }
        true
    }
}

fn oldest_send(panes: &HashMap<ResourceId, PaneSlot>) -> Option<Instant> {
    panes
        .values()
        .flat_map(|slot| slot.renderer.kitty_sends())
        .map(|send| send.sent)
        .min()
}

fn due_images(panes: &HashMap<ResourceId, PaneSlot>, cutoff: Instant) -> Vec<Due> {
    let mut due = Vec::new();
    for (pane, slot) in panes {
        for send in slot.renderer.kitty_sends() {
            if send.sent <= cutoff {
                due.push(Due {
                    sent: send.sent,
                    bytes: send.bytes,
                    pane: pane.clone(),
                    image: send.pane_id,
                });
            }
        }
    }
    due
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests;
