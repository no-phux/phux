//! Dragging a session to a new place in the stored order: its header, or
//! the lone row that stands for it.

use phux_protocol::ids::SessionKey;
use rune::El;

use super::super::Ink;
use crate::desk::{Desk, Grab};

/// `handle` dragging the session `key` names, outlined while a drop would
/// land on it. A handle with no key (a satellite's) stays put.
pub(super) fn reorderable(handle: El, key: Option<SessionKey>, target: bool, ink: &Ink) -> El {
    let Some(key) = key else { return handle };
    let grab = Grab::Session(key);
    handle
        .on_drag(move |d: &mut Desk, drag, _| d.drag_reorder(grab, drag))
        .on_layout_tx(move |d: &mut Desk, rect, tx| {
            d.reorder.laid_out(grab, rect);
            tx.live_only();
        })
        .border(if target { 1.0 } else { 0.0 }, ink.accent)
}
