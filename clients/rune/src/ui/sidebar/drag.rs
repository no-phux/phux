//! Session ordering and local resource membership drags share hit-tested
//! session headers and resource rows. Satellites never enter this lane.

use phux_protocol::ids::{ResourceId, SessionKey};
use rune::El;

use super::super::{Paint, Tokens};
use crate::desk::{Desk, Grab};

/// Bound sidebar drops to the list's actual scroll viewport, not its footer.
pub(super) fn region(handle: El) -> El {
    handle.on_layout_tx(|d: &mut Desk, rect, tx| {
        d.connection.reorder.sidebar_laid_out(rect);
        tx.live_only();
    })
}

/// A local resource row drags its resource, not the session it belongs to.
pub(super) fn resource(handle: El, id: &ResourceId, session: SessionKey) -> El {
    let Some(id) = id.local_id() else {
        return handle;
    };
    let grab = Grab::Resource(id, session);
    handle
        .help("Open resource · drag to another session, or onto a pane as a tab")
        .on_drag(move |d: &mut Desk, drag, _| d.drag_reorder(grab, drag))
        .on_layout_tx(move |d: &mut Desk, rect, tx| {
            d.connection.reorder.laid_out(grab, rect);
            tx.live_only();
        })
}

/// `handle` dragging the session `key` names, outlined in the focus ring's
/// colour while a drop would land on it. A handle with no key
/// (a satellite's) stays put.
pub(super) fn reorderable(handle: El, key: Option<SessionKey>, target: bool, t: &Tokens) -> El {
    let Some(key) = key else { return handle };
    let grab = Grab::Session(key);
    handle
        .on_drag(move |d: &mut Desk, drag, _| d.drag_reorder(grab, drag))
        .on_layout_tx(move |d: &mut Desk, rect, tx| {
            d.connection.reorder.laid_out(grab, rect);
            tx.live_only();
        })
        .border(
            t.space.hairline,
            if target {
                t.roles.border.focus.paint()
            } else {
                rune::Color::TRANSPARENT
            },
        )
}
