//! Latest-frame PNG decoding. Rune's cache owns queued jobs even when the UI
//! drops an Image, so submitting every screencast frame makes an unbounded
//! backlog. Each document submits one job and retains only the latest bytes.

use super::{Body, Document};

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

impl Decode {
    fn complete(&self) -> bool {
        // Rune publishes its decode result before setting Pixels/mips. A
        // successful replacement must have actual uploadable pixels too.
        self.image.is_ready()
            && (self.image.error().is_some() || self.image.pixels().size() != (0, 0))
    }
}

impl Document {
    pub(super) fn publish_surface(&mut self, bytes: &[u8], images: Option<&rune::ImageCache>) {
        if bytes.is_empty() {
            self.invalidate_surface();
            self.body = Body::Empty;
            return;
        }
        // The cache accepts JPEG/SVG too; this resource contract accepts PNG
        // only. Full validation, 8192-side / 256 MiB decode limits, and mip
        // generation remain on Rune's real decoder workers.
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            self.invalidate_surface();
            self.body = Body::Invalid("Invalid PNG signature".to_owned());
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
            self.start_surface(bytes, cache);
        }
    }

    fn start_surface(&mut self, bytes: &[u8], cache: &rune::ImageCache) {
        let image = cache.load(bytes.to_vec());
        // Keep the last completed pixels while the next image decodes. An
        // error stays visible until a later valid image actually completes.
        if matches!(self.body, Body::Pending | Body::Empty) {
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

    pub(super) fn finish_surface(&mut self, images: Option<&rune::ImageCache>) -> bool {
        if !self.surface.flight.as_ref().is_some_and(Decode::complete) {
            return false;
        }
        let job = self.surface.flight.take().expect("ready decode");
        if job.present {
            self.body = match job.image.error() {
                Some(error) => Body::Invalid(format!("Invalid PNG: {error}")),
                None => Body::Surface(job.image),
            };
        }
        let pending = self.surface.pending.take();
        let changed = job.present || pending.is_some();
        if let Some(bytes) = pending {
            self.publish_surface(&bytes, images);
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
