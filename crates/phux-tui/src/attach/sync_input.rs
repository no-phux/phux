//! Synchronized input: what you type goes to every pane of one tab.
//!
//! On for a single tab at a time, named by its group id, so it stops by
//! itself when you move to another tab and resumes when you come back.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use phux_protocol::ids::ResourceId;

use crate::layout::ArrangementDoc;
use crate::render::chrome::dividers::OriginBadge;

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

/// Whether the tab in view has it on, as of the last [`settle`]; what the
/// pane title rules read to draw their standing mark.
static SHOWN: AtomicBool = AtomicBool::new(false);

/// Note whether the tab in view has it on. Returns whether that changed, in
/// which case the pane title rules need a repaint.
pub(in crate::attach) fn settle(workspace: &ArrangementDoc) -> bool {
    let current = workspace.groups.get(workspace.active).map(|group| group.id);
    let on = current.is_some() && TAB.lock().is_ok_and(|tab| *tab == current);
    SHOWN.swap(on, Ordering::Relaxed) != on
}

/// The standing `[sync]` mark for a pane title rule while the tab in view
/// shares its input.
pub(in crate::attach) fn badge<'a>() -> Option<OriginBadge<'a>> {
    SHOWN.load(Ordering::Relaxed).then_some(OriginBadge {
        text: "sync",
        dim: false,
    })
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

    /// The standing mark shows only while the tab in view shares input.
    #[test]
    fn the_mark_follows_the_tab_in_view() {
        let mut doc = ArrangementDoc::single(ResourceId::local(1));
        doc.add_group("b".to_owned(), ResourceId::local(2));
        doc.select(0);
        assert!(!settle(&doc) && badge().is_none());
        assert!(toggle(&doc) && settle(&doc) && badge().is_some());
        doc.select(1);
        assert!(settle(&doc) && badge().is_none(), "another tab in view");
        doc.select(0);
        assert!(settle(&doc) && badge().is_some(), "and back");
        assert!(!toggle(&doc) && settle(&doc) && badge().is_none());
    }

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
