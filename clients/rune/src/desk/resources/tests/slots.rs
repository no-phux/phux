//! Tabs inside one split (slot tabs): the window's own keys show the next
//! tab, its hidden tabs keep their views, and nothing is written. A terminal
//! dropped on a pane joins its slot, and that is written.
use super::*;
use crate::desk::Grab;
use phux_client_core::layout::Slot;
use phux_protocol::wire::frame::ArrangementNode;
use rune::Phase;

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

#[test]
fn a_terminal_dropped_on_a_pane_joins_its_slot() {
    let (client, mut rt) = fixture(&terminals(), &doc(split(1, 2)));
    outbound(&client);
    let [x, y] = rt.state.connection.origins[&id(2)];
    let over = [x + 10.0, y + 10.0];
    let onto_itself = Grab::Resource(2, session());
    assert_eq!(rt.state.drop_target(onto_itself, over), None);

    let grab = Grab::Resource(3, session());
    rt.update(|d, _| d.drag_at(grab, Phase::Move, over, false));
    assert_eq!(rt.state.connection.reorder.target(), Some(Grab::Pane(2)));
    rt.update(|d, _| d.drag_at(grab, Phase::Up, over, false));
    let (document, slot_tabs) = outbound(&client)
        .into_iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                command:
                    Command::ArrangementReplace {
                        document,
                        slot_tabs,
                        ..
                    },
                ..
            } => Some((document, slot_tabs)),
            _ => None,
        })
        .expect("the slot is written");
    assert!(slot_tabs, "a server that keeps tabs takes them as written");
    let ArrangementNode::Split { second, .. } = &document.groups[0].tree else {
        panic!("still one split: {document:?}");
    };
    let ArrangementNode::Leaf(slot) = second.as_ref() else {
        panic!("pane 2 is still a leaf");
    };
    assert_eq!(slot.tabs.len(), 1, "terminal 3 is its tab");
    assert_eq!(slot.shown_at, 0, "pane 2 stays the default");
}
