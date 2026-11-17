//! The slot tab keys: `next-slot-tab` and `previous-slot-tab` switch what
//! this client shows of the focused slot and write nothing.

use crate::layout::{ArrangementDoc, Slot, leaves};

use super::args::bare_action;
use super::test_support::*;

/// Pane 1 beside a slot showing 2, with tabs 3 and 4; focus on the slot.
fn slotted() -> ArrangementDoc {
    let mut doc = two_pane_workspace();
    doc.slots.push(Slot {
        strip: vec![tid(2), tid(3), tid(4)],
        default: 0,
        shown: 0,
    });
    doc.groups[0].state.focus = Some(tid(2));
    doc
}

fn shown(doc: &ArrangementDoc) -> Vec<phux_protocol::ResourceId> {
    leaves(doc.groups[0].state.tree.as_ref().expect("a tree"))
}

#[test]
fn the_keys_switch_the_focused_slots_tab_and_write_nothing() {
    let mut f = fx(slotted());
    let effects = f.run(&bare_action("next-slot-tab"));
    assert!(
        effects.layout_mutated && !effects.set_metadata,
        "client-local"
    );
    assert_eq!(effects.set_focus, Some(tid(3)));
    assert_eq!(shown(&f.workspace), [tid(1), tid(3)]);
    let effects = f.run(&bare_action("previous-slot-tab"));
    assert_eq!(effects.set_focus, Some(tid(2)));
    assert_eq!(shown(&f.workspace), [tid(1), tid(2)]);
}

#[test]
fn a_pane_outside_a_slot_bells() {
    let mut f = fx(two_pane_workspace());
    let effects = f.run(&bare_action("next-slot-tab"));
    assert!(effects.bell && !effects.layout_mutated);
}
