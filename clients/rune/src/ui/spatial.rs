//! Phone-only spatial canvas and its dedicated navigation surfaces. No
//! transforms scale terminal glyphs; Rune springs layout boxes and offsets.
//!
//! The toolbar, minimap and picker are opaque (`layer.overlay`, the
//! floating tier, `radius.md`): the phone's one blur is the session dock.

use crate::ui::tokens::Voice;
use crate::ui::tokens::layout::PHONE;
use phux_client_core::layout::Direction;
use phux_protocol::ids::ResourceId;
use rune::{Cursor, El, Key, col, el, row, text};

use super::phone::{Look, ground_look, touchable};
use super::tokens::{Paint, Tier};
use super::{Tokens, panes};
use crate::desk::Desk;
use crate::desk::phone::{
    Phone,
    spatial::{self, Rect},
};
use crate::ui::Cover;

pub fn view(desk: &Desk, phone: &Phone, ink: &Tokens, area: El) -> El {
    let Some(shown) = desk.connection.shown.as_ref() else {
        return area;
    };
    let fallback = phux_client_core::layout::LayoutNode::Leaf(shown.resource.clone());
    let tree = phone.tree.as_ref().unwrap_or(&fallback);
    let all = spatial::rects(tree);
    let Some((_, focus)) = all.iter().find(|(id, _)| id == &shown.resource) else {
        return area;
    };
    let page = super::phone::page_look(desk);
    let content = canvas(desk, phone, &all, *focus, ink, &page);
    let content = if desk.phone_content_blocked() {
        content.inert()
    } else {
        content
    };
    let session = desk.selected_session_name().unwrap_or_default();
    // Only this viewport is measured and clipped. The toolbar is a sibling,
    // so neither terminal rows nor the camera can extend behind its controls.
    let viewport = el()
        .key("phone-spatial-stage")
        .grow(1.0)
        .w_full()
        .min_h(0.0)
        .min_w(0.0)
        .clip_rounded(0.0)
        .on_layout(|d: &mut Desk, r| d.phone_stage_laid_out([r[2], r[3]]))
        .children(peek(desk, phone, &page))
        .child(pager(phone, &session, content, &page));
    // The toolbar sits on the terminal's ground, so it takes that ground's
    // tokens, as the phone's other controls do.
    let mut stage = area.key("phone-spatial-shell").child(
        col()
            .w_full()
            .h_full()
            .min_h(0.0)
            .child(viewport)
            .child(toolbar(desk, all.len() > 1, &ground_look(desk))),
    );
    if desk.phone_picker_visible() {
        stage = stage.child(picker(desk, &all, &shown.resource, &page));
    }
    stage
}

/// Move and Panes, only when there is somewhere to go. A lone terminal
/// keeps the whole stage: its finger scrolls it directly.
fn toolbar(desk: &Desk, several: bool, p: &Look) -> El {
    if !several {
        return el().key("phone-navigation-toolbar").h(0.0);
    }
    let tools = row()
        .key("phone-navigation-toolbar")
        .w_full()
        .h(p.space.touch_min)
        .min_h(p.space.touch_min)
        .gap(p.space.s0_5)
        .radius(p.radius.md)
        .continuous_corners()
        .fill(p.roles.layer.overlay.paint())
        .preserve_focus_on_press(true)
        .child(handle(p))
        .child(
            quiet_control(
                "phone-pane-picker",
                "Choose pane by name or direction",
                "Panes  ▴",
                p,
            )
            .on_press(Desk::phone_picker),
        );
    let tools = p.elevate(tools, Tier::Floating);
    if desk.modal().is_some() {
        tools.inert()
    } else {
        tools
    }
}

/// A tool on the toolbar: no surface of its own, only the washes.
fn quiet_control(key: &str, label: &str, words: &str, p: &Look) -> El {
    control(key, label, words, p).grow(1.0).radius(p.radius.md)
}

/// The spring the camera and the pane boxes move on: a page move on the
/// phone.
fn page_spring(p: &Look) -> rune::Spring {
    p.motion.page.enter
}

fn canvas(
    desk: &Desk,
    phone: &Phone,
    all: &[(ResourceId, Rect)],
    focus: Rect,
    ink: &Tokens,
    p: &Look,
) -> El {
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
            page_spring(p)
        })
        .motion(page_spring(p));
    for (id, rect) in all {
        let focused = desk
            .connection
            .shown
            .as_ref()
            .is_some_and(|s| &s.resource == id);
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
                .motion(page_spring(p))
                .clip_rounded(0.0)
                .child(pane),
        );
    }
    canvas
}

/// One session's page of the stage. A sideways finger carries it one to one;
/// lifted, it springs home, or slides off as the next session slides in
/// from the other side, parked off the stage until that session shows.
fn pager(phone: &Phone, session: &str, content: El, p: &Look) -> El {
    let swipe = &phone.swipe;
    let leaving = (swipe.leaving.as_ref()).is_some_and(|(name, _)| name == session);
    let x = if leaving {
        swipe.arrival.unwrap_or(0.0) * phone.stage[0]
    } else if swipe.paging {
        swipe.page
    } else {
        0.0
    };
    let page = el()
        .key(format!("phone-session-page-{session}"))
        .cover()
        .offset(x, 0.0)
        .offset_motion(if swipe.paging {
            rune::Spring::NONE
        } else {
            page_spring(p)
        })
        .child(content);
    let way = if swipe.paging {
        (swipe.page != 0.0).then(|| swipe.page.signum())
    } else {
        swipe.arrival
    };
    let Some(way) = way else { return page };
    let toward = |x: f32| rune::Effect {
        offset_fraction: rune::Vec2::new(x, 0.0),
        ..rune::Effect::IDENTITY
    };
    page.transition(
        rune::Transition {
            insertion: toward(-way),
            removal: toward(way),
            ..rune::Transition::offset(0.0, 0.0)
        }
        .springs(p.motion.page),
    )
}

/// Under the sliding page, in the strip it uncovers: the session a lift
/// goes to, lit once lifting commits. Nothing at either end, where the
/// page only gives.
fn peek(desk: &Desk, phone: &Phone, p: &Look) -> Option<El> {
    let swipe = &phone.swipe;
    if !swipe.paging {
        return None;
    }
    let name = desk.page_target(swipe.page)?;
    let w = phone.stage[0];
    let progress =
        (swipe.page.abs() / (w * crate::desk::phone::swipe::PAGE_COMMIT).max(1.0)).min(1.0);
    let forward = swipe.page < 0.0;
    let (fill, ink) = if progress >= 1.0 {
        (
            p.roles.accent.accent.paint(),
            p.roles.accent.on_accent.paint(),
        )
    } else {
        (p.roles.layer.raised.paint(), p.roles.text.text.paint())
    };
    let words = if forward {
        format!("{name}  ›")
    } else {
        format!("‹  {name}")
    };
    let gap = swipe.page.abs().min(w);
    let pill = el()
        .key("phone-session-peek")
        .min_h(p.space.touch_min)
        .max_w((gap - 2.0 * p.space.s3).max(0.0))
        .px(p.space.s4)
        .radius(p.radius.full)
        .continuous_corners()
        .center()
        .fill(fill)
        .motion(p.motion.control.enter)
        .opacity(progress)
        .child(text(words).font(p.ty.label.medium()).color(ink).truncate());
    // The uncovered strip reads as the layer under the page.
    let lane = row()
        .absolute(if forward { w - gap } else { 0.0 }, 0.0)
        .w(gap)
        .h_full()
        .center()
        .fill(p.roles.layer.sidebar.paint())
        .inert()
        .a11y_hidden();
    Some(lane.child(p.elevate(pill, Tier::Floating)))
}

/// A touch control: the touch target's height, its words in the label
/// role, hover (an iPad pointer) and press washes.
fn control(key: &str, label: &str, words: &str, p: &Look) -> El {
    touchable(
        el().key(key.to_owned())
            .h(p.space.touch_min)
            .min_w(p.space.touch_min)
            .px(p.space.s3)
            .radius(p.radius.sm)
            .center(),
        p,
    )
    .label(label.to_owned())
    .child(
        text(words.to_owned())
            .font(p.ty.label.medium())
            .color(p.roles.text.text.paint()),
    )
}

fn handle(p: &Look) -> El {
    quiet_control(
        "phone-navigation",
        "Move between panes: swipe left, right, up or down",
        "↔  Move  ↕",
        p,
    )
    .cursor(Cursor::Grab)
    .touch_drags_at_once()
    .on_drag(|d: &mut Desk, drag, tx| d.phone_navigate(drag, false, tx))
}

/// Inline top-bar tile, not an overlay over terminal text. It replaces the
/// redundant More button only when this active group has several panes.
pub(super) fn minimap(desk: &Desk, p: &Look) -> Option<El> {
    let tree = desk.phone.as_ref()?.tree.as_ref()?;
    let all = spatial::rects(tree);
    if all.len() < 2 {
        return None;
    }
    let map = map(&all, &desk.connection.shown.as_ref()?.resource, p);
    Some(if desk.modal().is_some() {
        map.inert()
    } else {
        map
    })
}

fn map(all: &[(ResourceId, Rect)], focus: &ResourceId, p: &Look) -> El {
    // The minimap: a phone screen's 3:2 thumbnail on the 4-point grid.
    let (w, h) = PHONE.minimap;
    let hairline = p.space.hairline;
    let ground = p.roles.layer.ground.paint();
    let map = el()
        .key("phone-minimap")
        .size(w, h)
        .min_w(w)
        .min_h(h)
        .fill(ground)
        .radius(p.radius.md)
        .continuous_corners()
        .clip_rounded(p.radius.md)
        .label("Pane map: tap a pane or swipe to its neighbor")
        .cursor(Cursor::Pointer)
        .touch_drags_at_once()
        .on_drag(|d: &mut Desk, drag, tx| d.phone_navigate(drag, true, tx));
    let mut map = p.elevate(map, Tier::Raised);
    for (id, r) in all {
        map = map.child(
            el().key(format!("phone-map-pane-{id}"))
            .absolute(r.x * w, r.y * h)
            .size(r.w * w, r.h * h)
            .fill(p.roles.wash.current.paint())
            .border(hairline, ground)
            // One shared hit test, not expanded, overlapping tiny buttons.
            .inert(),
        );
    }
    if let Some((_, r)) = all.iter().find(|(id, _)| id == focus) {
        map = map.child(
            el().key("phone-map-focus")
                .absolute(r.x * w, r.y * h)
                .size(r.w * w, r.h * h)
                .fill(p.roles.accent.accent.paint())
                .border(hairline, ground)
                .motion(p.motion.select.enter)
                .inert(),
        );
    }
    map
}

fn picker(desk: &Desk, all: &[(ResourceId, Rect)], focus: &ResourceId, p: &Look) -> El {
    let s = &p.space;
    let list = col()
        .key("phone-pane-list")
        .pin(Some(s.s3), Some(s.s3), Some(s.s3), Some(s.s3))
        .padding(s.s2)
        .gap(s.s1)
        .radius(p.radius.md)
        .continuous_corners()
        .fill(p.roles.layer.overlay.paint())
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .scroll_margin(s.s2);
    let mut list = p.elevate(list, Tier::Floating);
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
                p,
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
        let current = id == focus;
        let status = if current { " · current" } else { "" };
        let label = format!("{name} · {id}{status}");
        // Keep identity visible even when a long name needs an ellipsis.
        let words = format!("{id} · {name}{status}");
        let pick = picker_button(&format!("phone-pick-{id}"), &label, &words, p, move |d| {
            d.phone_pick(&target)
        })
        .w_full();
        list = list.child(if current {
            super::lifted(pick, p)
        } else {
            pick
        });
    }
    list = list.child(
        picker_button(
            "phone-picker-actions",
            "Terminal actions",
            "Terminal actions…",
            p,
            |d| {
                d.close_phone_picker();
                d.open_phone_actions();
            },
        )
        .w_full(),
    );
    list = list.child(
        picker_button(
            "phone-picker-close",
            "Close pane picker",
            "Done",
            p,
            Desk::close_phone_picker,
        )
        .w_full(),
    );
    // A real scroll tail lets the last focus ring clear the viewport too.
    list = list.child(el().w_full().h(s.s2));
    el().key("phone-picker-modal")
        .cover()
        .fill(p.roles.layer.scrim.paint())
        .modal(true, Desk::close_phone_picker)
        .on_key(|d: &mut Desk, event, _| d.phone_picker_key(event))
        .on_paste(|_: &mut Desk, _, _| {})
        .child(list)
}

fn pane_name(desk: &Desk, id: &ResourceId) -> String {
    desk.connection
        .topology
        .as_ref()
        .and_then(|t| t.pane(id))
        .map(crate::palette::pane_label)
        .or_else(|| {
            desk.connection
                .resources
                .entries
                .get(id)
                .and_then(|d| d.record.name.clone())
        })
        .unwrap_or_else(|| id.to_string())
}

/// Picker rows grow with their text, unlike the fixed-height toolbar.
/// At accessibility sizes a single line keeps each whole target revealable
/// even in the keyboard-reduced small phone. The full accessible name remains.
/// Focusability belongs only inside this modal: outside it Tab is terminal input.
fn picker_button(
    key: &str,
    label: &str,
    words: &str,
    p: &Look,
    action: impl Fn(&mut Desk) + Clone + 'static,
) -> El {
    let pressed = action.clone();
    let lines = if p.type_scale >= PHONE.stacked_title_scale {
        1
    } else {
        2
    };
    touchable(
        el().key(key.to_owned())
            .min_h(p.space.touch_min)
            .min_w(p.space.touch_min)
            .px(p.space.s3)
            .py(p.space.s2)
            .radius(p.radius.sm)
            .center(),
        p,
    )
    .label(label.to_owned())
    .child(
        text(words.to_owned())
            .key(format!("{key}-text"))
            .w_full()
            .min_w(0.0)
            .font(p.ty.label.medium())
            .color(p.roles.text.text.paint())
            .align(rune::Align::Center)
            .wrap_anywhere()
            .max_lines(lines)
            .a11y_hidden(),
    )
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
