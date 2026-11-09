//! Typed documents: dispatch by content type, and images on the real
//! decoder workers, GIF/WebP included.

use super::*;
use crate::desk::resources::typed::{self, MAX_TEXT};

mod animation;

fn typed(content_type: &str, bytes: &[u8]) -> (Body, String) {
    typed::decode(content_type, bytes)
}

fn text(body: &Body) -> &str {
    let Body::Tree(text) = body else {
        panic!("text body")
    };
    text
}

#[test]
fn json_is_pretty_printed_and_invalid_json_says_so() {
    let (body, detail) = typed("application/json; charset=utf-8", br#"{"a":[1]}"#);
    assert_eq!(text(&body), "{\n  \"a\": [\n    1\n  ]\n}");
    assert_eq!(detail, "application/json; charset=utf-8");
    let (body, _) = typed("Application/JSON", b"{nope");
    assert!(matches!(body, Body::Invalid(error) if error.starts_with("Invalid JSON")));
}

#[test]
fn text_and_structured_suffixes_are_source_never_rendered() {
    for content_type in [
        "text/markdown",
        "text/html",
        "image/svg+xml",
        "application/ld+json",
    ] {
        let (body, detail) = typed(content_type, b"<b>hi</b>");
        assert_eq!(text(&body), "<b>hi</b>", "{content_type}");
        assert_eq!(detail, content_type);
    }
    let (body, _) = typed("text/plain", b"\xff\xfe");
    assert!(matches!(body, Body::Invalid(error) if error.starts_with("Invalid UTF-8")));
}

#[test]
fn an_unknown_type_is_named_by_size() {
    let (body, _) = typed("application/pdf", b"%PDF-1.7");
    assert_eq!(text(&body), "8 bytes of application/pdf");
    assert!(!typed::image("image/svg+xml") && !typed::image("image/tiff"));
    assert!(typed::image("IMAGE/WEBP; q=1") && typed::image("image/gif"));
}

#[test]
fn long_text_is_cut_on_a_char_boundary_and_the_caption_says_so() {
    // Two-byte chars put MAX_TEXT mid-character.
    let long = format!("a{}", "é".repeat(MAX_TEXT));
    let (body, detail) = typed("text/plain", long.as_bytes());
    let shown = text(&body);
    assert_eq!(shown.len(), MAX_TEXT - 1);
    assert!(long.starts_with(shown));
    assert_eq!(detail, "text/plain · showing the first 256 KiB of 513 KiB");
    let (_, detail) = typed("text/plain", &[b'a'; MAX_TEXT]);
    assert_eq!(detail, "text/plain", "exactly the cap is not cut");
}

fn publication(seq: u64, content_type: &str, bytes: Vec<u8>) -> DocumentPublication {
    DocumentPublication {
        resource_id: id(1),
        kind: ResourceKind::Document,
        seq,
        bytes,
        content_type: Some(content_type.to_owned()),
    }
}

fn encoded(format: image::ImageFormat) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::RgbaImage::new(3, 2)
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}

#[test]
fn a_document_is_a_mirror_with_pending_empty_text_and_image_states() {
    assert!(document_kind(ResourceKind::Document));
    let cache = rune::ImageCache::new(Arc::new(|| {}));
    let mut decodes = surface::Decodes::default();
    let mut d = Document::new(record(1, ResourceKind::Document));
    assert!(matches!(d.body, Body::Pending));
    assert!(d.publish(
        &publication(0, "text/plain", Vec::new()),
        Some(&cache),
        &mut decodes
    ));
    assert!(matches!(d.body, Body::Empty));
    assert!(d.publish(
        &publication(1, "text/plain", b"hi".to_vec()),
        Some(&cache),
        &mut decodes
    ));
    assert_eq!(
        (text(&d.body), d.detail.as_deref()),
        ("hi", Some("text/plain"))
    );
    for (seq, (content_type, format)) in [
        ("image/png", image::ImageFormat::Png),
        ("image/gif", image::ImageFormat::Gif),
        ("image/webp", image::ImageFormat::WebP),
    ]
    .into_iter()
    .enumerate()
    {
        let mut d = Document::new(record(1, ResourceKind::Document));
        let bytes = encoded(format);
        assert!(d.publish(
            &publication(seq as u64, content_type, bytes),
            Some(&cache),
            &mut decodes
        ));
        let Body::Surface(image) = &d.body else {
            panic!("{content_type} decodes off-thread")
        };
        assert_eq!(image.wait(), Ok([3, 2]), "{content_type}");
    }
}

#[test]
fn a_broken_typed_image_is_an_invalid_image() {
    let cache = rune::ImageCache::new(Arc::new(|| {}));
    let mut decodes = surface::Decodes::default();
    let mut d = Document::new(record(1, ResourceKind::Document));
    let bytes = b"GIF89a broken".to_vec();
    assert!(d.publish(
        &publication(0, "image/gif", bytes),
        Some(&cache),
        &mut decodes
    ));
    let Body::Surface(image) = &d.body else {
        panic!("decoding")
    };
    assert!(image.wait().is_err());
    while !d.finish_surface(Some(&cache), &mut decodes) {
        std::thread::yield_now();
    }
    assert!(matches!(&d.body, Body::Invalid(error) if error.starts_with("Invalid image")));
}
