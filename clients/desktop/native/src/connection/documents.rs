//! Native read-only document decoding. Content never confers terminal authority.
use super::*;
use phux_client_core::session::DocumentPublication;
use phux_protocol::wire::frame::ResourceRecord;

pub(crate) struct Document {
    record: ResourceRecord,
    pub content: Content,
}

pub(crate) enum Content {
    Empty,
    Invalid(&'static str),
    Text(String),
    Image(Arc<gpui::RenderImage>),
}

pub(crate) fn is_document(kind: ResourceKind) -> bool {
    matches!(
        kind,
        ResourceKind::AccessibilityTree | ResourceKind::PixelSurface
    )
}

impl Connection {
    pub(super) fn publish_document(&mut self, publication: &DocumentPublication) {
        let record = self.with_organization(|replica| {
            replica.resource_by_live(&publication.resource_id).cloned()
        });
        let Some(record) =
            record.filter(|record| record.kind == publication.kind && is_document(record.kind))
        else {
            return;
        };
        self.documents.insert(
            publication.resource_id.clone(),
            Document {
                content: decode(record.kind, &publication.bytes),
                record,
            },
        );
    }

    pub(super) fn retain_documents(&mut self) {
        let client = &self.client;
        self.documents.retain(|id, document| {
            client.with_control(|control| {
                control
                    .organization()
                    .resource_by_live(id)
                    .is_some_and(|record| {
                        record.key == document.record.key
                            && record.kind == document.record.kind
                            && record.activation == document.record.activation
                    })
            })
        });
    }

    pub(crate) fn document_content(&self, id: &ResourceId) -> Option<&Content> {
        if self.status != Status::Attached {
            return None;
        }
        self.documents.get(id).map(|document| &document.content)
    }
}

fn decode(kind: ResourceKind, bytes: &[u8]) -> Content {
    if bytes.is_empty() {
        return Content::Empty;
    }
    match kind {
        ResourceKind::AccessibilityTree => serde_json::from_slice::<serde_json::Value>(bytes)
            .ok()
            .and_then(|value| serde_json::to_string_pretty(&value).ok())
            .map_or(
                Content::Invalid("Invalid accessibility JSON"),
                Content::Text,
            ),
        ResourceKind::PixelSurface => {
            surface_image(bytes).map_or(Content::Invalid("Invalid PNG frame"), Content::Image)
        }
        _ => Content::Invalid("Unsupported document kind"),
    }
}

fn surface_image(png: &[u8]) -> Option<Arc<gpui::RenderImage>> {
    let mut buffer = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .ok()?
        .to_rgba8();
    for pixel in buffer.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Some(Arc::new(gpui::RenderImage::new([image::Frame::new(
        buffer,
    )])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_invalid_documents_have_explicit_states() {
        for kind in [ResourceKind::AccessibilityTree, ResourceKind::PixelSurface] {
            assert!(matches!(decode(kind, b""), Content::Empty));
            assert!(matches!(
                decode(kind, b"not a document"),
                Content::Invalid(_)
            ));
        }
    }
}
