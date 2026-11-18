//! Plain publications keep one selectable source across successive updates.
use super::*;

fn publish_source(client: &Client, source: &str) {
    let payload = phux_protocol::wire::frame::typed_document::encode_bootstrap(
        "text/plain",
        source.as_bytes(),
    );
    for frame in bootstrap_frames(
        &id(1),
        [1, 1],
        BootstrapStreamProfile::DocumentTypedV1,
        [0, 0],
        &payload,
    ) {
        client.feed(frame).unwrap();
    }
}

#[test]
fn document_paragraphs_preserve_wrapping_crlf_and_copy_bytes() {
    for scale in [1.0, 1.25, 1.875, 2.0] {
        let (client, mut rt) = fixture(
            &[record(1, ResourceKind::Document)],
            &doc(LayoutNode::Leaf(id(1))),
        );
        rt.set_viewport((320.0, 340.0), scale);
        let source = "SOURCE_READY\r\n\r\nA long source line that wraps across the small viewport without losing any bytes or selection boundaries.\n日本語 🦀\nשלום source\n\nTAIL_READY\n";
        publish_source(&client, source);
        step(&mut rt);
        let text = crate::ui::element(&rt, &text_key(&id(1)));
        assert_eq!(text.text.as_deref(), Some(source));
        let clipboard = rune::clipboard::memory();
        rt.use_clipboard(&clipboard);
        outbound(&client);
        rt.update(|d, tx| d.shortcut(crate::keys::Shortcut::SelectAll, tx));
        rt.frame(0.0);
        rt.update(|d, tx| d.shortcut(crate::keys::Shortcut::Copy, tx));
        assert_eq!(rt.selected_text().as_deref(), Some(source), "scale {scale}");
        assert_eq!(
            clipboard.text().as_deref().map(String::as_str),
            Some(source),
            "scale {scale}"
        );
        assert!(
            outbound(&client).is_empty(),
            "copy never addresses a terminal"
        );
    }
}

#[test]
fn large_source_publications_preserve_scroll_extent_and_latest_copy() {
    let (client, mut rt) = fixture(
        &[record(1, ResourceKind::Document)],
        &doc(LayoutNode::Leaf(id(1))),
    );
    rt.set_viewport((640.0, 360.0), 1.875);
    rt.set_srgb_blend(true);
    let mut camera = std::env::var_os("RUNE_DOCUMENT_SNAP").map(|directory| {
        let gpu = rune::render::Gpu::headless();
        let mut renderer = rune::render::Renderer::new(
            &gpu.device,
            rune::render::OFFSCREEN_FORMAT.remove_srgb_suffix(),
        );
        renderer.set_srgb_blend(true);
        (std::path::PathBuf::from(directory), gpu, renderer)
    });
    let mut snapshot = |rt: &mut Runtime<Desk>, state: &str| {
        if let Some((directory, gpu, renderer)) = &mut camera {
            std::fs::create_dir_all(&directory).unwrap();
            let pixels = crate::ui::offscreen_as_app_with_renderer(gpu, renderer, rt, (1200, 675));
            crate::test_support::visual::write(
                &directory.join(format!("source-{state}.png")),
                rt,
                (1200, 675),
                &pixels,
                state,
            );
        }
    };
    let prefix: String = (0..1600)
        .map(|line| {
            format!(
                "Record {line:04}: stable source text, read-only native document publication.\n"
            )
        })
        .collect();
    assert!(prefix.len() < phux_client_core::document_view::MAX_TEXT);
    publish_source(&client, &prefix);
    step(&mut rt);
    let mut timings = Vec::new();
    let mut latest = String::new();
    for seq in 1..=12 {
        latest = format!("{prefix}\nTAIL_PUBLICATION_{seq}\n");
        let start = Instant::now();
        client
            .feed(FrameKind::ResourceOutput {
                terminal_id: id(1),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq,
                bytes: latest.as_bytes().to_vec().into(),
            })
            .unwrap();
        step(&mut rt);
        timings.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    eprintln!("source_publication_ms={timings:?}");
    snapshot(&mut rt, "top");
    let elements = rt.elements();
    let text = elements
        .iter()
        .find(|element| element.key.as_deref() == Some(text_key(&id(1)).as_str()))
        .unwrap();
    let viewport = elements[text.parent.unwrap()].rect;
    let overflows = text.rect[3] > viewport[3];
    rt.wheel_xy(
        viewport[0] + viewport[2] / 2.0,
        viewport[1] + viewport[3] / 2.0,
        0.0,
        -1_000_000.0,
    );
    rt.frame(1.0);
    rt.frame(1.0);
    snapshot(&mut rt, "bottom");
    let elements = rt.elements();
    let text = elements
        .iter()
        .find(|element| element.key.as_deref() == Some(text_key(&id(1)).as_str()))
        .unwrap();
    let viewport = elements[text.parent.unwrap()].rect;
    let [_, y, _, h] = text.rect;
    assert!(
        overflows,
        "source must overflow: {:?}, viewport {viewport:?}",
        text.rect
    );
    assert!(
        y < viewport[1],
        "the long document actually scrolled: {text:?}, viewport {viewport:?}"
    );
    assert!((y + h - viewport[1] - viewport[3]).abs() <= 1.0);
    let clipboard = rune::clipboard::memory();
    rt.use_clipboard(&clipboard);
    rt.update(|d, tx| d.shortcut(crate::keys::Shortcut::SelectAll, tx));
    rt.frame(0.0);
    rt.update(|d, tx| d.shortcut(crate::keys::Shortcut::Copy, tx));
    assert_eq!(
        clipboard.text().as_deref().map(String::as_str),
        Some(latest.as_str())
    );
}
