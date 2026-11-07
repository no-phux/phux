//! Typed documents drawn by content type, as the desktop does (#2023):
//! images through the surface decoder, JSON pretty-printed, `text/*` and
//! `+xml`/`+json` as source (Markdown and HTML stay source), anything
//! else a line naming its size and type.

use phux_protocol::wire::frame::typed_document::essence_of;

use super::Body;

/// Rune lays a whole string out on the UI thread; a 4 MiB document
/// would stall it, so at most this much text is shown.
pub(super) const MAX_TEXT: usize = 256 * 1024;

fn essence(content_type: &str) -> String {
    essence_of(content_type).to_ascii_lowercase()
}

pub(super) fn image(content_type: &str) -> bool {
    matches!(
        essence(content_type).as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

/// A non-image, non-empty document's body and its caption detail: the
/// content type, plus how much is shown when the text was cut.
pub(super) fn decode(content_type: &str, bytes: &[u8]) -> (Body, String) {
    let essence = essence(content_type);
    let mut detail = content_type.to_owned();
    let mut text = if essence == "application/json" {
        match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(value) => serde_json::to_string_pretty(&value).expect("JSON value"),
            Err(error) => return (Body::Invalid(format!("Invalid JSON: {error}")), detail),
        }
    } else if essence.starts_with("text/")
        || essence.ends_with("+xml")
        || essence.ends_with("+json")
    {
        match String::from_utf8(bytes.to_vec()) {
            Ok(text) => text,
            Err(error) => {
                return (
                    Body::Invalid(format!("Invalid UTF-8 text: {error}")),
                    detail,
                );
            }
        }
    } else {
        let line = format!("{} bytes of {content_type}", bytes.len());
        return (Body::Tree(line), detail);
    };
    if text.len() > MAX_TEXT {
        let total = text.len().div_ceil(1024);
        text.truncate(text.floor_char_boundary(MAX_TEXT));
        let shown = MAX_TEXT / 1024;
        detail = format!("{detail} · showing the first {shown} KiB of {total} KiB");
    }
    (Body::Tree(text), detail)
}
