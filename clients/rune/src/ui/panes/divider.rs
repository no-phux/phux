//! The gutter between two split cards: a grip to drag or step with arrows.
use phux_design::tokens::SPACE;
use rune::{El, Key, Phase, el};

use super::{Paint, Tokens};
use crate::desk::{Desk, SplitPath};
use crate::ui::Cover;

/// A divider: the clear gutter between two cards, with a grip in
/// `border.strong` that turns to the machine's colour under the pointer.
/// Drag it to move it, live, and the new ratio is written on release; or
/// focus it and step it with the arrows along its axis.
pub(super) fn divider(path: SplitPath, horizontal: bool, ratio: f32, t: &Tokens) -> El {
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
                .child(crate::ui::reveal(grip().fill(t.host), true)),
        )
}

/// How far one arrow press moves a divider, as a share of its split, and
/// which way: Left and Up shrink the first side, Right and Down grow it.
pub(super) fn step_of(key: &Key, horizontal: bool) -> Option<f32> {
    let step = 0.05;
    match (key, horizontal) {
        (Key::Left, true) | (Key::Up, false) => Some(-step),
        (Key::Right, true) | (Key::Down, false) => Some(step),
        _ => None,
    }
}
