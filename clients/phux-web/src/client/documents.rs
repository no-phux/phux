//! Document tiles: a read-only `<div>` over each document's layout leaf,
//! drawing its newest publication as text in a `<pre>`, an image from a
//! blob URL, or one line. Text only ever goes in as text content, never
//! markup, so a document cannot inject HTML into the page.

use phux_client_core::document_view::DocumentContent;
use phux_protocol::ids::ResourceId;

use super::*;
use crate::session::WebDocument;

/// DOM id of the element that holds the document tiles, beside the canvas.
pub const DOCUMENTS_CONTAINER_ID: &str = "phux-documents";

const TILE_STYLE: &str = "position:fixed;box-sizing:border-box;overflow:auto;margin:0;\
    padding:6px;display:flex;flex-direction:column;gap:4px;background:#0b0b0d;\
    color:#d4d4d8;font:12px/1.4 ui-monospace,Menlo,monospace;";
const CAPTION_STYLE: &str = "opacity:.6;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;";
const TEXT_STYLE: &str = "margin:0;white-space:pre;font:inherit;";
const IMAGE_STYLE: &str = "flex:1;min-height:0;max-width:100%;object-fit:contain;";

impl App {
    /// Lay a tile over each document leaf and redraw those whose
    /// publication changed; drop tiles whose document left the layout.
    pub(super) fn paint_documents(&self) {
        let tiles = self.session.document_tiles();
        let Some(container) = self.documents_container(!tiles.is_empty()) else {
            return;
        };
        let Some(page) = container.owner_document() else {
            return;
        };
        for child in children(&container) {
            let id = child.get_attribute("data-resource-id").unwrap_or_default();
            if !tiles.iter().any(|(listed, _, _)| listed.to_string() == id) {
                revoke(&child);
                child.remove();
            }
        }
        let canvas = self.canvas.get_bounding_client_rect();
        let (css_w, css_h) = self.css_cell(&canvas);
        for (id, pane, document) in &tiles {
            let Some(tile) = tile(&container, &page, id) else {
                continue;
            };
            let _ = tile.set_attribute(
                "style",
                &format!(
                    "{TILE_STYLE}left:{}px;top:{}px;width:{}px;height:{}px;",
                    canvas.left() + f64::from(pane.x) * css_w,
                    canvas.top() + f64::from(pane.y) * css_h,
                    f64::from(pane.cols) * css_w,
                    f64::from(pane.rows) * css_h,
                ),
            );
            let drawn = format!("{:?}/{}", document.seq, document.lost);
            if tile.get_attribute("data-seq").as_deref() != Some(drawn.as_str()) {
                draw(&page, &tile, document);
                let _ = tile.set_attribute("data-seq", &drawn);
            }
        }
    }

    /// Remove every tile and release its image.
    pub(super) fn dispose_documents(&self) {
        let Some(container) = self.documents_container(false) else {
            return;
        };
        for child in children(&container) {
            revoke(&child);
        }
        container.remove();
    }

    /// The tile container, created next to the canvas when `create`.
    fn documents_container(&self, create: bool) -> Option<web_sys::Element> {
        let page = self.canvas.owner_document()?;
        if let Some(existing) = page.get_element_by_id(DOCUMENTS_CONTAINER_ID) {
            return Some(existing);
        }
        if !create {
            return None;
        }
        let container = page.create_element("div").ok()?;
        container.set_id(DOCUMENTS_CONTAINER_ID);
        let parent = self
            .canvas
            .parent_element()
            .or_else(|| page.body().map(Into::into))?;
        parent.append_child(&container).ok()?;
        Some(container)
    }
}

fn children(container: &web_sys::Element) -> Vec<web_sys::Element> {
    let mut out = Vec::new();
    let mut next = container.first_element_child();
    while let Some(child) = next {
        next = child.next_element_sibling();
        out.push(child);
    }
    out
}

/// The tile for `id`, created empty the first time.
fn tile(
    container: &web_sys::Element,
    page: &web_sys::Document,
    id: &ResourceId,
) -> Option<web_sys::Element> {
    let id = id.to_string();
    let existing = children(container)
        .into_iter()
        .find(|child| child.get_attribute("data-resource-id").as_deref() == Some(id.as_str()));
    if existing.is_some() {
        return existing;
    }
    let tile = page.create_element("div").ok()?;
    tile.set_class_name("phux-document");
    tile.set_attribute("data-resource-id", &id).ok()?;
    container.append_child(&tile).ok()?;
    Some(tile)
}

/// Replace a tile's contents with its document's newest publication.
fn draw(page: &web_sys::Document, tile: &web_sys::Element, document: &WebDocument) {
    revoke(tile);
    tile.set_text_content(None);
    let _ = tile.set_attribute("data-kind", document.kind.as_str());
    let caption = document.caption();
    let _ = append(page, tile, "div", CAPTION_STYLE, &caption);
    if let Some(line) = document.notice() {
        let _ = append(page, tile, "div", "", &line);
        return;
    }
    match &document.content {
        Some(DocumentContent::Text { text, .. }) => {
            let _ = append(page, tile, "pre", TEXT_STYLE, text);
        }
        Some(DocumentContent::Image(content_type)) => {
            let Some(url) = blob_url(&document.image, content_type) else {
                let _ = append(page, tile, "div", "", "Image unavailable in this browser");
                return;
            };
            let _ = tile.set_attribute("data-url", &url);
            if let Some(image) = append(page, tile, "img", IMAGE_STYLE, "") {
                let _ = image.set_attribute("alt", &caption);
                let _ = image.set_attribute("src", &url);
            }
        }
        _ => {}
    }
}

fn append(
    page: &web_sys::Document,
    parent: &web_sys::Element,
    tag: &str,
    style: &str,
    text: &str,
) -> Option<web_sys::Element> {
    let child = page.create_element(tag).ok()?;
    if !style.is_empty() {
        child.set_attribute("style", style).ok()?;
    }
    if !text.is_empty() {
        child.set_text_content(Some(text));
    }
    parent.append_child(&child).ok()?;
    Some(child)
}

fn blob_url(bytes: &[u8], content_type: &str) -> Option<String> {
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(content_type);
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).ok()?;
    web_sys::Url::create_object_url_with_blob(&blob).ok()
}

/// Release the blob URL a tile's image holds, if any.
fn revoke(tile: &web_sys::Element) {
    if let Some(url) = tile.get_attribute("data-url") {
        let _ = web_sys::Url::revoke_object_url(&url);
        let _ = tile.remove_attribute("data-url");
    }
}
