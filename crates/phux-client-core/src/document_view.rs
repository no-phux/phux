//! What a client draws for a document publication (`docs/resource-kinds.md`).
//!
//! Each publication has one content type (a tree's JSON, the image type a
//! surface frame starts with, a typed document's declared type), and one
//! decode by that type serves every document kind.

use phux_protocol::ids::ResourceKind;
use phux_protocol::wire::frame::typed_document::{essence_of, is_textual};

/// The most text a client lays out at once: a whole 4 MiB document would
/// stall a frame, so text past this is cut.
pub const MAX_TEXT: usize = 256 * 1024;

/// What one document publication draws as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentContent {
    /// No bytes: an empty publication.
    Empty,
    /// An image of this lower-case content type essence (`image/png`,
    /// `image/jpeg`, `image/gif` or `image/webp`) whose bytes start with
    /// that format's signature ([`raster_type`]). The publication's bytes
    /// are the image; the client's decoder checks the rest.
    Image(String),
    /// Source text, JSON pretty-printed.
    Text {
        /// The text, at most [`MAX_TEXT`] bytes.
        text: String,
        /// How much is shown, when the text was cut.
        cut: Option<String>,
    },
    /// Bytes no client draws.
    Opaque {
        /// Their content type, as declared.
        content_type: String,
        /// Their size in bytes.
        len: usize,
    },
    /// Bytes that are not what their content type says.
    Invalid(String),
}

/// Whether `content_type` is an image a client draws: PNG, JPEG, GIF or
/// WebP, whatever its case or parameters.
#[must_use]
pub fn is_image(content_type: &str) -> bool {
    matches!(
        essence_of(content_type).to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

/// The raster format `bytes` start with, as its content type.
///
/// PNG, JPEG, GIF (87a or 89a) or WebP (`RIFF`, a size, `WEBP`). A client
/// hands an image decoder only bytes this names, so SVG or a mislabeled
/// blob never reaches one.
#[must_use]
pub fn raster_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The content type of a `kind` publication of `bytes`.
///
/// JSON for an accessibility tree, PNG or JPEG by signature for a pixel
/// surface, the declared type for a typed document (opaque bytes if it
/// declared none).
///
/// # Errors
///
/// Why none applies: a surface frame that is neither PNG nor JPEG, or a
/// kind that carries no document.
pub fn content_type(
    kind: ResourceKind,
    declared: Option<&str>,
    bytes: &[u8],
) -> Result<String, String> {
    match kind {
        ResourceKind::AccessibilityTree => Ok("application/json".to_owned()),
        ResourceKind::PixelSurface => match raster_type(bytes) {
            Some(png_or_jpeg @ ("image/png" | "image/jpeg")) => Ok(png_or_jpeg.to_owned()),
            _ => Err("Invalid frame: not a PNG or JPEG".to_owned()),
        },
        ResourceKind::Document => Ok(declared.unwrap_or("application/octet-stream").to_owned()),
        _ => Err("Unsupported document kind".to_owned()),
    }
}

/// What a `kind` publication of `bytes` draws as, `declared` being a typed
/// document's content type.
#[must_use]
pub fn decode(kind: ResourceKind, declared: Option<&str>, bytes: &[u8]) -> DocumentContent {
    if bytes.is_empty() {
        return DocumentContent::Empty;
    }
    let content_type = match content_type(kind, declared, bytes) {
        Ok(content_type) => content_type,
        Err(why) => return DocumentContent::Invalid(why),
    };
    let essence = essence_of(&content_type).to_ascii_lowercase();
    if is_image(&essence) {
        // A declared image whose bytes are another format is not drawn.
        if raster_type(bytes) != Some(essence.as_str()) {
            return DocumentContent::Opaque {
                content_type,
                len: bytes.len(),
            };
        }
        return DocumentContent::Image(essence);
    }
    let text = if essence == "application/json" {
        match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_default(),
            Err(error) => return DocumentContent::Invalid(format!("Invalid JSON: {error}")),
        }
    } else if is_textual(&content_type) {
        match String::from_utf8(bytes.to_vec()) {
            Ok(text) => text,
            Err(error) => return DocumentContent::Invalid(format!("Invalid UTF-8 text: {error}")),
        }
    } else {
        return DocumentContent::Opaque {
            content_type,
            len: bytes.len(),
        };
    };
    let (text, cut) = cut(text);
    DocumentContent::Text { text, cut }
}

fn cut(mut text: String) -> (String, Option<String>) {
    if text.len() <= MAX_TEXT {
        return (text, None);
    }
    let total = text.len().div_ceil(1024);
    text.truncate(text.floor_char_boundary(MAX_TEXT));
    let shown = MAX_TEXT / 1024;
    (
        text,
        Some(format!("showing the first {shown} KiB of {total} KiB")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(content: DocumentContent) -> (String, Option<String>) {
        let DocumentContent::Text { text, cut } = content else {
            panic!("text, not {content:?}")
        };
        (text, cut)
    }

    #[test]
    fn each_kind_has_one_content_type() {
        let png = b"\x89PNG\r\n\x1a\nrest";
        let jpeg = [0xff, 0xd8, 0xff, 0];
        let typed = Some("text/markdown");
        for (kind, declared, bytes, expected) in [
            (
                ResourceKind::AccessibilityTree,
                None,
                &b"{}"[..],
                Ok("application/json"),
            ),
            (ResourceKind::PixelSurface, None, png, Ok("image/png")),
            (ResourceKind::PixelSurface, None, &jpeg, Ok("image/jpeg")),
            (ResourceKind::Document, typed, b"x", Ok("text/markdown")),
            (
                ResourceKind::Document,
                None,
                b"x",
                Ok("application/octet-stream"),
            ),
            (
                ResourceKind::PixelSurface,
                None,
                b"GIF89a",
                Err("Invalid frame: not a PNG or JPEG"),
            ),
            (
                ResourceKind::Terminal,
                None,
                b"x",
                Err("Unsupported document kind"),
            ),
            (
                ResourceKind::AgentSession,
                None,
                b"x",
                Err("Unsupported document kind"),
            ),
        ] {
            let expected = expected.map(str::to_owned).map_err(str::to_owned);
            assert_eq!(content_type(kind, declared, bytes), expected, "{kind}");
        }
    }

    #[test]
    fn a_tree_and_a_json_document_decode_the_same_way() {
        let json = br#"{"a":[1]}"#;
        let pretty = "{\n  \"a\": [\n    1\n  ]\n}".to_owned();
        let tree = decode(ResourceKind::AccessibilityTree, None, json);
        let typed = decode(ResourceKind::Document, Some("Application/JSON; q=1"), json);
        assert_eq!(tree, typed);
        assert_eq!(text(tree), (pretty, None));
        assert!(matches!(
            decode(ResourceKind::AccessibilityTree, None, b"{nope"),
            DocumentContent::Invalid(error) if error.starts_with("Invalid JSON")
        ));
    }

    #[test]
    fn images_text_and_opaque_bytes() {
        let png = b"\x89PNG\r\n\x1a\nrest";
        assert_eq!(
            decode(ResourceKind::PixelSurface, None, png),
            DocumentContent::Image("image/png".to_owned())
        );
        assert_eq!(
            decode(
                ResourceKind::Document,
                Some("IMAGE/WEBP; q=1"),
                b"RIFF\0\0\0\0WEBPVP8 "
            ),
            DocumentContent::Image("image/webp".to_owned())
        );
        assert!(!is_image("image/svg+xml") && !is_image("image/tiff"));
        let svg = decode(ResourceKind::Document, Some("image/svg+xml"), b"<svg/>");
        assert_eq!(text(svg).0, "<svg/>");
        assert!(matches!(
            decode(ResourceKind::Document, Some("text/plain"), b"\xff"),
            DocumentContent::Invalid(error) if error.starts_with("Invalid UTF-8")
        ));
        assert_eq!(
            decode(ResourceKind::Document, Some("application/pdf"), b"%PDF-1.7"),
            DocumentContent::Opaque {
                content_type: "application/pdf".to_owned(),
                len: 8
            }
        );
        for kind in [ResourceKind::PixelSurface, ResourceKind::Document] {
            assert_eq!(
                decode(kind, Some("text/plain"), b""),
                DocumentContent::Empty
            );
        }
    }

    #[test]
    fn only_bytes_with_a_raster_signature_are_images() {
        for (bytes, expected) in [
            (&b"\x89PNG\r\n\x1a\n"[..], Some("image/png")),
            (&[0xff, 0xd8, 0xff, 0xe0], Some("image/jpeg")),
            (b"GIF87a..", Some("image/gif")),
            (b"GIF89a..", Some("image/gif")),
            (b"RIFF\x10\0\0\0WEBPVP8 ", Some("image/webp")),
            (b"RIFF\x10\0\0\0WAVEfmt ", None),
            (b"RIFF", None),
            (b"<svg xmlns='http://www.w3.org/2000/svg'/>", None),
            (b"", None),
        ] {
            assert_eq!(raster_type(bytes), expected, "{bytes:?}");
        }
        // A declared image whose bytes are another format, or SVG behind a
        // raster type, is its type and size, never an image.
        for (declared, bytes) in [
            ("image/png", &b"<svg/>"[..]),
            ("image/webp", b"GIF89a.."),
            ("image/gif", b"RIFF"),
        ] {
            assert_eq!(
                decode(ResourceKind::Document, Some(declared), bytes),
                DocumentContent::Opaque {
                    content_type: declared.to_owned(),
                    len: bytes.len()
                },
                "{declared}"
            );
        }
        assert_eq!(
            decode(ResourceKind::PixelSurface, None, b"GIF89a.."),
            DocumentContent::Invalid("Invalid frame: not a PNG or JPEG".to_owned()),
            "a surface is PNG or JPEG only"
        );
    }

    #[test]
    fn long_text_is_cut_on_a_char_boundary() {
        let long = format!("a{}", "é".repeat(MAX_TEXT));
        let (shown, cut) = text(decode(
            ResourceKind::Document,
            Some("text/plain"),
            long.as_bytes(),
        ));
        assert_eq!(shown.len(), MAX_TEXT - 1);
        assert!(long.starts_with(&shown));
        assert_eq!(cut.as_deref(), Some("showing the first 256 KiB of 513 KiB"));
        let exact = decode(
            ResourceKind::Document,
            Some("text/plain"),
            &vec![b'a'; MAX_TEXT],
        );
        assert_eq!(text(exact).1, None, "exactly the cap is not cut");
    }
}
