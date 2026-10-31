//! The terminal area: the split tree, one terminal per leaf, with the
//! keyboard's pane marked and draggable dividers between them.

use phux_client_core::layout::{LayoutNode, SplitDir};
use phux_protocol::ids::ResourceId;
use rune::{Color, Cx, El, Phase, col, el, row};

use super::status::placeholder;
use super::{Ink, PAD, overlay};
use crate::desk::{Desk, SplitPath};
use crate::grid;

/// The hit area a divider gives the pointer, around its one-point line.
pub(crate) const DIVIDER: f32 = 7.0;

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
        return area.fill(ink.bg).center().child(placeholder(desk, ink));
    };
    let several = matches!(tree, LayoutNode::Split { .. });
    area.fill(ink.bg)
        .child(node(desk, &tree, &mut Vec::new(), several, ink))
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

/// A divider: drag to move it, live, and the new ratio is written on release.
fn divider(path: SplitPath, horizontal: bool, ratio: f32, ink: &Ink) -> El {
    let line = if horizontal {
        el().w(1.0).h_full()
    } else {
        el().w_full().h(1.0)
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
        .hover(ink.accent.alpha(0.12))
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
        .child(line.fill(ink.separator))
}

fn pane(desk: &Desk, id: &ResourceId, several: bool, ink: &Ink) -> El {
    let focused = desk.shown.as_ref().is_some_and(|s| &s.terminal == id);
    let view = if focused {
        desk.shown.as_ref().map(|s| s.view)
    } else {
        desk.others.get(id).copied()
    };
    let frame = desk
        .client
        .as_ref()
        .zip(view)
        .and_then(|(client, view)| client.acquire_view(view).map(|f| (f, view)));
    let (laid, wheel, press, moved, left, dropped) = (
        id.clone(),
        id.clone(),
        id.clone(),
        id.clone(),
        id.clone(),
        id.clone(),
    );
    let host = el()
        .key(format!("pane-host-{id}"))
        .w_full()
        .h_full()
        .padding(PAD)
        .on_layout(move |d: &mut Desk, rect| {
            d.laid_out(laid.clone(), [rect[2] - 2.0 * PAD, rect[3] - 2.0 * PAD])
        });
    let Some((frame, view)) = frame else {
        return host
            .fill(ink.bg)
            .center()
            .child(rune::text("Opening…").font(super::CAPTION).color(ink.dim));
    };
    if focused {
        // The exact frame this build paints, for the drawn receipt.
        desk.painting(view, &frame);
    }
    let metrics = desk.metrics();
    let keyboard = focused && desk.focused;
    let mut grid_layer = el()
        .child(grid::paint(
            &frame,
            keyboard,
            desk.blink_on,
            &metrics,
            ink.selection,
        ))
        .children(overlay::link_underline(desk, id, &metrics, ink));
    if keyboard
        && !desk.palette_open
        && !desk.inbox.open
        && !desk.settings_open
        && desk.pending.is_none()
    {
        grid_layer = grid_layer.child(grid::ime_layer(
            &frame,
            desk.preedit.as_deref(),
            &metrics,
            (ink.text, ink.raised),
        ));
    }
    let edge = if several && focused {
        ink.accent.alpha(0.55)
    } else {
        Color::TRANSPARENT
    };
    let linked = desk.pointer.hover.as_ref().is_some_and(|(t, _)| t == id);
    let area = desk.pane_area(id).unwrap_or([0.0, 0.0]);
    host.fill(grid::background(&frame))
        .border(1.0, edge)
        .cursor(if linked { rune::Cursor::Pointer } else { rune::Cursor::Text })
        .on_pointer_move_local(move |d: &mut Desk, event, tx| {
            d.hover(&moved, event.x - PAD, event.y - PAD, tx.mods().cmd);
        })
        .on_drop_files(move |d: &mut Desk, paths, _| d.drop_files(&dropped, paths))
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
            if !d.press_link(&press, drag.phase, x, y, mods.cmd, tx) {
                d.pointer(&press, drag.phase, x, y, drag.clicks, mods);
                if drag.phase == Phase::Up {
                    d.selection_done(tx);
                }
            }
        })
        .child(grid_layer)
        .children(overlay::history(desk, id, &frame, area, ink))
        .children(overlay::find_bar(desk, id, area, ink))
        .children(focused.then(|| super::inbox::ask_card(desk, id, area, ink)).flatten())
        // Panes without the keyboard sit back a little.
        .children((several && !focused).then(|| el().absolute(0.0, 0.0).w_full().h_full().fill(Color::BLACK.alpha(0.18)).inert()))
}
