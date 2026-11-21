//! The terminal area: the split tree, one floating glass card per leaf, the
//! keyboard's card raised a tier with no colour of its own, and draggable
//! gutters between them. A lone pane with the sidebar folded goes flush.

use std::time::Instant;

use phux_client_core::layout::{LayoutNode, SplitDir};
use phux_protocol::ids::ResourceId;
use rune::{Color, Cx, El, Key, Phase, PointerButton, Transition, col, el, row};

use super::status::placeholder;
use super::tokens::Tier;
use super::{PAD, Paint, Tokens, overlay};
use crate::desk::mouse::Press;
use crate::desk::{Desk, Grab, SplitPath};
use crate::ui::Cover;
use crate::{grid, perf};
use phux_design::tokens::SPACE;

pub fn view(desk: &Desk, ink: &Tokens, cx: &mut Cx<Desk>) -> El {
    let t = ink;
    desk.scale.set(cx.env().scale);
    desk.connection.painted.replace(None);
    let area = el()
        .key("terminal")
        .grow(1.0)
        .w_full()
        .min_h(0.0)
        .min_w(0.0);
    let Some(tree) = desk.connection.layout.clone() else {
        let waiting = el()
            .w_full()
            .h_full()
            .center()
            .child(placeholder(desk, ink));
        if desk.phone.is_some() {
            return area.child(waiting);
        }
        return area.child(card(waiting, t.roles.layer.ground.paint(), Lit::Resting, t));
    };
    if let Some(phone) = &desk.phone {
        return super::spatial::view(desk, phone, ink, area);
    }
    let several = matches!(tree, LayoutNode::Split { .. });
    let floor = pane_floor(desk, &tree);
    area.child(node(desk, &tree, &mut Vec::new(), several, floor, ink))
        .children(perf::hud(t))
}

/// The fewest cells a pane is squeezed to while its window has room: a
/// prompt and a short command. Deep splits halve a pane at every level, so
/// without a floor the eighth split of a spiral draws one or two columns.
const MIN_CELLS: [f32; 2] = [20.0, 4.0];

/// The least room `tree` takes along each axis when every pane has `leaf`.
fn least(tree: &LayoutNode, leaf: [f32; 2]) -> [f32; 2] {
    match tree {
        LayoutNode::Leaf(_) => leaf,
        LayoutNode::Split {
            dir, left, right, ..
        } => {
            let (a, b) = (least(left, leaf), least(right, leaf));
            if *dir == SplitDir::Horizontal {
                [a[0] + b[0] + SPACE.card_gap, a[1].max(b[1])]
            } else {
                [a[0].max(b[0]), a[1] + b[1] + SPACE.card_gap]
            }
        }
    }
}

/// Each pane's floor in points: [`MIN_CELLS`] and its card's padding,
/// shrunk on an axis where the whole tree couldn't keep it, so the split
/// gives way before it ever overflows its window.
pub(super) fn pane_floor(desk: &Desk, tree: &LayoutNode) -> [f32; 2] {
    let Some(area) = desk.split_area() else {
        return [0.0; 2];
    };
    let metrics = desk.metrics();
    let leaf = [
        MIN_CELLS[0] * metrics.cell_w + 2.0 * PAD,
        MIN_CELLS[1] * metrics.cell_h + 2.0 * PAD,
    ];
    let (gutters, full) = (least(tree, [0.0; 2]), least(tree, leaf));
    let fit = |axis: usize| {
        let room = area[axis] - gutters[axis];
        let need = full[axis] - gutters[axis];
        if need > 0.0 {
            (room / need).clamp(0.0, 1.0)
        } else {
            1.0
        }
    };
    [leaf[0] * fit(0), leaf[1] * fit(1)]
}

fn node(
    desk: &Desk,
    tree: &LayoutNode,
    path: &mut SplitPath,
    several: bool,
    floor: [f32; 2],
    ink: &Tokens,
) -> El {
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
    let first = node(desk, left, path, several, floor, ink);
    path.pop();
    path.push(true);
    let second = node(desk, right, path, several, floor, ink);
    path.pop();
    // Each side keeps its share unless that would squeeze a pane in it under
    // the floor; then the other side gives up the difference.
    let share = |el: El, grow: f32, side: &LayoutNode| {
        let [w, h] = least(side, floor);
        el.grow(grow.max(0.01)).min_w(w).min_h(h)
    };
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
        .child(share(el().w_full().h_full().child(first), ratio, left))
        .child(divider(here, horizontal, ratio, ink))
        .child(share(
            el().w_full().h_full().child(second),
            1.0 - ratio,
            right,
        ))
}

/// A divider: the clear gutter between two cards, with a grip in
/// `border.strong` that turns to the machine's colour under the pointer.
/// Drag it to move it, live, and the new ratio is written on release; or
/// focus it and step it with the arrows along its axis.
fn divider(path: SplitPath, horizontal: bool, ratio: f32, t: &Tokens) -> El {
    let s = &t.space;
    let (across, along) = (s.s1, s.s10);
    let grip = || {
        let grip = if horizontal {
            el().size(across, along)
        } else {
            el().size(along, across)
        };
        grip.radius(t.radius.full).inert()
    };
    let host = if horizontal {
        el().w(SPACE.card_gap).h_full()
    } else {
        el().w_full().h(SPACE.card_gap)
    };
    let stepped = path.clone();
    host.key(format!("divider-{path:?}"))
        .center()
        .role("separator")
        .label(if horizontal {
            "Resize the panes side by side"
        } else {
            "Resize the panes above and below"
        })
        .range_value(f64::from(ratio), 0.0, 1.0)
        .focusable(true)
        .cursor(if horizontal {
            rune::Cursor::ResizeColumn
        } else {
            rune::Cursor::ResizeRow
        })
        .hover_group()
        .on_key(move |d: &mut Desk, event, _| {
            let Some(step) = step_of(&event.key, horizontal) else {
                return false;
            };
            d.drag_divider(stepped.clone(), ratio + step, true);
            true
        })
        .on_drag(move |d: &mut Desk, drag, _| {
            if drag.phase == Phase::Down {
                // The ratio at the press, kept for the whole gesture.
                drag.data = ratio;
            }
            if drag.cancelled() {
                return d.cancel_divider();
            }
            let span = d.split_span(&path, horizontal).max(1.0);
            let travel = if horizontal { drag.dx } else { drag.dy };
            d.drag_divider(
                path.clone(),
                drag.data + travel / span,
                drag.phase == Phase::Up,
            );
        })
        .child(grip().fill(t.roles.border.strong.paint()))
        .child(
            el().cover()
                .center()
                .inert()
                .child(super::reveal(grip().fill(t.host), true)),
        )
}

/// How far one arrow press moves a divider, as a share of its split, and
/// which way: Left and Up shrink the first side, Right and Down grow it.
fn step_of(key: &Key, horizontal: bool) -> Option<f32> {
    let step = 0.05;
    match (key, horizontal) {
        (Key::Left, true) | (Key::Up, false) => Some(-step),
        (Key::Right, true) | (Key::Down, false) => Some(step),
        _ => None,
    }
}

/// How a card is lit: the keyboard's, among several, stands one tier
/// higher with a firmer rim. Focus adds no colour of its own, and the
/// cards without the keyboard stay at full strength, as legible as it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Lit {
    Resting,
    Focused,
    /// The window's one surface ([`Desk::flush`]): no rim and no lift, the
    /// card's ground continuous with the window's.
    Flush,
}

impl Lit {
    /// How the pane with or without the keyboard is lit in `desk`.
    fn of(desk: &Desk, several: bool, focused: bool) -> Self {
        if desk.flush() {
            Self::Flush
        } else if several && focused {
            Self::Focused
        } else {
            Self::Resting
        }
    }
}

/// A floating card around `content`: `radius.lg`, on the raised
/// tier at rest and lifted to floating when it has the keyboard, edged
/// with a top-lit hairline rim that is firmer on the lifted card.
/// `content` is clipped to the corners; the rim draws over it and never
/// takes the pointer.
pub(super) fn card(content: El, ground: Color, lit: Lit, ink: &Tokens) -> El {
    let t = ink;
    let r = t.radius.lg;
    let b = &t.roles.border;
    let (top, bottom, tier) = match lit {
        Lit::Resting => (b.base.paint(), b.muted.paint(), Tier::Raised),
        Lit::Focused => (b.strong.paint(), b.base.paint(), Tier::Floating),
        Lit::Flush => (Color::TRANSPARENT, Color::TRANSPARENT, Tier::Flat),
    };
    let lifted = el()
        .w_full()
        .h_full()
        .radius(r)
        .continuous_corners()
        .fill(ground);
    t.elevate(lifted, tier)
        .child(content.radius(r).continuous_corners().clip_rounded(r))
        .child(
            el().cover()
                .radius(r)
                .continuous_corners()
                .border_gradient(t.space.hairline, top, bottom)
                .inert(),
        )
}

/// A pane's card, keyed by its terminal so it keeps its place as the tree
/// changes: it enters from the enter scale on the list spring and fades
/// out.
fn float(id: &ResourceId, card: El, t: &Tokens) -> El {
    card.key(format!("pane-{id}"))
        .transition(Transition::asymmetric(
            Transition::scale(t.motion.enter_scale).springs(t.motion.list),
            Transition::opacity(),
        ))
}

pub(super) fn pane(desk: &Desk, id: &ResourceId, several: bool, ink: &Tokens) -> El {
    let focused = desk
        .connection
        .shown
        .as_ref()
        .is_some_and(|s| &s.resource == id);
    if desk.is_document(id) {
        let lit = Lit::of(desk, several, focused);
        let t = ink;
        let ground = t.roles.layer.ground.paint();
        return float(
            id,
            card(super::documents::view(desk, id, ink), ground, lit, t),
            t,
        );
    }
    terminal_pane(desk, id, several, ink)
}

fn terminal_pane(desk: &Desk, id: &ResourceId, several: bool, ink: &Tokens) -> El {
    let t = ink;
    let focused = desk.keyboard().is_some_and(|(has, _)| has == id);
    let view = desk.view_of(id);
    perf::RENDERS.incr();
    let acquiring = Instant::now();
    let frame = desk
        .connection
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
        let ground = t.roles.layer.ground.paint();
        let opening = host.fill(ground).center().child(
            rune::text("Opening…")
                .font(t.ty.caption)
                .color(t.roles.text.dim.paint()),
        );
        return float(
            id,
            framed(desk, opening, ground, Lit::of(desk, several, false), t),
            t,
        );
    };
    if focused {
        // The exact frame this build paints, for the drawn receipt.
        desk.painting(view, &frame);
    }
    let area = desk.pane_area(id).unwrap_or([0.0, 0.0]);
    let question = focused
        .then(|| super::inbox::ask_card(desk, id, area, ink))
        .flatten();
    let editing = question.as_ref().is_some_and(|card| card.editing);
    let grid_layer = terminal_layer(desk, id, &frame, focused, editing, ink);
    let ground = grid::background(&frame);
    let lit = Lit::of(desk, several, focused);
    let joining = id
        .local_id()
        .is_some_and(|n| desk.connection.reorder.target() == Some(Grab::Pane(n)));
    let host = terminal_input(host.fill(ground), desk, id)
        .child(grid_layer)
        .children(overlay::history(desk, id, &frame, area, ink))
        .children(overlay::find_bar(desk, id, area, ink))
        .children(question.map(|card| card.content))
        .children(tab_badge(desk, id, t))
        // Outlined while a dragged terminal would join its slot as a tab.
        .children(joining.then(|| {
            el().cover()
                .border(t.space.hairline, t.roles.border.focus.paint())
                .inert()
        }));
    float(id, framed(desk, host, ground, lit, t), t)
}

/// `2/3` in the corner of a pane among a split's tabs.
fn tab_badge(desk: &Desk, id: &ResourceId, t: &Tokens) -> Option<El> {
    let slot = (desk.connection.slots.iter()).find(|slot| slot.strip.contains(id))?;
    let at = slot.strip.iter().position(|tab| tab == id)? + 1;
    let label = format!("{at}/{}", slot.strip.len());
    Some(
        rune::text(label)
            .key(format!("slot-tab-{id}"))
            .font(t.ty.caption)
            .color(t.roles.text.dim.paint())
            .pin(None, Some(t.space.s1), Some(t.space.s2), None)
            .inert(),
    )
}

/// A pane as a card, or bare on a phone, where the terminal runs edge to edge
/// under the controls.
fn framed(desk: &Desk, content: El, ground: Color, lit: Lit, t: &Tokens) -> El {
    if desk.phone.is_some() {
        return content.fill(ground);
    }
    card(content, ground, lit, t)
}

fn terminal_layer(
    desk: &Desk,
    id: &ResourceId,
    frame: &phux_client_runtime::publication::GridFrame,
    focused: bool,
    editing: bool,
    ink: &Tokens,
) -> El {
    let t = ink;
    let metrics = desk.metrics();
    let keyboard = focused && desk.focused;
    let mut layer = el()
        .child(grid::paint(
            frame,
            keyboard,
            desk.blink_on,
            &metrics,
            ink.selection(),
        ))
        .children(overlay::link_underline(desk, id, &metrics, ink));
    if keyboard && desk.modal().is_none() && !editing {
        layer = layer.child(grid::ime_layer(
            frame,
            desk.connection.preedit.as_deref(),
            &metrics,
            (t.roles.text.text.paint(), t.roles.layer.raised.paint()),
            desk.settings.option_as_meta,
        ));
    }
    layer
}

/// Only terminal cards install terminal mouse, drop, and history handlers.
fn terminal_input(host: El, desk: &Desk, id: &ResourceId) -> El {
    if desk.phone.is_some()
        && !desk
            .connection
            .shown
            .as_ref()
            .is_some_and(|s| &s.resource == id)
    {
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
    // The terminal owns the entire gesture, including stationary word holds.
    // Rune must neither add touch slop nor turn its lift into a context menu.
    .touch_drags_at_once()
    .on_drag(move |d: &mut Desk, drag, tx| {
        // Drag points include the padding; the grid starts inside it.
        let (x, y, mods) = (drag.x - PAD, drag.y - PAD, tx.mods());
        // A phone's finger scrolls, holds to select and taps (`phone/touch.rs`).
        if d.phone.is_some() && tx.button() == PointerButton::Primary {
            let step = Press {
                phase: drag.phase,
                x,
                y,
                clicks: drag.clicks,
                mods,
                button: PointerButton::Primary,
            };
            let travel = [drag.dx, drag.dy];
            return d.phone_touch(&press, &step, travel, drag.cancelled(), tx);
        }
        // Only the primary button opens links; the rest are the program's.
        let cmd = mods.cmd && tx.button() == PointerButton::Primary;
        // A phone opens links by its finger's tap alone: any other button
        // there is the program's, press and release both.
        if d.phone.is_some() || !d.press_link(&press, drag.phase, x, y, cmd, tx) {
            let step = Press {
                phase: drag.phase,
                x,
                y,
                clicks: drag.clicks,
                mods,
                button: tx.button(),
            };
            // A cancelled gesture still releases the program's mouse, but
            // copies nothing.
            d.pointer(&press, &step, tx);
            if drag.phase == Phase::Up && !drag.cancelled() {
                d.selection_done(tx);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(n: u32) -> LayoutNode {
        LayoutNode::Leaf(ResourceId::local(n))
    }

    fn split(dir: SplitDir, left: LayoutNode, right: LayoutNode) -> LayoutNode {
        LayoutNode::Split {
            dir,
            ratio: 0.5,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    #[test]
    fn a_split_needs_both_sides_and_its_gutter() {
        // [1 | [2 / 3]]: two panes across, two stacked on the right.
        let tree = split(
            SplitDir::Horizontal,
            leaf(1),
            split(SplitDir::Vertical, leaf(2), leaf(3)),
        );
        assert_eq!(
            least(&tree, [100.0, 40.0]),
            [200.0 + SPACE.card_gap, 80.0 + SPACE.card_gap]
        );
        assert_eq!(
            least(&tree, [0.0; 2]),
            [SPACE.card_gap, SPACE.card_gap],
            "gutters alone"
        );
    }

    #[test]
    fn a_divider_steps_along_its_own_axis() {
        assert!(step_of(&Key::Right, true).is_some_and(|s| s > 0.0));
        assert!(step_of(&Key::Left, true).is_some_and(|s| s < 0.0));
        assert!(step_of(&Key::Down, false).is_some_and(|s| s > 0.0));
        assert!(step_of(&Key::Up, false).is_some_and(|s| s < 0.0));
        // Across its axis an arrow is the window's, not the divider's.
        assert_eq!(step_of(&Key::Up, true), None);
        assert_eq!(step_of(&Key::Right, false), None);
        assert_eq!(step_of(&Key::Enter, true), None);
    }
}
