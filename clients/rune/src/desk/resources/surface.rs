//! Latest-frame PNG/JPEG decoding. Rune's cache owns queued jobs even when the UI
//! drops an Image, so submitting every screencast frame makes an unbounded
//! backlog. Each document retains only the latest bytes; cache-wide admission
//! accounting survives document retirement and bounds outstanding work too.

use super::{Body, Document, Resources};

// Rune has at most four native decoder workers. Keep this many outstanding
// document jobs across the entire cache, including jobs whose documents died.
pub(super) const MAX_DECODES: usize = 4;

#[derive(Default)]
pub(super) struct Decodes {
    outstanding: Vec<rune::Image>,
    #[cfg(test)]
    pub(super) started: usize,
}

impl Decodes {
    pub(super) fn reap(&mut self) {
        self.outstanding.retain(|image| !complete(image));
    }

    fn available(&mut self) -> bool {
        self.reap();
        self.outstanding.len() < MAX_DECODES
    }

    fn load(&mut self, cache: &rune::ImageCache, bytes: &[u8]) -> Option<rune::Image> {
        if !self.available() {
            return None;
        }
        let image = cache.load(bytes.to_vec());
        self.outstanding.push(image.clone());
        #[cfg(test)]
        {
            self.started += 1;
        }
        Some(image)
    }
}

impl Resources {
    /// A window renewal reuses the cache, so it must also reuse its admission
    /// accounting, even though every old resource/document is discarded.
    pub(in crate::desk) fn carry_images(&mut self, next: &mut Resources) {
        next.images = self.images.take();
        next.decodes = std::mem::take(&mut self.decodes);
    }
}

#[derive(Default)]
pub(super) struct Surface {
    pub(super) flight: Option<Decode>,
    pub(super) pending: Option<Vec<u8>>,
    #[cfg(test)]
    pub(super) started: usize,
}

pub(super) struct Decode {
    pub(super) image: rune::Image,
    present: bool,
}

fn complete(image: &rune::Image) -> bool {
    // Rune publishes its decode result before setting Pixels/mips. A
    // successful replacement must have actual uploadable pixels too.
    image.is_ready() && (image.error().is_some() || image.pixels().size() != (0, 0))
}

impl Document {
    pub(super) fn publish_surface(
        &mut self,
        bytes: &[u8],
        images: Option<&rune::ImageCache>,
        decodes: &mut Decodes,
    ) {
        if bytes.is_empty() {
            self.invalidate_surface();
            self.body = Body::Empty;
            return;
        }
        // The cache accepts SVG too; this resource contract accepts PNG or
        // JPEG only. Full validation, 8192-side / 256 MiB decode limits, and
        // mip generation remain on Rune's real decoder workers.
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") && !bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            self.invalidate_surface();
            self.body = Body::Invalid("Invalid frame: not a PNG or JPEG".to_owned());
            return;
        }
        let Some(cache) = images else {
            self.invalidate_surface();
            self.body = Body::Invalid("Image decoder unavailable; reopen this window.".to_owned());
            return;
        };
        if self.surface.flight.is_some() {
            self.surface.pending = Some(bytes.to_vec());
        } else {
            self.start_surface(bytes, cache, decodes);
        }
    }

    fn start_surface(&mut self, bytes: &[u8], cache: &rune::ImageCache, decodes: &mut Decodes) {
        let image = decodes.load(cache, bytes);
        if matches!(self.body, Body::Pending | Body::Empty) {
            self.body = Body::Decoding;
        }
        let Some(image) = image else {
            self.surface.pending = Some(bytes.to_vec());
            return;
        };
        self.surface.pending = None;
        // Keep the last completed pixels while the next image decodes. An
        // error stays visible until a later valid image actually completes.
        if matches!(self.body, Body::Decoding) {
            self.body = Body::Surface(image.clone());
        }
        self.surface.flight = Some(Decode {
            image,
            present: true,
        });
        #[cfg(test)]
        {
            self.surface.started += 1;
        }
    }

    pub(super) fn finish_surface(
        &mut self,
        images: Option<&rune::ImageCache>,
        decodes: &mut Decodes,
    ) -> bool {
        if self
            .surface
            .flight
            .as_ref()
            .is_some_and(|job| !complete(&job.image))
        {
            return false;
        }
        let job = self.surface.flight.take().filter(|job| job.present);
        let mut changed = job.is_some();
        if let Some(job) = job {
            self.body = match job.image.error() {
                Some(error) => Body::Invalid(format!("Invalid PNG or JPEG: {error}")),
                None => Body::Surface(job.image),
            };
        }
        if self.surface.pending.is_some() && !decodes.available() {
            return changed;
        }
        let pending = self.surface.pending.take();
        if let Some(bytes) = pending {
            let decoding = matches!(self.body, Body::Pending | Body::Empty);
            self.publish_surface(&bytes, images, decodes);
            changed |= self.surface.flight.is_some() || decoding;
        }
        changed
    }

    /// Do not orphan a running cache job and immediately queue another. Let
    /// it finish, but never let its obsolete pixels overwrite empty/error or
    /// detached state. Dropping Document drops the pending bytes entirely.
    pub(in crate::desk) fn invalidate_surface(&mut self) {
        self.surface.pending = None;
        if let Some(job) = &mut self.surface.flight {
            job.present = false;
        }
    }
}
