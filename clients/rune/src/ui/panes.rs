//! The terminal area: the split tree, one floating glass card per leaf, the
//! keyboard's card lit, and draggable gutters between them.

use std::time::Instant;

use phux_client_core::layout::{LayoutNode, SplitDir};
use phux_protocol::ids::ResourceId;
use rune::{Color, Cx, El, Phase, PointerButton, Transition, col, el, row};

use super::status::placeholder;
use super::{CARD_GAP, CARD_RADIUS, Ink, PAD, overlay};
use crate::desk::mouse::Press;
use crate::desk::{Desk, SplitPath};
use crate::{grid, perf};

/// The gutter between two cards, all of it the divider's hit area.
pub(crate) const DIVIDER: f32 = CARD_GAP;

pub fn view(desk: &Desk, ink: &Ink, cx: &mut Cx<Desk>) -> El {
    desk.scale.set(cx.env().scale);
    desk.painted.replace(None);
    let area = el()
        .key("terminal")
        .grow(1.0)
        .w_full()
        .min_h(0.0)
        .min_w(0.0);
    let Some(tree) = desk.layout.clone() else {
        let waiting = el()
            .w_full()
            .h_full()
            .center()
            .child(placeholder(desk, ink));
        if desk.phone.is_some() {
            return area.child(waiting);
        }
        return area.child(card(waiting, ink.bg, Lit::Resting, ink));
    };
    if let Some(phone) = &desk.phone {
        return super::spatial::view(desk, phone, ink, area);
    }
    let several = matches!(tree, LayoutNode::Split { .. });
    area.child(node(desk, &tree, &mut Vec::new(), several, ink))
        .children(perf::hud(ink.dim))
}

fn node(desk: &Desk, tree: &LayoutNode, path: &mut SplitPath, several: bool, ink: &Ink) -> El {
    let (dir, ratio, left, right) = match tree {
        LayoutNode::Leaf(id) => return pane(desk, id, several, ink),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => (*dir, *ratio, left, right),
    };
    let horizontal = dir == SplitDir::Horizontal;
    let here = path.clone();
    path.push(false);
    let first = node(desk, left, path, several, ink);
    path.pop();
    path.push(true);
    let second = node(desk, right, path, several, ink);
    path.pop();
    let share = |el: El, grow: f32| el.grow(grow.max(0.01)).min_w(0.0).min_h(0.0);
    let container = if horizontal { row() } else { col() };
    let measured = here.clone();
    container
        .key(format!("split-{here:?}"))
        .on_layout(move |d: &mut Desk, rect| d.split_laid_out(measured.clone(), [rect[2], rect[3]]))
        .grow(1.0)
        .w_full()
        .h_full()
        .min_w(0.0)
        .min_h(0.0)
        .child(share(el().w_full().h_full().child(first), ratio))
        .child(divider(here, horizontal, ratio, ink))
        .child(share(el().w_full().h_full().child(second), 1.0 - ratio))
}

/// A divider: the clear gutter between two cards, with a grip that shows
/// under the pointer. Drag to move it, live, and the new ratio is written on
/// release.
fn divider(path: SplitPath, horizontal: bool, ratio: f32, ink: &Ink) -> El {
    const GRIP: (f32, f32) = (3.0, 36.0);
    let grip = if horizontal {
        el().size(GRIP.0, GRIP.1)
    } else {
        el().size(GRIP.1, GRIP.0)
    };
    let host = if horizontal {
        el().w(DIVIDER).h_full()
    } else {
        el().w_full().h(DIVIDER)
    };
    host.key(format!("divider-{path:?}"))
        .center()
        .cursor(if horizontal {
            rune::Cursor::ResizeColumn
        } else {
            rune::Cursor::ResizeRow
        })
        .hover_group()
        .on_drag(move |d: &mut Desk, drag, _| {
            if drag.phase == Phase::Down {
                // The ratio at the press, kept for the whole gesture.
                drag.data = ratio;
            }
            let span = d.split_span(&path, horizontal).max(1.0);
            let travel = if horizontal { drag.dx } else { drag.dy };
            d.drag_divider(
                path.clone(),
                drag.data + travel / span,
                drag.phase == Phase::Up,
            );
        })
        .child(
            grip.radius(GRIP.0 / 2.0)
                .fill(ink.accent)
                .group_opacity(0.0, 0.85)
                .inert(),
        )
}

/// How a card is lit: the keyboard's, among several, glows in the accent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lit {
    Resting,
    Focused,
}

/// A floating glass card around `content`: rounded, lifted off the window by
/// its shadow, and edged with a top-lit hairline rim. `content` is clipped to
/// the corners; the rim draws over it and never takes the pointer.
fn card(content: El, ground: Color, lit: Lit, ink: &Ink) -> El {
    let (top, bottom) = match lit {
        Lit::Resting => (ink.text.alpha(0.16), ink.text.alpha(0.05)),
        Lit::Focused => (ink.accent.alpha(0.8), ink.accent.alpha(0.35)),
    };
    let lifted = el()
        .w_full()
        .h_full()
        .radius(CARD_RADIUS)
        .continuous_corners()
        .fill(ground)
        .shadow(1.0, 3.0, ink.shade)
        .shadow(10.0, 30.0, ink.shade);
    let lifted = match lit {
        Lit::Resting => lifted,
        Lit::Focused => lifted.shadow(0.0, 22.0, ink.accent.alpha(0.22)),
    };
    lifted
        .child(
            content
                .radius(CARD_RADIUS)
                .continuous_corners()
                .clip_rounded(CARD_RADIUS),
        )
        .child(
            el().absolute(0.0, 0.0)
                .w_full()
                .h_full()
                .radius(CARD_RADIUS)
                .continuous_corners()
                // A faint sheen from the top, as light catches glass.
                .gradient(ink.text.alpha(0.035), Color::TRANSPARENT)
                .border_gradient(1.0, top, bottom)
                .inert(),
        )
}

/// A pane's card, keyed by its terminal so it keeps its place as the tree
/// changes, and eased in when it opens.
fn float(id: &ResourceId, card: El) -> El {
    card.key(format!("pane-{id}"))
        .transition(Transition::scale(0.98).combined(Transition::opacity()))
}

pub(super) fn pane(desk: &Desk, id: &ResourceId, several: bool, ink: &Ink) -> El {
    let focused = desk.shown.as_ref().is_some_and(|s| &s.terminal == id);
    if desk.is_document(id) {
        let lit = if focused && several {
            Lit::Focused
        } else {
            Lit::Resting
        };
        return float(
            id,
            card(super::documents::view(desk, id, ink), ink.bg, lit, ink),
        );
    }
    terminal_pane(desk, id, several, ink)
}

fn terminal_pane(desk: &Desk, id: &ResourceId, several: bool, ink: &Ink) -> El {
    let focused = desk.shown.as_ref().is_some_and(|s| &s.terminal == id);
    let view = if focused {
        desk.shown.as_ref().and_then(|s| s.view)
    } else {
        desk.others.get(id).copied().flatten()
    };
    perf::RENDERS.incr();
    let acquiring = Instant::now();
    let frame = desk
        .client
        .as_ref()
        .zip(view)
        .and_then(|(client, view)| client.acquire_view(view).map(|f| (f, view)));
    perf::ACQUIRE.record_elapsed(acquiring);
    let laid = id.clone();
    let host = el()
        .key(format!("pane-host-{id}"))
        .w_full()
        .h_full()
        .padding(PAD)
        .on_layout(move |d: &mut Desk, rect| {
            let [x, y, w, h] = rect;
            d.laid_out(
                laid.clone(),
                [x + PAD, y + PAD, w - 2.0 * PAD, h - 2.0 * PAD],
            )
        });
    let Some((frame, view)) = frame else {
        perf::ACQUIRE_REJECTED.incr();
        let opening = host
            .fill(ink.bg)
            .center()
            .child(rune::text("Opening…").font(super::CAPTION).color(ink.dim));
        return float(id, framed(desk, opening, ink.bg, Lit::Resting, ink));
    };
    if focused {
        // The exact frame this build paints, for the drawn receipt.
        desk.painting(view, &frame);
    }
    let grid_layer = terminal_layer(desk, id, &frame, focused, ink);
    let area = desk.pane_area(id).unwrap_or([0.0, 0.0]);
    let ground = grid::background(&frame);
    let lit = if several && focused {
        Lit::Focused
    } else {
        Lit::Resting
    };
    let host = terminal_input(host.fill(ground), desk, id)
        .child(grid_layer)
        .children(overlay::history(desk, id, &frame, area, ink))
        .children(overlay::find_bar(desk, id, area, ink))
        .children(focused.then(|| super::inbox::ask_card(desk, id, area, ink)).flatten())
        // Panes without the keyboard sit back a little.
        .children((several && !focused).then(|| el().absolute(0.0, 0.0).w_full().h_full().fill(ground.alpha(0.42)).inert()));
    float(id, framed(desk, host, ground, lit, ink))
}

/// A pane as a card, or bare on a phone, where the terminal runs edge to edge
/// under the controls.
fn framed(desk: &Desk, content: El, ground: Color, lit: Lit, ink: &Ink) -> El {
    if desk.phone.is_some() {
        return content.fill(ground);
    }
    card(content, ground, lit, ink)
}

fn terminal_layer(
    desk: &Desk,
    id: &ResourceId,
    frame: &phux_client_runtime::publication::GridFrame,
    focused: bool,
    ink: &Ink,
) -> El {
    let metrics = desk.metrics();
    let keyboard = focused && desk.focused;
    let mut layer = el()
        .child(grid::paint(
            frame,
            keyboard,
            desk.blink_on,
            &metrics,
            ink.selection,
        ))
        .children(overlay::link_underline(desk, id, &metrics, ink));
    if keyboard && desk.modal().is_none() {
        layer = layer.child(grid::ime_layer(
            frame,
            desk.preedit.as_deref(),
            &metrics,
            (ink.text, ink.raised),
        ));
    }
    layer
}

/// Only terminal cards install terminal mouse, drop, and history handlers.
fn terminal_input(host: El, desk: &Desk, id: &ResourceId) -> El {
    if desk.phone.is_some() && !desk.shown.as_ref().is_some_and(|s| &s.terminal == id) {
        return host.inert();
    }
    let (wheel, press, moved, left, dropped, menu) = (
        id.clone(),
        id.clone(),
        id.clone(),
        id.clone(),
        id.clone(),
        id.clone(),
    );
    let linked = desk.pointer.hover.as_ref().is_some_and(|(t, _)| t == id);
    host.cursor(if linked {
        rune::Cursor::Pointer
    } else {
        rune::Cursor::Text
    })
    .on_pointer_move_local(move |d: &mut Desk, event, tx| {
        d.hover(&moved, event.x - PAD, event.y - PAD, tx.mods());
    })
    .on_drop_files(move |d: &mut Desk, paths, _| d.drop_files(&dropped, paths))
    .pointer_buttons([
        PointerButton::Middle,
        PointerButton::Back,
        PointerButton::Forward,
    ])
    .on_secondary_press_local(move |d: &mut Desk, event, tx| {
        let at = Press {
            phase: Phase::Down,
            x: event.x - PAD,
            y: event.y - PAD,
            clicks: 1,
            mods: tx.mods(),
            button: PointerButton::Secondary,
        };
        d.secondary_press(&menu, at, event.window());
    })
    .on_hover_change(move |d: &mut Desk, inside, _| {
        if !inside {
            d.pointer_left(&left);
        }
    })
    .on_wheel(move |d: &mut Desk, event, _| {
        d.wheel(&wheel, event.x - PAD, event.y - PAD, event.dy, event.mods);
        true
    })
    .on_drag(move |d: &mut Desk, drag, tx| {
        // Drag points include the padding; the grid starts inside it.
        let (x, y, mods) = (drag.x - PAD, drag.y - PAD, tx.mods());
        // Only the primary button opens links; the rest are the program's.
        let cmd = mods.cmd && tx.button() == PointerButton::Primary;
        if !d.press_link(&press, drag.phase, x, y, cmd, tx) {
            let step = Press {
                phase: drag.phase,
                x,
                y,
                clicks: drag.clicks,
                mods,
                button: tx.button(),
            };
            d.pointer(&press, &step, tx);
            if drag.phase == Phase::Up {
                d.selection_done(tx);
            }
        }
    })
}
