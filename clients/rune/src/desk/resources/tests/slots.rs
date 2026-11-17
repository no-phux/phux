//! Tabs inside one split (slot tabs): the window's own keys show the next
//! tab, its hidden tabs keep their views, and nothing is written.
use super::*;
use phux_client_core::layout::Slot;

/// Pane 1 beside a slot of 2 (its default) and 3.
fn slotted() -> ArrangementDoc {
    let mut doc = doc(split(1, 2));
    doc.slots.push(Slot {
        strip: vec![id(2), id(3)],
        default: 0,
        shown: 0,
    });
    doc
}

fn terminals() -> Vec<ResourceRecord> {
    (1..=3).map(|n| record(n, ResourceKind::Terminal)).collect()
}

fn on_screen(rt: &Runtime<Desk>) -> Vec<ResourceId> {
    rt.state
        .connection
        .layout
        .as_ref()
        .map(leaves)
        .unwrap_or_default()
}

#[test]
fn the_next_tab_shows_in_its_split_and_the_hidden_one_stays_warm() {
    let (client, mut rt) = fixture(&terminals(), &slotted());
    assert_eq!(on_screen(&rt), [id(1), id(2)]);
    assert!(
        rt.state.connection.others.contains_key(&id(3)),
        "the hidden tab has a view"
    );
    rt.update(|d, _| d.focus_pane(&id(2)));
    step(&mut rt);
    rt.update(|d, _| d.cycle_slot_tab(1));
    step(&mut rt);
    assert_eq!(on_screen(&rt), [id(1), id(3)]);
    assert_eq!(rt.state.connection.shown.as_ref().unwrap().resource, id(3));
    assert!(rt.state.connection.others.contains_key(&id(2)));
    assert!(
        rt.elements()
            .iter()
            .any(|e| e.key.as_deref() == Some("slot-tab-@3")),
        "the tab's place is drawn"
    );
    let stored = client
        .organization()
        .arrangement_document(&session())
        .unwrap()
        .2;
    assert_eq!(stored.slots[0].default, 0, "which tab shows is not written");

    // The window's choice outlives the keyboard moving away.
    rt.update(|d, _| d.focus_pane(&id(1)));
    step(&mut rt);
    assert_eq!(on_screen(&rt), [id(1), id(3)]);
}

#[cfg(target_os = "macos")]
#[test]
fn command_shift_arrows_walk_a_splits_tabs() {
    use crate::keys::{Shortcut, shortcut};
    let mods = Mods {
        cmd: true,
        shift: true,
        ..Mods::default()
    };
    assert_eq!(
        shortcut(&Key::Right, mods, None),
        Some(Shortcut::NextSlotTab)
    );
    assert_eq!(
        shortcut(&Key::Left, mods, None),
        Some(Shortcut::PreviousSlotTab)
    );
}
