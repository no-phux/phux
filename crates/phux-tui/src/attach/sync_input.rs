//! Synchronized input: what you type goes to every pane of one tab.
//!
//! On for a single tab at a time, named by its group id, so it stops by
//! itself when you move to another tab and resumes when you come back.

use std::sync::Mutex;

use phux_protocol::ids::ResourceId;

use crate::layout::ArrangementDoc;

// ponytail: one flag for the process, because the key path has no place of
// its own to keep it; move it into the attach state if a second client ever
// shares a process.
static TAB: Mutex<Option<[u8; 16]>> = Mutex::new(None);

/// Turn it on for the tab in view, or off if it is on there. Returns whether
/// it is on now.
pub(in crate::attach) fn toggle(workspace: &ArrangementDoc) -> bool {
    let current = workspace.groups.get(workspace.active).map(|group| group.id);
    let Ok(mut tab) = TAB.lock() else {
        return false;
    };
    *tab = if *tab == current { None } else { current };
    tab.is_some()
}

/// The other panes that get what `focused` gets: the rest of the tab in
/// view, when synchronized input is on for it.
pub(in crate::attach) fn others(
    workspace: &ArrangementDoc,
    focused: &ResourceId,
) -> Vec<ResourceId> {
    let on = TAB.lock().ok().and_then(|tab| *tab);
    others_when(workspace, focused, on)
}

fn others_when(
    workspace: &ArrangementDoc,
    focused: &ResourceId,
    on: Option<[u8; 16]>,
) -> Vec<ResourceId> {
    let Some(group) = workspace.groups.get(workspace.active) else {
        return Vec::new();
    };
    if on != Some(group.id) {
        return Vec::new();
    }
    let Some(tree) = group.state.tree.as_ref() else {
        return Vec::new();
    };
    let mut panes = crate::layout::leaves(tree);
    panes.retain(|pane| pane != focused);
    panes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_tab_it_is_on_for_shares_input() {
        let pane = ResourceId::local;
        let mut doc = ArrangementDoc::single(pane(1));
        doc.add_group("b".to_owned(), pane(2));
        doc.select(0);
        let first = doc.groups[0].id;

        assert!(others_when(&doc, &pane(1), None).is_empty(), "off");
        // One pane in the tab: nobody else to type into.
        assert!(others_when(&doc, &pane(1), Some(first)).is_empty());
        doc.select(1);
        assert!(
            others_when(&doc, &pane(2), Some(first)).is_empty(),
            "another tab"
        );
    }
}
