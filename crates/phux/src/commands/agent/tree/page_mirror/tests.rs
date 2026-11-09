use super::*;
use phux_protocol::wire::frame::SpawnResource;

const LISTING: &str = r#"[
    {"type":"background_page","title":"ext","url":"chrome-extension://x","webSocketDebuggerUrl":"ws://127.0.0.1:9222/devtools/page/A"},
    {"type":"page","title":"phux - terminal multiplexer","url":"https://phux.sh/","webSocketDebuggerUrl":"ws://127.0.0.1:9222/devtools/page/B"},
    {"type":"page","title":"Docs","url":"https://phux.sh/docs","webSocketDebuggerUrl":"ws://127.0.0.1:9222/devtools/page/C"}
]"#;

#[test]
fn browser_labels_fit_spawn_bounds_without_changing_source_identity() {
    let mut page = Page {
        title: "雪".repeat(50),
        url: format!("https://example.test/{}", "a".repeat(250)),
        ws: "ws://localhost/devtools/page/1".into(),
    };
    let original = page.clone();
    assert_eq!(page_label(&page).len(), 120);
    assert_eq!(page, original);
    page.title.clear();
    assert_eq!(page_label(&page).len(), 120);
    page.url.clear();
    assert_eq!(page_label(&page), "Browser page");
    page.title = format!("{}雪", "a".repeat(119));
    assert_eq!(page_label(&page), "a".repeat(119));
    let label = page_label(&page);
    for resource in [
        SpawnResource::accessibility_tree(ResourceId::local(1), label),
        SpawnResource::pixel_surface(ResourceId::local(1), label),
    ] {
        assert!(
            resource.provider.unwrap().len()
                <= phux_protocol::wire::frame::MAX_RESOURCE_PROVIDER_BYTES
        );
    }
}

#[test]
fn a_page_is_picked_by_title_or_url_and_never_an_extension() {
    assert!(pick_page("first", LISTING).unwrap().ws.ends_with("/B"));
    assert_eq!(
        pick_page("Docs", LISTING).unwrap().url,
        "https://phux.sh/docs"
    );
    assert_eq!(pick_page("/docs", LISTING).unwrap().title, "Docs");
    assert!(pick_page("ext", LISTING).is_err());
}

#[test]
fn a_body_is_complete_once_its_content_length_has_arrived() {
    let raw = b"HTTP/1.1 200 OK\r\nContent-Length:5\r\n\r\n[1,2";
    assert_eq!(http_body(raw), None);
    assert_eq!(
        http_body(b"HTTP/1.1 200 OK\r\nContent-Length:5\r\n\r\n[1,2]"),
        Some("[1,2]")
    );
}

#[test]
fn the_screencast_asks_for_the_frames_the_mirror_was_given() {
    let png = Frames {
        jpeg: None,
        max_width: None,
    };
    assert_eq!(
        screencast(png),
        serde_json::json!({ "format": "png", "everyNthFrame": 1 })
    );
    let jpeg = Frames {
        jpeg: Some(70),
        max_width: Some(800),
    };
    assert_eq!(
        screencast(jpeg),
        serde_json::json!({ "format": "jpeg", "quality": 70, "maxWidth": 800, "everyNthFrame": 1 })
    );
}
