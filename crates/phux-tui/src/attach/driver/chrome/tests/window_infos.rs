//! `window_infos`: the per-window flags and labels the tab strip shows.

use super::*;

/// `window_infos` marks a window when ANY of its leaves has
/// the asked flag — including a non-focused leaf — and only that window.
#[test]
fn window_infos_flags_attention_on_the_asking_window() {
    let front = ResourceId::local(1);
    let back = ResourceId::local(2);
    let mut workspace = ArrangementDoc::single(front.clone());
    workspace.add_group("2".to_owned(), back.clone());
    workspace.select(0);
    let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
    panes.insert(front, PaneSlot::new_with_size(80, 24).expect("slot"));
    let mut asking = PaneSlot::new_with_size(80, 24).expect("slot");
    asking.attention = true;
    panes.insert(back.clone(), asking);

    let infos = window_infos(
        &workspace,
        &panes,
        None,
        &HashMap::new(),
        &mut VcsIndex::default(),
    );
    assert!(
        !infos[0].attention,
        "quiet window must not carry the marker"
    );
    assert!(
        infos[1].attention,
        "the asking (background) window carries the marker"
    );

    // Clearing the flag clears the marker.
    assert!(clear_attention_on_input(&mut panes, &back));
    let infos = window_infos(
        &workspace,
        &panes,
        None,
        &HashMap::new(),
        &mut VcsIndex::default(),
    );
    assert!(!infos[1].attention);
}

/// `window_infos` marks a window when ANY of its leaves is
/// retained after exit — including a non-focused leaf — and only that window.
#[test]
fn window_infos_flags_exited_on_the_retained_window() {
    let front = ResourceId::local(1);
    let back = ResourceId::local(2);
    let mut workspace = ArrangementDoc::single(front.clone());
    workspace.add_group("2".to_owned(), back.clone());
    workspace.select(0);
    let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
    panes.insert(front, PaneSlot::new_with_size(80, 24).expect("slot"));
    let mut retained = PaneSlot::new_with_size(80, 24).expect("slot");
    retained.exited = Some(ExitMark {
        status: Some(3),
        signal: None,
    });
    panes.insert(back, retained);

    let infos = window_infos(
        &workspace,
        &panes,
        None,
        &HashMap::new(),
        &mut VcsIndex::default(),
    );
    assert_eq!(
        infos[0].exited.as_deref(),
        None,
        "live window stays unmarked"
    );
    assert_eq!(
        infos[1].exited.as_deref(),
        Some("3"),
        "the retained (background) window carries the compact status"
    );
}

/// A split whose unfocused leaf is retained still marks
/// the window, so the tab/sidebar show it without focusing that pane.
#[test]
fn window_infos_marks_a_split_when_the_unfocused_leaf_exited() {
    use crate::layout::{LayoutNode, LayoutState, SplitDir};
    let live = ResourceId::local(1);
    let dead = ResourceId::local(2);
    let mut workspace = ArrangementDoc::single(live.clone());
    workspace.groups[0].state = LayoutState {
        tree: Some(LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(live.clone())),
            right: Box::new(LayoutNode::Leaf(dead.clone())),
        }),
        focus: Some(live.clone()),
    };
    let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
    panes.insert(live, PaneSlot::new_with_size(80, 24).expect("slot"));
    let mut retained = PaneSlot::new_with_size(80, 24).expect("slot");
    retained.exited = Some(ExitMark {
        status: None,
        signal: Some(9),
    });
    panes.insert(dead, retained);

    let infos = window_infos(
        &workspace,
        &panes,
        None,
        &HashMap::new(),
        &mut VcsIndex::default(),
    );
    assert_eq!(
        infos[0].exited.as_deref(),
        Some("sig9"),
        "unfocused retained leaf still marks the window"
    );
}

/// Tab labels: the focused leaf's OSC title beats the stored name, a
/// declared agent record beats the title, and no (or a blank) title falls
/// back to the stored name.
#[test]
fn window_infos_label_precedence() {
    let id = ResourceId::local(1);
    let workspace = ArrangementDoc::single(id.clone());
    let reviewer = AgentRecord {
        name: "reviewer".to_owned(),
        state: phux_client::agent_meta::AgentMetaState::Blocked,
        ..AgentRecord::default()
    };
    for (title, record, label) in [
        (&b"\x1b]2;~/src/phux\x07"[..], None, "~/src/phux"),
        (b"", None, "1"),
        (b"\x1b]2;   \x07", None, "1"),
        (b"\x1b]2;~/src/phux\x07", Some(reviewer), "reviewer"),
        (b"\x1b]2;claude task\x07", None, "claude task"),
    ] {
        let (_, _, panes) = published_test_state(&[(&id, 80, 24, title)]);
        let records: HashMap<ResourceId, AgentRecord> =
            record.into_iter().map(|r| (id.clone(), r)).collect();
        let infos = window_infos(&workspace, &panes, None, &records, &mut VcsIndex::default());
        assert_eq!(infos[0].name, label, "{title:?}");
        assert!(infos[0].active);
    }
}
