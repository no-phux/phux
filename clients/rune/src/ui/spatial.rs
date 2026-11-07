//! Phone-only spatial canvas and its dedicated navigation surfaces. No
//! transforms scale terminal glyphs; Rune springs layout boxes and offsets.

use phux_client_core::layout::Direction;
use phux_protocol::ids::ResourceId;
use rune::{Cursor, El, Key, col, el, row, text, tok};

use super::{CAPTION, Ink, panes};
use crate::desk::Desk;
use crate::desk::phone::{
    Phone,
    spatial::{self, Rect},
};

const MAP_W: f32 = 96.0;
const MAP_H: f32 = 64.0;
const TOUCH: f32 = 44.0;

pub fn view(desk: &Desk, phone: &Phone, ink: &Ink, area: El) -> El {
    let Some(shown) = desk.shown.as_ref() else {
        return area;
    };
    let fallback = phux_client_core::layout::LayoutNode::Leaf(shown.terminal.clone());
    let tree = phone.tree.as_ref().unwrap_or(&fallback);
    let all = spatial::rects(tree);
    let Some((_, focus)) = all.iter().find(|(id, _)| id == &shown.terminal) else {
        return area;
    };
    let content = canvas(desk, phone, &all, *focus, ink);
    let content = if desk.phone_content_blocked() {
        content.inert()
    } else {
        content
    };
    let mut stage = area
        .key("phone-spatial-stage")
        .clip_rounded(0.0)
        .on_layout(|d: &mut Desk, r| d.phone_stage_laid_out([r[2], r[3]]))
        .child(content);
    let mut controls = el()
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .preserve_focus_on_press(true);
    if !desk.is_document(&shown.terminal) {
        controls = controls.child(history(ink));
    }
    if all.len() > 1 {
        controls = controls
            .child(minimap(&all, &shown.terminal, ink))
            .child(handle(ink))
            .child(
                control(
                    "phone-pane-picker",
                    "Choose pane by name or direction",
                    "Panes  ▴",
                    ink,
                )
                .pin(Some(140.0), None, None, Some(8.0))
                .on_press(Desk::phone_picker),
            );
    }
    // Keep these hit surfaces over Find/confirmation: their handlers refuse
    // navigation, but an inert map would pass its press into a tracking
    // terminal underneath. A pointer-taking sheet (or our own picker) has
    // its own scrim, so the underlying surfaces can be inert there.
    if desk.phone_content_blocked() {
        controls = controls.inert();
    }
    stage = stage.child(controls);
    if desk.phone_picker_visible() {
        stage = stage.child(picker(desk, &all, &shown.terminal, ink));
    }
    stage
}

fn canvas(desk: &Desk, phone: &Phone, all: &[(ResourceId, Rect)], focus: Rect, ink: &Ink) -> El {
    let fallback = spatial::camera_fallback(focus, phone.stage);
    let [cw, ch, x, y] = spatial::camera(focus, phone.stage);
    let mut canvas = el()
        .key("phone-canvas")
        .absolute(x, y)
        .size(cw, ch)
        .offset(phone.travel[0], phone.travel[1])
        .offset_motion(if phone.navigating() {
            rune::Spring::NONE
        } else {
            tok::motion::SNAPPY
        })
        .motion(tok::motion::SNAPPY);
    for (id, rect) in all {
        let focused = desk.shown.as_ref().is_some_and(|s| &s.terminal == id);
        if fallback && !focused {
            continue;
        }
        // Collapsed or precision-unsafe focus fills the stage without
        // inventing an area in the literal minimap or losing sibling views.
        let r = if focused && fallback {
            Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            }
        } else {
            *rect
        };
        let pane = panes::pane(desk, id, false, ink);
        let pane = if focused { pane } else { pane.inert() };
        canvas = canvas.child(
            el().key(format!("phone-spatial-pane-{id}"))
                .absolute(r.x * cw, r.y * ch)
                .size(r.w * cw, r.h * ch)
                .motion(tok::motion::SNAPPY)
                .clip_rounded(0.0)
                .child(pane),
        );
    }
    canvas
}

fn control(key: &str, label: &str, words: &str, ink: &Ink) -> El {
    el().key(key.to_owned())
        .h(TOUCH)
        .min_w(TOUCH)
        .px(10.0)
        .radius(12.0)
        .center()
        .fill(ink.raised)
        .border(1.0, ink.edge)
        .press(ink.hover)
        .cursor(Cursor::Pointer)
        .label(label.to_owned())
        .child(text(words.to_owned()).font(CAPTION).color(ink.text))
}

fn handle(ink: &Ink) -> El {
    control(
        "phone-navigation",
        "Move between panes: swipe left, right, up or down",
        "↔  Move  ↕",
        ink,
    )
    .pin(Some(8.0), None, None, Some(8.0))
    .cursor(Cursor::Grab)
    .on_drag(|d: &mut Desk, drag, _| d.phone_navigate(drag, false))
}

fn history(ink: &Ink) -> El {
    control(
        "phone-history",
        "Scroll focused pane: drag up or down",
        "↕  Scroll",
        ink,
    )
    .pin(None, None, Some(8.0), Some(8.0))
    .cursor(Cursor::Grab)
    .on_drag(|d: &mut Desk, drag, _| d.phone_history(drag))
}

fn minimap(all: &[(ResourceId, Rect)], focus: &ResourceId, ink: &Ink) -> El {
    let mut map = el()
        .key("phone-minimap")
        .pin(None, Some(8.0), Some(8.0), None)
        .size(MAP_W, MAP_H)
        .fill(ink.bg)
        .radius(8.0)
        .clip_rounded(8.0)
        .border(1.0, ink.edge)
        .label("Pane map: tap a pane or swipe to its neighbor")
        .cursor(Cursor::Pointer)
        .on_drag(|d: &mut Desk, drag, _| d.phone_navigate(drag, true));
    for (id, r) in all {
        map = map.child(
            el().key(format!("phone-map-pane-{id}"))
            .absolute(r.x * MAP_W, r.y * MAP_H)
            .size(r.w * MAP_W, r.h * MAP_H)
            .fill(ink.current)
            .border(1.0, ink.bg)
            // One shared hit test, not expanded, overlapping tiny buttons.
            .inert(),
        );
    }
    if let Some((_, r)) = all.iter().find(|(id, _)| id == focus) {
        map = map.child(
            el().key("phone-map-focus")
                .absolute(r.x * MAP_W, r.y * MAP_H)
                .size(r.w * MAP_W, r.h * MAP_H)
                .fill(ink.accent)
                .border(1.0, ink.bg)
                .motion(tok::motion::SNAPPY)
                .inert(),
        );
    }
    map
}

fn picker(desk: &Desk, all: &[(ResourceId, Rect)], focus: &ResourceId, ink: &Ink) -> El {
    let mut list = col()
        .key("phone-pane-list")
        .pin(Some(12.0), Some(12.0), Some(12.0), Some(12.0))
        .padding(6.0)
        .gap(4.0)
        .radius(12.0)
        .fill(ink.raised)
        .border(1.0, ink.edge)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        });
    let mut directions = row().w_full();
    for (label, dir) in [
        ("←", Direction::Left),
        ("↑", Direction::Up),
        ("↓", Direction::Down),
        ("→", Direction::Right),
    ] {
        directions = directions.child(
            picker_button(
                &format!("phone-direction-{dir:?}"),
                &format!("Focus pane {dir:?}"),
                label,
                ink,
                move |d| d.phone_toward(dir),
            )
            .grow(1.0)
            .px(0.0),
        );
    }
    list = list.child(directions);
    for (id, _) in all {
        let target = id.clone();
        let name = pane_name(desk, id);
        let label = format!(
            "{name} · {id}{}",
            if id == focus { " · current" } else { "" }
        );
        list = list.child(
            picker_button(&format!("phone-pick-{id}"), &label, &label, ink, move |d| {
                d.phone_pick(&target)
            })
            .w_full(),
        );
    }
    list = list.child(
        picker_button(
            "phone-picker-close",
            "Close pane picker",
            "Done",
            ink,
            Desk::close_phone_picker,
        )
        .w_full(),
    );
    el().key("phone-picker-modal")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .fill(ink.shade)
        .modal(true, Desk::close_phone_picker)
        .on_key(|d: &mut Desk, event, _| d.phone_picker_key(event))
        .on_paste(|_: &mut Desk, _, _| {})
        .child(list)
}

fn pane_name(desk: &Desk, id: &ResourceId) -> String {
    desk.topology
        .as_ref()
        .and_then(|t| t.pane(id))
        .map(crate::palette::pane_label)
        .or_else(|| {
            desk.resources
                .entries
                .get(id)
                .and_then(|d| d.record.name.clone())
        })
        .unwrap_or_else(|| id.to_string())
}

/// Focusability belongs only inside this modal, never the terminal chrome:
/// outside it Tab must continue to mean the terminal's Tab key.
fn picker_button(
    key: &str,
    label: &str,
    words: &str,
    ink: &Ink,
    action: impl Fn(&mut Desk) + Clone + 'static,
) -> El {
    let pressed = action.clone();
    control(key, label, words, ink)
        .focusable(true)
        .on_press(move |d: &mut Desk| pressed(d))
        .on_key(move |d: &mut Desk, event, _| {
            if event.key == Key::Enter || event.key == Key::Text(" ".into()) {
                action(d);
                true
            } else {
                false
            }
        })
}
