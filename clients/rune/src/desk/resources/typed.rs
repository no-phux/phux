//! Documents drawn by content type, as the desktop does (#2023), through
//! the one decode every kind shares (`phux_client_core::document_view`):
//! images through the surface decoder, JSON pretty-printed, `text/*` and
//! `+xml`/`+json` as source (Markdown and HTML stay source), anything
//! else a line naming its size and type.

use phux_client_core::document_view::{self, DocumentContent};
use phux_client_core::session::DocumentPublication;

use super::{Body, Document, surface};

impl Document {
    /// Draw a newer publication of any document kind. The caption detail
    /// is the declared content type, plus how much is shown of cut text.
    pub(super) fn publish_content(
        &mut self,
        publication: &DocumentPublication,
        images: Option<&rune::ImageCache>,
        decodes: &mut surface::Decodes,
    ) {
        self.detail = publication.content_type.clone();
        let content = document_view::decode(
            publication.kind,
            publication.content_type.as_deref(),
            &publication.bytes,
        );
        let (body, cut) = match content {
            DocumentContent::Image(_) => {
                return self.publish_image(&publication.bytes, images, decodes);
            }
            DocumentContent::Empty => (Body::Empty, None),
            DocumentContent::Text { text, cut } => (Body::Tree(text), cut),
            DocumentContent::Opaque { content_type, len } => {
                (Body::Tree(format!("{len} bytes of {content_type}")), None)
            }
            DocumentContent::Invalid(why) => (Body::Invalid(why), None),
        };
        self.invalidate_surface();
        self.body = body;
        if let Some(cut) = cut {
            self.detail = Some(match self.detail.take() {
                Some(declared) => format!("{declared} · {cut}"),
                None => cut,
            });
        }
    }
}
