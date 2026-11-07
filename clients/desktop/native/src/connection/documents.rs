//! Native read-only document decoding. Content never confers terminal authority.
use super::*;
use phux_client_core::document_input::{DocumentInputs, Refusal};
use phux_client_core::session::DocumentPublication;
use phux_protocol::wire::frame::ResourceRecord;
use phux_protocol::wire::frame::{CommandResult, DocumentInput};

mod store;
pub(crate) use store::Documents;

pub(crate) struct Document {
    record: ResourceRecord,
    /// A typed document's content type, which picks how it is drawn.
    content_type: Option<String>,
    pub content: Content,
}

/// Which documents take pane input, the sends awaiting a reply, and the
/// refusals not yet said.
#[derive(Default)]
pub(crate) struct Input {
    pub(super) inputs: DocumentInputs,
    refusals: Vec<Refusal>,
}

pub(crate) enum Content {
    Empty,
    Invalid(&'static str),
    Text(String),
    Image(Arc<gpui::RenderImage>),
    /// A surface frame or typed image waiting for the decode worker.
    Pending(Vec<u8>),
}

pub(crate) fn is_document(kind: ResourceKind) -> bool {
    matches!(
        kind,
        ResourceKind::AccessibilityTree | ResourceKind::PixelSurface | ResourceKind::Document
    )
}

/// The image format a document of `kind` and `content_type` decodes as,
/// if it is an image. A surface frame is a PNG or a JPEG, picked by its
/// signature when it is decoded.
fn image_format(kind: ResourceKind, content_type: Option<&str>) -> Option<image::ImageFormat> {
    if kind == ResourceKind::PixelSurface {
        return Some(image::ImageFormat::Png);
    }
    match essence(content_type?).as_str() {
        "image/png" => Some(image::ImageFormat::Png),
        "image/jpeg" => Some(image::ImageFormat::Jpeg),
        "image/gif" => Some(image::ImageFormat::Gif),
        "image/webp" => Some(image::ImageFormat::WebP),
        _ => None,
    }
}

fn essence(content_type: &str) -> String {
    phux_protocol::wire::frame::typed_document::essence_of(content_type).to_ascii_lowercase()
}

impl Connection {
    pub(super) fn publish_document(&mut self, publication: &DocumentPublication) {
        let record = self.document_request_record(&publication.resource_id);
        let Some(record) =
            record.filter(|record| record.kind == publication.kind && is_document(record.kind))
        else {
            return;
        };
        let id = publication.resource_id.clone();
        let content_type = publication.content_type.clone();
        if image_format(record.kind, content_type.as_deref()).is_some() {
            let content = Content::Pending(publication.bytes.clone());
            let document = Document {
                record,
                content_type,
                content,
            };
            self.documents.submit(id, document);
            return;
        }
        let content = decode(record.kind, content_type.as_deref(), &publication.bytes);
        let document = Document {
            record,
            content_type,
            content,
        };
        self.documents.insert(id, document);
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

    /// The frame size, in pixels, of a document pane that takes pointer and
    /// keys: an image whose producer declared a control class and has not
    /// refused this client. `None` keeps the pane view-only.
    pub(crate) fn document_input_frame(&self, id: &ResourceId) -> Option<[u32; 2]> {
        if !self.document_input.inputs.accepts(id) {
            return None;
        }
        let Some(Content::Image(image)) = self.document_content(id) else {
            return None;
        };
        let size = image.size(0);
        Some([size.width.0, size.height.0].map(|side| u32::try_from(side).unwrap_or(0)))
    }

    /// Why a document will not take input from this window, once it said.
    pub(crate) fn document_input_refused(&self, id: &ResourceId) -> Option<&'static str> {
        self.document_input.inputs.refused(id)
    }

    /// Send `intent` to the producer of `id`, when its pane takes input
    /// and the connection is up. The reply settles in [`Self::drain`].
    pub(crate) fn send_document_input(&mut self, id: &ResourceId, intent: DocumentInput) -> bool {
        if self.closed || self.status != Status::Attached || self.document_input_frame(id).is_none()
        {
            return false;
        }
        let command = DocumentInputs::command(id, intent);
        let request = self
            .client
            .with_control(|control| control.send_command(command));
        self.document_input.inputs.sent(request, id.clone());
        self.client.wake();
        true
    }

    pub(super) fn answer_document_input(&mut self, request_id: u32, result: &CommandResult) {
        match self.document_input.inputs.answer(request_id, result) {
            Ok(Some(refusal)) => self.document_input.refusals.push(refusal),
            Ok(None) => {}
            Err(error) => log::warn!("document input not delivered: {error}"),
        }
    }

    /// Documents that refused input since the last call, each once.
    pub(crate) fn take_document_refusals(&mut self) -> Vec<Refusal> {
        std::mem::take(&mut self.document_input.refusals)
    }

    pub(crate) fn document_content(&self, id: &ResourceId) -> Option<&Content> {
        if !self.subscription_ready(id) {
            return None;
        }
        self.documents.get(id).map(|document| &document.content)
    }
}

fn decode(kind: ResourceKind, content_type: Option<&str>, bytes: &[u8]) -> Content {
    if bytes.is_empty() {
        return Content::Empty;
    }
    if kind == ResourceKind::Document {
        return decode_typed(content_type.unwrap_or_default(), bytes);
    }
    match kind {
        ResourceKind::AccessibilityTree => pretty_json(bytes).map_or(
            Content::Invalid("Invalid accessibility JSON"),
            Content::Text,
        ),
        ResourceKind::PixelSurface => image::guess_format(bytes)
            .ok()
            .filter(|format| matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg))
            .and_then(|format| surface_image(bytes, format))
            .map_or(
                Content::Invalid("Invalid PNG or JPEG frame"),
                Content::Image,
            ),
        _ => Content::Invalid("Unsupported document kind"),
    }
}

/// A typed document by its content type: images drawn, JSON pretty, text
/// and `+xml`/`+json` as source, anything else a line saying what it is.
/// Markdown and HTML are shown as source; no engine renders them.
fn decode_typed(content_type: &str, bytes: &[u8]) -> Content {
    let essence = essence(content_type);
    if let Some(format) = image_format(ResourceKind::Document, Some(content_type)) {
        return surface_image(bytes, format)
            .map_or(Content::Invalid("Invalid image"), Content::Image);
    }
    if essence == "application/json" {
        return pretty_json(bytes).map_or(Content::Invalid("Invalid JSON"), Content::Text);
    }
    if essence.starts_with("text/") || essence.ends_with("+xml") || essence.ends_with("+json") {
        return String::from_utf8(bytes.to_vec())
            .map_or(Content::Invalid("Invalid UTF-8 text"), Content::Text);
    }
    Content::Text(format!("{} bytes of {content_type}", bytes.len()))
}

fn pretty_json(bytes: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

fn surface_image(bytes: &[u8], format: image::ImageFormat) -> Option<Arc<gpui::RenderImage>> {
    let mut buffer = image::load_from_memory_with_format(bytes, format)
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
    fn a_replaced_or_closed_frame_is_handed_back_for_the_atlas() {
        let (wake, _rx) = async_channel::bounded(1);
        let mut documents = Documents::new(wake);
        let record = ResourceRecord::new(
            phux_protocol::ids::ResourceKey::new([4; 16]).unwrap(),
            phux_protocol::ids::SessionKey::new([2; 16]).unwrap(),
            phux_protocol::ids::Revision::new(1),
        )
        .with_kind(ResourceKind::PixelSurface);
        let png = crate::workspace::view_tests::documents::png();
        for _ in 0..2 {
            let content = Content::Pending(png.clone());
            documents.submit(
                ResourceId::local(9),
                Document {
                    record: record.clone(),
                    content_type: None,
                    content,
                },
            );
        }
        assert_eq!(documents.take_retired().len(), 1, "the first frame retires");
        documents.remove(&ResourceId::local(9));
        assert_eq!(
            documents.take_retired().len(),
            1,
            "closing retires the last"
        );
    }

    #[test]
    fn empty_and_invalid_documents_have_explicit_states() {
        for kind in [ResourceKind::AccessibilityTree, ResourceKind::PixelSurface] {
            assert!(matches!(decode(kind, None, b""), Content::Empty));
            assert!(matches!(
                decode(kind, None, b"not a document"),
                Content::Invalid(_)
            ));
        }
    }

    #[test]
    fn a_surface_frame_decodes_as_a_png_or_a_jpeg() {
        let mut jpeg = Vec::new();
        image::RgbImage::from_pixel(5, 3, image::Rgb([255, 0, 0]))
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let png = crate::workspace::view_tests::documents::png();
        for frame in [&jpeg, &png] {
            let Content::Image(image) = decode(ResourceKind::PixelSurface, None, frame) else {
                panic!("a surface frame decodes");
            };
            assert!(image.size(0).width.0 > 0);
        }
        let gif = b"GIF89a\x05\x00\x03\x00\x00\x00\x00;";
        assert!(matches!(
            decode(ResourceKind::PixelSurface, None, gif),
            Content::Invalid(_)
        ));
    }

    #[test]
    fn a_typed_document_is_drawn_by_its_content_type() {
        let typed = |content_type: &str, bytes: &[u8]| {
            decode(ResourceKind::Document, Some(content_type), bytes)
        };
        let png = crate::workspace::view_tests::documents::png();
        assert!(matches!(typed("image/png", &png), Content::Image(_)));
        assert!(matches!(typed("image/jpeg", b"nope"), Content::Invalid(_)));
        assert!(matches!(typed("application/json", b"{\"a\":1}"),
            Content::Text(text) if text == "{\n  \"a\": 1\n}"));
        for content_type in ["text/markdown", "text/html; charset=utf-8", "image/svg+xml"] {
            assert!(matches!(typed(content_type, b"<b>hi</b>"),
                Content::Text(text) if text == "<b>hi</b>"));
        }
        assert!(matches!(typed("text/csv", b"\xff"), Content::Invalid(_)));
        assert!(matches!(typed("application/pdf", b"%PDF-1.7"),
            Content::Text(text) if text == "8 bytes of application/pdf"));
        assert!(matches!(typed("text/plain", b""), Content::Empty));
        assert_eq!(
            image_format(ResourceKind::Document, Some("Image/WebP")),
            Some(image::ImageFormat::WebP)
        );
    }
}
