//! Native read-only document presentation, never a browser or terminal grid.

use phux_protocol::ids::ResourceId;
use rune::{El, col, el, row, text};

use super::{CAPTION, Ink, LABEL, PAD};
use crate::desk::Desk;
use crate::desk::resources::Body;

pub fn view(desk: &Desk, id: &ResourceId, ink: &Ink) -> El {
    let focused = id.clone();
    let selected = id.clone();
    let measured = id.clone();
    let document = &desk.connection.resources.entries[id];
    let label = document.record.name.as_deref().unwrap_or("Document");
    let detail = document.detail.as_deref().map(|d| format!("{d} · "));
    let area = desk.pane_area(id).unwrap_or([0.0, 0.0]);
    let shown = match (&document.body, &document.viewer) {
        (Body::Tree(source), Some(viewer)) => Some(viewer.text(source)),
        _ => None,
    };
    let content = match shown {
        Some(words) => source_text(words, ink, id),
        None => body(&document.body, area, ink, id),
    };
    let interactive = desk.document_input_frame(id).is_some();
    let state = match desk.document_input_refused(id) {
        Some(reason) => format!("View only: {reason}"),
        None if interactive => "Live mirror · click, drag and type".to_owned(),
        None => "Read-only mirror".to_owned(),
    };
    let surface = el()
        .grow(1.0)
        .w_full()
        .min_h(0.0)
        .min_w(0.0)
        .on_layout(move |d: &mut Desk, rect| d.laid_out(measured.clone(), rect))
        .child(content);
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
                "{label} · {}{id} · {state}",
                detail.unwrap_or_default()
            ))
            .wrap()
            .font(CAPTION)
            .color(ink.dim),
        )
        .children(viewer_controls(desk, id, ink))
        .child(if interactive {
            takes_input(surface, id, area)
        } else {
            surface
        })
}

fn viewer_controls(desk: &Desk, id: &ResourceId, ink: &Ink) -> Option<El> {
    if !matches!(
        desk.connection.resources.entries.get(id)?.body,
        Body::Tree(_)
    ) {
        return None;
    }
    let viewer = desk.connection.resources.entries.get(id)?.viewer.as_ref()?;
    let target = id.clone();
    let enabled = !cfg!(target_os = "ios")
        && desk
            .file
            .markdown_viewer
            .as_ref()
            .is_none_or(|argv| !argv.is_empty());
    let button = enabled.then(|| {
        super::pressable(format!("document-source-{id}"), ink)
            .w(100.0)
            .h(26.0)
            .center()
            .label(if viewer.source {
                "Show rendered Markdown"
            } else {
                "Show Markdown source"
            })
            .on_press(move |d: &mut Desk| d.toggle_document_source(&target))
            .child(
                text(if viewer.source { "Rendered" } else { "Source" })
                    .font(CAPTION)
                    .color(ink.text),
            )
    });
    Some(
        row()
            .w_full()
            .h(28.0)
            .items_center()
            .gap(8.0)
            .child(
                text(viewer.status().to_owned())
                    .font(CAPTION)
                    .color(ink.dim)
                    .grow(1.0)
                    .min_w(0.0),
            )
            .children(button),
    )
}

/// A surface whose producer takes input: a press clicks the frame pixel
/// under it once released, a press that moves drags (selecting text), and
/// the wheel scrolls the page. Drag points and sizes are the element's own,
/// where the frame is drawn contain-fit.
fn takes_input(surface: El, id: &ResourceId, area: [f32; 2]) -> El {
    let (pressed, wheeled) = (id.clone(), id.clone());
    surface
        .cursor(rune::Cursor::Pointer)
        .touch_drags_at_once()
        .on_drag(move |d: &mut Desk, drag, _| match drag.phase {
            rune::Phase::Down => d.focus_pane(&pressed),
            rune::Phase::Move => {}
            rune::Phase::Up if !drag.cancelled() => d.document_release(
                &pressed,
                [drag.start_x, drag.start_y],
                [drag.x, drag.y],
                [drag.w, drag.h],
            ),
            rune::Phase::Up => {}
        })
        .on_wheel(move |d: &mut Desk, event, _| {
            d.document_wheel(&wheeled, area, [event.dx, event.dy])
        })
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
        Body::Tree(json) => source_text(json, ink, id),
        Body::Surface(image) => surface(image, area, ink),
    }
}

fn source_text(words: &str, ink: &Ink, id: &ResourceId) -> El {
    el().w_full()
        .h_full()
        .min_h(0.0)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .child(
            text(words.to_owned())
                .key(crate::desk::resources::text_key(id))
                .selectable(true)
                .wrap_anywhere()
                .font(rune::TextStyle::new(12.0, 400.0).mono())
                .color(ink.text),
        )
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
