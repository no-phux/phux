//! Typed documents: dispatch by content type, and images on the real
//! decoder workers, GIF/WebP included.

use super::*;
use phux_client_core::document_view::MAX_TEXT;

mod lines;

/// A non-image publication's body and caption, through the one decode path.
fn typed(content_type: &str, bytes: &[u8]) -> (Body, String) {
    let mut d = Document::new(record(1, ResourceKind::Document));
    let published = publication(1, content_type, bytes.to_vec());
    assert!(d.publish(&published, None, &mut surface::Decodes::default()));
    (d.body, d.detail.expect("a typed document names its type"))
}

fn text(body: &Body) -> &str {
    let Body::Tree(text) = body else {
        panic!("text body")
    };
    text
}

#[test]
fn markdown_wire_publications_render_in_the_native_pane_and_copy_what_is_shown() {
    let (client, mut rt) = fixture(
        &[record(1, ResourceKind::Document)],
        &doc(LayoutNode::Leaf(id(1))),
    );
    rt.state.file.markdown_viewer = Some(vec!["/usr/bin/tr".into(), "a-z".into(), "A-Z".into()]);
    let payload =
        phux_protocol::wire::frame::typed_document::encode_bootstrap("text/markdown", b"# hello");
    for frame in bootstrap_frames(
        &id(1),
        [1, 1],
        BootstrapStreamProfile::DocumentTypedV1,
        [0, 0],
        &payload,
    ) {
        client.feed(frame).unwrap();
    }
    let started = Instant::now();
    loop {
        step(&mut rt);
        let viewer = rt.state.connection.resources.entries[&id(1)]
            .viewer
            .as_ref()
            .unwrap();
        if viewer.text("# hello") == "# HELLO" {
            break;
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let clipboard = rune::clipboard::memory();
    rt.use_clipboard(&clipboard);
    rt.update(|d, tx| d.shortcut(crate::keys::Shortcut::SelectAll, tx));
    rt.frame(1.0);
    rt.update(|d, tx| d.shortcut(crate::keys::Shortcut::Copy, tx));
    assert_eq!(
        clipboard.text().map(|s| s.to_string()),
        Some("# HELLO".into())
    );
    let pane_before = crate::ui::element(&rt, "document-@1").rect;
    let button = crate::ui::element(&rt, "document-source-@1").rect;
    rt.pointer_down(button[0] + 5.0, button[1] + 5.0);
    rt.pointer_up(button[0] + 5.0, button[1] + 5.0);
    step(&mut rt);
    let viewer = rt.state.connection.resources.entries[&id(1)]
        .viewer
        .as_ref()
        .unwrap();
    assert!(viewer.source);
    assert_eq!(viewer.text("# hello"), "# hello");
    assert_eq!(crate::ui::element(&rt, "document-@1").rect, pane_before);
    client
        .feed(FrameKind::ResourceOutput {
            terminal_id: id(1),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq: 1,
            bytes: b"# newer".to_vec().into(),
        })
        .unwrap();
    step(&mut rt);
    let document = &rt.state.connection.resources.entries[&id(1)];
    assert_eq!(text(&document.body), "# newer");
    assert!(document.viewer.as_ref().unwrap().source);
    assert!(
        !outbound(&client)
            .iter()
            .any(|frame| matches!(frame, FrameKind::ResizeTerminal { .. }))
    );
}

#[test]
fn only_markdown_gets_a_viewer_and_source_choice_survives_new_publications() {
    let mut d = Document::new(record(1, ResourceKind::Document));
    let mut decodes = surface::Decodes::default();
    assert!(d.publish(
        &publication(1, "Text/Markdown; charset=utf-8", b"# hi".to_vec()),
        None,
        &mut decodes
    ));
    d.viewer.as_mut().expect("Markdown viewer").toggle();
    assert!(d.publish(
        &publication(2, "text/markdown", b"# newer".to_vec()),
        None,
        &mut decodes
    ));
    assert!(d.viewer.as_ref().unwrap().source);
    assert!(!d.publish(
        &publication(1, "text/plain", b"stale".to_vec()),
        None,
        &mut decodes
    ));
    assert!(
        d.viewer.is_some(),
        "stale publications don't replace a viewer"
    );
    assert!(d.publish(
        &publication(3, "text/html", b"<b>source</b>".to_vec()),
        None,
        &mut decodes
    ));
    assert!(d.viewer.is_none(), "HTML never executes or renders");
    assert!(d.publish(
        &publication(4, "text/markdown", b"\xff".to_vec()),
        None,
        &mut decodes
    ));
    assert!(d.viewer.is_none(), "invalid UTF-8 stays an error");
}

#[test]
fn an_unknown_type_is_named_by_size() {
    let (body, _) = typed("application/pdf", b"%PDF-1.7");
    assert_eq!(text(&body), "8 bytes of application/pdf");
}

#[test]
fn a_cut_text_caption_names_its_type_and_the_cut() {
    let long = format!("a{}", "é".repeat(MAX_TEXT));
    let (_, detail) = typed("text/plain", long.as_bytes());
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
fn typed_png_gif_and_webp_decode_on_the_real_workers() {
    let cache = rune::ImageCache::new(Arc::new(|| {}));
    let mut decodes = surface::Decodes::default();
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

#[test]
fn a_tree_caption_carries_only_its_cut() {
    let mut decodes = surface::Decodes::default();
    let published = |kind, seq, bytes: &[u8]| DocumentPublication {
        resource_id: id(1),
        kind,
        seq,
        bytes: bytes.to_vec(),
        content_type: None,
    };
    let mut tree = Document::new(record(1, ResourceKind::AccessibilityTree));
    let json = br#"{"a":[1]}"#;
    assert!(tree.publish(
        &published(ResourceKind::AccessibilityTree, 0, json),
        None,
        &mut decodes
    ));
    assert_eq!(tree.detail, None, "a tree declares no type");
    let long = format!("[\"{}\"]", "a".repeat(MAX_TEXT));
    assert!(tree.publish(
        &published(ResourceKind::AccessibilityTree, 1, long.as_bytes()),
        None,
        &mut decodes
    ));
    assert_eq!(
        tree.detail.as_deref(),
        Some("showing the first 256 KiB of 257 KiB"),
        "a tree is cut like any text"
    );
}

#[test]
fn a_declared_image_without_its_signature_never_reaches_the_cache() {
    let cache = rune::ImageCache::new(Arc::new(|| {}));
    let mut decodes = surface::Decodes::default();
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec();
    for (seq, content_type) in ["image/png", "image/webp", "image/gif"]
        .into_iter()
        .enumerate()
    {
        let mut d = Document::new(record(1, ResourceKind::Document));
        let published = publication(seq as u64, content_type, svg.clone());
        assert!(d.publish(&published, Some(&cache), &mut decodes));
        assert_eq!(
            text(&d.body),
            format!("{} bytes of {content_type}", svg.len()),
            "{content_type}"
        );
    }
    let (body, _) = typed("image/svg+xml", &svg);
    assert!(text(&body).starts_with("<svg"), "SVG is source");
    assert_eq!(decodes.started, 0, "nothing was handed to a decoder");
}
