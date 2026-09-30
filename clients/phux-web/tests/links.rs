//! Link detection and the scheme allowlist (`phux_web::links`), under node.

use phux_web::links::{allowed_link, url_at};
use wasm_bindgen_test::wasm_bindgen_test;

fn row(text: &str) -> Vec<char> {
    text.chars().collect()
}

#[wasm_bindgen_test]
fn only_http_https_and_mailto_links_open() {
    for good in [
        "https://example.com/a?b=c#d",
        "http://127.0.0.1:8080/",
        "HTTPS://EXAMPLE.COM",
        "mailto:someone@example.com",
    ] {
        assert_eq!(allowed_link(good), Some(good), "{good}");
    }
    assert_eq!(
        allowed_link("  https://example.com  "),
        Some("https://example.com")
    );
    for bad in [
        "javascript:alert(1)",
        "JavaScript:alert(1)",
        "data:text/html,hi",
        "file:///etc/passwd",
        "vbscript:x",
        "https://",
        "http:example.com",
        "mailto:",
        "https://exa mple.com",
        "https://example.com/\u{7}",
        "https://exa\nmple.com/",
        "",
    ] {
        assert_eq!(allowed_link(bad), None, "{bad:?}");
    }
}

#[wasm_bindgen_test]
fn a_plain_url_under_the_pointer_is_found_without_trailing_punctuation() {
    let text = row("see https://example.com/x_(y). and (http://a.test/b), mail mailto:me@a.test!");
    let start = 4;
    let end = start + "https://example.com/x_(y)".len();
    for col in [start, start + 8, end - 1] {
        assert_eq!(
            url_at(&text, col).as_deref(),
            Some("https://example.com/x_(y)"),
            "col {col}"
        );
    }
    assert_eq!(
        url_at(&text, end),
        None,
        "the trailing period is not part of it"
    );
    assert_eq!(url_at(&text, 0), None, "plain text");
    let second = text.iter().collect::<String>().find("http://a").unwrap();
    assert_eq!(url_at(&text, second - 1), None, "the opening parenthesis");
    assert_eq!(
        url_at(&text, second + 3).as_deref(),
        Some("http://a.test/b"),
        "an unbalanced closing parenthesis is trimmed"
    );
    let mail = text.iter().collect::<String>().find("mailto").unwrap();
    assert_eq!(url_at(&text, mail).as_deref(), Some("mailto:me@a.test"));
    assert_eq!(
        url_at(&row("xhttps://glued.test"), 3),
        None,
        "not at a word start"
    );
    assert_eq!(url_at(&row("javascript:alert(1)"), 2), None);
}
