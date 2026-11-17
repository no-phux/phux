//! Native read-only document presentation, never a browser or terminal grid.

use phux_protocol::ids::ResourceId;
use rune::{El, col, el, text};

use super::PAD;
use super::tokens::{Paint, Tokens};
use crate::desk::Desk;
use crate::desk::resources::Body;

pub fn view(desk: &Desk, id: &ResourceId, p: &Tokens) -> El {
    let ink = p;
    let focused = id.clone();
    let selected = id.clone();
    let measured = id.clone();
    let document = &desk.connection.resources.entries[id];
    let label = document.record.name.as_deref().unwrap_or("Document");
    let detail = document.detail.as_deref().map(|d| format!("{d} · "));
    let area = desk.pane_area(id).unwrap_or([0.0, 0.0]);
    let content = body(&document.body, area, ink, id);
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
        .gap(ink.space.s2)
        .fill(ink.roles.layer.ground.paint())
        .on_press(move |d: &mut Desk| d.focus_pane(&focused))
        .child(
            text(format!(
                "{label} · {}{id} · {state}",
                detail.unwrap_or_default()
            ))
            .wrap()
            .font(ink.ty.caption)
            .color(ink.roles.text.dim.paint()),
        )
        .child(if interactive {
            takes_input(surface, id, area)
        } else {
            surface
        })
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

fn message(words: impl Into<String>, ink: &Tokens) -> El {
    el().w_full().h_full().center().child(
        text(words.into())
            .wrap()
            .w_full()
            .font(ink.ty.label)
            .color(ink.roles.text.dim.paint()),
    )
}

fn body(body: &Body, area: [f32; 2], ink: &Tokens, id: &ResourceId) -> El {
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
                    .font(ink.ty.mono)
                    .color(ink.roles.text.text.paint()),
            ),
        Body::Surface(image) => surface(image, area, ink),
    }
}

fn surface(image: &rune::Image, area: [f32; 2], ink: &Tokens) -> El {
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
        .child(el().w(w).h(h).pixels(image.pixels()))
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
