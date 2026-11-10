//! What an outer terminal already holds of one pane's Kitty graphics.
//!
//! libghostty stores only decoded pixels, so re-transmitting a visible image
//! on every paint costs its full raw size each time. The live replay instead
//! diffs the pane's visible images against this record: libghostty's
//! per-image generation stamp is the content identity, and the placement
//! commands last written (origin applied) are compared byte for byte.

use std::{
    io::{self, Write},
    mem,
    ops::Range,
    time::Instant,
};

use libghostty_vt::{
    kitty::graphics::Graphics,
    style::{RgbColor, StyleColor},
};

use super::{
    KittyReplayError, ReplayPlacement,
    ids::{self, IdMap},
    image_payload, write_delete_image, write_delete_placements, write_image_apc,
};

/// libghostty never stamps a stored image with generation zero, so zero
/// marks an id the outer terminal may hold with unknown content.
const UNKNOWN_GENERATION: u64 = 0;

/// The Kitty images and placements one outer terminal holds for one pane,
/// as [`super::emit_kitty_graphics_replay`] last left them.
///
/// Own one per pane renderer per outer terminal. A fresh value assumes the
/// outer terminal holds nothing of this pane. The default sends pane image
/// ids unchanged, for an outer terminal that shows this pane alone; panes
/// sharing one outer terminal each use [`Self::shared`].
#[derive(Debug, Default)]
pub struct KittyResidency {
    held: Resident,
    /// Scratch for the replay being staged, swapped with `held` on success
    /// so steady-state replays do not allocate.
    next: Resident,
    /// Pane-to-outer image ids; `None` sends pane ids as they are.
    ids: Option<IdMap>,
}

#[derive(Debug, Default)]
struct Resident {
    /// Sorted by image id.
    images: Vec<ResidentImage>,
    /// Every image's placement commands, back to back.
    placements: Vec<u8>,
}

#[derive(Debug)]
struct ResidentImage {
    id: u32,
    /// The id the outer terminal knows this image by.
    outer: u32,
    generation: u64,
    /// This image's commands within [`Resident::placements`].
    placements: Range<usize>,
    /// The image has (or may have) a classic, cell-anchored placement.
    classic: bool,
    /// When its pixels were last transmitted, and how many bytes that took.
    sent: Option<Instant>,
    bytes: usize,
}

/// One image the outer terminal holds, as [`KittyResidency::sends`] reports
/// it for a heal policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSend {
    /// The pane's id for the image ([`KittyResidency::resend`] takes it).
    pub pane_id: u32,
    /// When its pixels were last transmitted.
    pub sent: Instant,
    /// The bytes that transmission wrote.
    pub bytes: usize,
}

impl Resident {
    fn clear(&mut self) {
        self.images.clear();
        self.placements.clear();
    }

    fn get(&self, id: u32) -> Option<&ResidentImage> {
        self.images
            .binary_search_by_key(&id, |image| image.id)
            .ok()
            .map(|index| &self.images[index])
    }

    fn placements_of(&self, image: &ResidentImage) -> &[u8] {
        &self.placements[image.placements.clone()]
    }
}

impl KittyResidency {
    /// A residency for one of several panes sharing an outer terminal: its
    /// image ids are mapped into outer ids no other shared residency on this
    /// thread uses, stable for the residency's life. Dropping it while it
    /// still holds images queues them for [`super::write_orphaned_deletes`].
    #[must_use]
    pub fn shared() -> Self {
        Self {
            held: Resident::default(),
            next: Resident::default(),
            ids: Some(IdMap::default()),
        }
    }

    /// The id the outer terminal knows pane image `pane_id` by, once mapped.
    #[must_use]
    pub fn outer_id(&self, pane_id: u32) -> Option<u32> {
        self.ids
            .as_ref()
            .map_or(Some(pane_id), |ids| ids.get(pane_id))
    }

    /// The foreground a Unicode placeholder cell painted with `fg` must use on
    /// the outer terminal: its image id's low 24 bits, mapped, as truecolour.
    /// The placement id (underline colour) and the high byte (third
    /// diacritic) need no rewrite.
    pub fn placeholder_fg(&mut self, fg: StyleColor) -> RgbColor {
        ids::placeholder_color(self.ids.as_mut(), fg)
    }

    /// The images the outer terminal holds with known content, with when and
    /// at what cost each was last transmitted.
    pub fn sends(&self) -> impl Iterator<Item = ImageSend> + '_ {
        self.held
            .images
            .iter()
            .filter(|image| image.generation != UNKNOWN_GENERATION)
            .filter_map(|image| {
                Some(ImageSend {
                    pane_id: image.id,
                    sent: image.sent?,
                    bytes: image.bytes,
                })
            })
    }

    /// Re-transmit pane image `pane_id` on the next replay, which the outer
    /// terminal may have dropped. Unlike [`Self::forget`] its placements are
    /// still known, so an image shown only through Unicode placeholders is
    /// re-sent without reporting touched cells.
    pub fn resend(&mut self, pane_id: u32) {
        if let Ok(index) = self.held.images.binary_search_by_key(&pane_id, |i| i.id) {
            self.held.images[index].generation = UNKNOWN_GENERATION;
        }
    }

    /// [`Self::resend`] every held image.
    pub fn resend_all(&mut self) {
        for image in &mut self.held.images {
            image.generation = UNKNOWN_GENERATION;
        }
    }

    /// Every on-screen placeholder cell is about to be painted again (a
    /// forced paint): forget which ids earlier paints spelled.
    pub fn begin_full_walk(&mut self) {
        if let Some(ids) = &mut self.ids {
            ids.begin_full_walk();
        }
    }

    /// Whether the outer terminal holds (or may hold) any of this pane's
    /// images.
    #[must_use]
    pub const fn holds_images(&self) -> bool {
        !self.held.images.is_empty()
    }

    /// Delete (`d=I`) every image the outer terminal holds for this pane and
    /// record that it holds nothing: the pane left the visible layout. Its id
    /// mapping is kept. Returns whether a classic placement was removed.
    pub fn release(&mut self, out: &mut impl Write) -> io::Result<bool> {
        for image in &self.held.images {
            write_delete_image(out, image.outer)?;
        }
        let touched_cells = self.held.images.iter().any(|image| image.classic);
        self.held.clear();
        Ok(touched_cells)
    }

    /// Forget what the outer terminal shows: after a screen clear, a dropped
    /// or unshipped frame, or anything else that may have removed images or
    /// placements behind the replay's back. The next replay re-transmits
    /// every visible image (deleting the old copy first) and still deletes
    /// the images that are gone.
    pub fn forget(&mut self) {
        for image in &mut self.held.images {
            image.generation = UNKNOWN_GENERATION;
            image.placements = 0..0;
            image.classic = true;
        }
        self.held.placements.clear();
    }

    /// Emit what the outer terminal lacks of `replay`, then record it as
    /// held. Returns whether a classic placement was written or removed.
    pub(super) fn replay(
        &mut self,
        graphics: &Graphics<'_>,
        replay: &[ReplayPlacement],
        origin: (u16, u16),
        out: &mut impl Write,
    ) -> Result<bool, KittyReplayError> {
        if let Err(error) = self.stage(graphics, replay, origin) {
            self.next.clear();
            return Err(error);
        }
        match self.sync(graphics, out) {
            Ok(touched_cells) => {
                mem::swap(&mut self.held, &mut self.next);
                // Best effort: a failed scan leaves extra mappings until the
                // next replay, and must not hide what was written.
                let _ = self.prune_ids(graphics);
                Ok(touched_cells)
            }
            Err(error) => {
                self.forget_after_failure();
                Err(error)
            }
        }
    }

    /// Build the wanted state in `next`: every image with a transmittable
    /// payload and its rendered placement commands.
    fn stage(
        &mut self,
        graphics: &Graphics<'_>,
        replay: &[ReplayPlacement],
        origin: (u16, u16),
    ) -> Result<(), KittyReplayError> {
        self.next.clear();
        for group in replay.chunk_by(|a, b| a.image_id == b.image_id) {
            let id = group[0].image_id;
            let Some(image) = graphics.image(id) else {
                continue;
            };
            if image_payload(&image)?.is_none() {
                continue;
            }
            let outer = self.ids.as_mut().map_or(id, |ids| ids.outer(id));
            let start = self.next.placements.len();
            for placement in group {
                placement.write(&mut self.next.placements, origin, outer)?;
            }
            self.next.images.push(ResidentImage {
                id,
                outer,
                generation: image.generation()?,
                placements: start..self.next.placements.len(),
                classic: group.iter().any(|p| !p.virtual_placement),
                sent: None,
                bytes: 0,
            });
        }
        Ok(())
    }

    fn sync(
        &mut self,
        graphics: &Graphics<'_>,
        out: &mut impl Write,
    ) -> Result<bool, KittyReplayError> {
        let mut touched_cells = self.delete_departed(out)?;
        let Resident { images, placements } = &mut self.next;
        for image in images {
            let wanted = &placements[image.placements.clone()];
            touched_cells |= sync_image(&self.held, graphics, image, wanted, out)?;
        }
        Ok(touched_cells)
    }

    /// Delete held images that are no longer visibly placed, so the outer
    /// terminal does not accumulate them.
    fn delete_departed(&self, out: &mut impl Write) -> Result<bool, KittyReplayError> {
        let mut touched_cells = false;
        for image in &self.held.images {
            if self.next.get(image.id).is_none() {
                write_delete_image(out, image.outer)?;
                touched_cells |= image.classic;
            }
        }
        Ok(touched_cells)
    }

    /// A failed write leaves the outer terminal's state unknown: remember
    /// every id either side mentions so a vanished image is still deleted.
    fn forget_after_failure(&mut self) {
        for image in self.next.images.drain(..) {
            if self.held.get(image.id).is_none() {
                self.held.images.push(image);
            }
        }
        self.held.images.sort_unstable_by_key(|image| image.id);
        self.next.placements.clear();
        self.forget();
    }
}

impl KittyResidency {
    /// Past [`IdMap`]'s bound, release the mappings of images neither held
    /// nor stored any more.
    fn prune_ids(&mut self, graphics: &Graphics<'_>) -> Result<(), KittyReplayError> {
        let Some(map) = self.ids.as_mut().filter(|map| map.wants_prune()) else {
            return Ok(());
        };
        let mut stored = Vec::new();
        graphics.image_ids_into(&mut stored)?;
        let held = &self.held.images;
        map.prune(|low| {
            let same = |id: &u32| id & 0x00FF_FFFF == low;
            stored.iter().any(same) || held.iter().any(|image| same(&image.id))
        });
        Ok(())
    }
}

/// Bring one wanted image up to date: nothing when unchanged, its placements
/// alone when only they changed, otherwise pixels and placements.
fn sync_image(
    held: &Resident,
    graphics: &Graphics<'_>,
    image: &mut ResidentImage,
    placements: &[u8],
    out: &mut impl Write,
) -> Result<bool, KittyReplayError> {
    let prior = held.get(image.id);
    match prior {
        Some(prior) if holds_content(prior, image) => {
            (image.sent, image.bytes) = (prior.sent, prior.bytes);
            if held.placements_of(prior) == placements {
                return Ok(false);
            }
            // Anonymous p=0 placements cannot be replaced by ID.
            write_delete_placements(out, image.outer)?;
        }
        _ => {
            if let Some(pixels) = graphics.image(image.id) {
                let mut counted = Counted { out, bytes: 0 };
                write_image_apc(&mut counted, &pixels, image.outer)?;
                (image.sent, image.bytes) = (Some(Instant::now()), counted.bytes);
            }
        }
    }
    out.write_all(placements)?;
    Ok(image.classic || prior.is_some_and(|prior| prior.classic))
}

/// A sink that counts what passes through it.
struct Counted<'a, W> {
    out: &'a mut W,
    bytes: usize,
}

impl<W: Write> Write for Counted<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.out.write(buf)?;
        self.bytes += written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

impl Drop for KittyResidency {
    fn drop(&mut self) {
        if self.ids.is_some() {
            ids::orphan(self.held.images.iter().map(|image| image.outer));
        }
    }
}

const fn holds_content(held: &ResidentImage, wanted: &ResidentImage) -> bool {
    held.generation != UNKNOWN_GENERATION && held.generation == wanted.generation
}
