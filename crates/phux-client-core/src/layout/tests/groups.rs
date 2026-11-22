//! Group add, select, move, prune, and pane close on an arrangement.

use super::*;

fn ws3() -> ArrangementDoc {
    let mut ws = ArrangementDoc::single(t(1));
    ws.add_group("2".to_owned(), t(2));
    ws.add_group("3".to_owned(), t(3));
    ws
}

#[test]
fn add_group_appends_and_activates() {
    let mut ws = ArrangementDoc::single(t(1));
    ws.add_group(ws.default_group_name(), t(2));
    assert_eq!(ws.groups.len(), 2);
    assert_eq!(ws.active, 1);
    assert_eq!(ws.groups[1].name, "2");
    assert_eq!(ws.active_group().unwrap().focus, Some(t(2)));
}

#[test]
fn next_prev_wrap() {
    let mut ws = ws3();
    assert_eq!(ws.active, 2);
    ws.next();
    assert_eq!(ws.active, 0);
    ws.prev();
    assert_eq!(ws.active, 2);
    ws.prev();
    assert_eq!(ws.active, 1);
}

#[test]
fn select_out_of_bounds_is_noop() {
    let mut ws = ws3();
    assert!(ws.select(0));
    assert_eq!(ws.active, 0);
    assert!(!ws.select(9));
    assert_eq!(ws.active, 0);
}

fn names(ws: &ArrangementDoc) -> Vec<&str> {
    ws.groups.iter().map(|w| w.name.as_str()).collect()
}

#[test]
fn move_group_reorders_and_active_follows_its_window() {
    let mut ws = ws3(); // active = "3"
    assert!(ws.move_group(2, 0));
    assert_eq!(names(&ws), ["3", "1", "2"]);
    assert_eq!(ws.groups[ws.active].name, "3");

    assert!(ws.move_group(0, 2));
    assert_eq!(names(&ws), ["1", "2", "3"]);
    assert_eq!(ws.groups[ws.active].name, "3");
}

#[test]
fn move_group_keeps_an_uninvolved_active_window_selected() {
    for (from, to) in [(0, 2), (2, 0), (0, 1), (1, 2), (2, 1), (1, 0)] {
        for active in 0..3 {
            let mut ws = ws3();
            ws.select(active);
            let focused = ws.groups[active].name.clone();
            assert!(ws.move_group(from, to));
            assert_eq!(
                ws.groups[ws.active].name, focused,
                "move {from}->{to} with active {active}"
            );
        }
    }
}

#[test]
fn move_group_out_of_range_or_in_place_is_noop() {
    let mut ws = ws3();
    assert!(!ws.move_group(1, 1));
    assert!(!ws.move_group(3, 0));
    assert!(!ws.move_group(0, 3));
    assert_eq!(names(&ws), ["1", "2", "3"]);
    assert_eq!(ws.active, 2);
}

#[test]
fn default_group_name_skips_used_integers() {
    let mut ws = ArrangementDoc::single(t(1)); // "1"
    ws.add_group("build".to_owned(), t(2)); // non-integer, ignored
    assert_eq!(ws.default_group_name(), "2");
    ws.add_group("2".to_owned(), t(3));
    assert_eq!(ws.default_group_name(), "3");
}

#[test]
fn prune_empty_groups_removes_treeless_window_and_keeps_active() {
    let mut ws = ws3(); // active = 2
    // Empty the middle window's tree (its last pane closed).
    ws.groups[1].state.tree = None;
    ws.groups[1].state.focus = None;
    assert!(ws.prune_empty_groups());
    assert_eq!(ws.groups.len(), 2);
    // Active window ("3") survived; it shifted from index 2 to 1.
    assert_eq!(ws.groups[ws.active].name, "3");
}

#[test]
fn prune_empty_groups_when_active_dies_lands_on_survivor() {
    let mut ws = ws3();
    ws.select(1); // active = middle ("2")
    ws.groups[1].state.tree = None;
    assert!(ws.prune_empty_groups());
    assert_eq!(ws.groups.len(), 2);
    // "2" died; the survivor that took its slot is "3".
    assert_eq!(ws.groups[ws.active].name, "3");
}

#[test]
fn close_pane_refuses_the_last_pane_without_mutation() {
    let mut ws = ArrangementDoc::single(t(1));
    let original = ws.clone();
    assert!(matches!(ws.close_pane(&t(1)), Err(LayoutError::LastPane)));
    assert_eq!(ws, original);
}

#[test]
fn close_pane_refuses_a_foreign_target_without_mutation() {
    let mut ws = ws3();
    let original = ws.clone();
    assert!(matches!(
        ws.close_pane(&t(99)),
        Err(LayoutError::PaneNotInLayout(id)) if id == t(99)
    ));
    assert_eq!(ws, original);
}

#[test]
fn close_pane_repairs_focus_onto_the_first_surviving_leaf() {
    let mut ws = ws_split(1, 2, 2);
    ws.close_pane(&t(2)).unwrap();
    assert_eq!(ws.groups.len(), 1);
    assert_eq!(
        leaves(ws.groups[0].state.tree.as_ref().unwrap()),
        vec![t(1)]
    );
    assert_eq!(ws.groups[0].state.focus, Some(t(1)));
    assert_eq!(ws.active, 0);
}

#[test]
fn close_pane_prunes_an_emptied_window_and_keeps_a_survivor() {
    let mut ws = ws3();
    ws.select(0);
    ws.close_pane(&t(2)).unwrap();
    assert_eq!(ws.groups.len(), 2);
    assert_eq!(ws.groups[0].name, "1");
    assert_eq!(ws.groups[1].name, "3");
    assert_eq!(ws.groups[0].state.focus, Some(t(1)));
    assert_eq!(ws.groups[1].state.focus, Some(t(3)));
    assert_eq!(ws.active, 1);
    assert_eq!(
        leaves(ws.groups[1].state.tree.as_ref().unwrap()),
        vec![t(3)]
    );
}
