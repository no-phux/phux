//! Native read-only document presentation, never a browser or terminal grid.

use phux_protocol::ids::ResourceId;
use rune::{El, col, el, text};

use super::{CAPTION, Ink, LABEL, PAD};
use crate::desk::Desk;
use crate::desk::resources::Body;

pub fn view(desk: &Desk, id: &ResourceId, ink: &Ink) -> El {
    let focused = id.clone();
    let selected = id.clone();
    let measured = id.clone();
    let document = &desk.resources.entries[id];
    let label = document.record.name.as_deref().unwrap_or("Document");
    let detail = document.detail.as_deref().map(|d| format!("{d} · "));
    let content = body(
        &document.body,
        desk.pane_area(id).unwrap_or([0.0, 0.0]),
        ink,
        id,
    );
    col()
        .key(format!("document-{id}"))
        .w_full()
        .h_full()
        .min_w(0.0)
        .min_h(0.0)
        .selection_group(true)
        .on_selection_change(move |d: &mut Desk, change, _| {
            if change.started() {
                d.focus_pane(&selected);
            }
        })
        .padding(PAD)
        .gap(8.0)
        .fill(ink.bg)
        .on_press(move |d: &mut Desk| d.focus_pane(&focused))
        .child(
            text(format!(
                "{label} · {}{id} · Read-only mirror",
                detail.unwrap_or_default()
            ))
            .wrap()
            .font(CAPTION)
            .color(ink.dim),
        )
        .child(
            el().grow(1.0)
                .w_full()
                .min_h(0.0)
                .min_w(0.0)
                .on_layout(move |d: &mut Desk, rect| d.laid_out(measured.clone(), rect))
                .child(content),
        )
}

fn message(words: impl Into<String>, ink: &Ink) -> El {
    el().w_full().h_full().center().child(
        text(words.into())
            .wrap()
            .w_full()
            .font(LABEL)
            .color(ink.dim),
    )
}

fn body(body: &Body, area: [f32; 2], ink: &Ink, id: &ResourceId) -> El {
    match body {
        Body::Pending => message("Waiting for the producer’s first document…", ink),
        Body::Decoding => message("Decoding image…", ink),
        Body::Empty => message("Empty document · waiting for the next publication", ink),
        Body::Invalid(error) => message(error.clone(), ink),
        Body::Tree(json) => el()
            .w_full()
            .h_full()
            .min_h(0.0)
            .scroll(rune::ScrollSpec {
                stick_bottom: false,
            })
            .child(
                text(json.clone())
                    .key(crate::desk::resources::text_key(id))
                    .selectable(true)
                    .wrap_anywhere()
                    .font(rune::TextStyle::new(12.0, 400.0).mono())
                    .color(ink.text),
            ),
        Body::Surface(image) => surface(image, area, ink),
    }
}

fn surface(image: &rune::Image, area: [f32; 2], ink: &Ink) -> El {
    if let Some(error) = image.error() {
        return message(format!("Invalid image: {error}"), ink);
    }
    let Some(size) = image.size() else {
        return message("Decoding image…", ink);
    };
    let [w, h] = letterbox(size, area);
    el().w_full()
        .h_full()
        .center()
        .child(el().size(w, h).pixels(image.pixels()))
}

pub(super) fn letterbox([w, h]: [u32; 2], [aw, ah]: [f32; 2]) -> [f32; 2] {
    let scale = (aw.max(0.0) / w.max(1) as f32).min(ah.max(0.0) / h.max(1) as f32);
    [w as f32 * scale, h as f32 * scale]
}

#[cfg(test)]
mod tests {
    #[test]
    fn image_is_contained_not_cropped_or_stretched() {
        assert_eq!(super::letterbox([200, 100], [100.0, 100.0]), [100.0, 50.0]);
        assert_eq!(super::letterbox([100, 200], [100.0, 100.0]), [50.0, 100.0]);
        assert_eq!(super::letterbox([200, 100], [0.0, 100.0]), [0.0, 0.0]);
    }
}
